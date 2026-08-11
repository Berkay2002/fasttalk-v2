mod processing;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream, StreamConfig, SupportedStreamConfig};
pub use processing::{
    ASR_FRAME_SAMPLES, ASR_SAMPLE_RATE, CaptureProcessor, DEVICE_FRAME_SAMPLES, DEVICE_SAMPLE_RATE,
    ProcessedCaptureFrame,
};
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const RING_CAPACITY_SAMPLES: usize = DEVICE_SAMPLE_RATE as usize * 2;
const ASR_RING_CAPACITY_SAMPLES: usize = 16_000 * 2;

#[derive(Clone, Debug)]
pub struct AudioConfig {
    pub aec_stream_delay_ms: i32,
    pub input_device_id: Option<String>,
    pub output_device_id: Option<String>,
    pub vad_model_path: Option<PathBuf>,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            aec_stream_delay_ms: 40,
            input_device_id: None,
            output_device_id: None,
            vad_model_path: None,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioDeviceInfo {
    pub id: String,
    pub name: String,
    pub is_default: bool,
    pub is_compatible: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioDevices {
    pub inputs: Vec<AudioDeviceInfo>,
    pub outputs: Vec<AudioDeviceInfo>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioStatus {
    pub active: bool,
    pub muted: bool,
    pub input_device_id: String,
    pub input_device: String,
    pub output_device_id: String,
    pub output_device: String,
    pub sample_rate_hz: u32,
    pub speech_active: bool,
    pub interruption_active: bool,
    pub queued_playback_samples: usize,
    pub dropped_capture_samples: u64,
    pub dropped_playback_samples: u64,
    pub dropped_asr_samples: u64,
    pub last_cancel_to_callback_ms: Option<f64>,
    pub last_error: Option<String>,
}

#[derive(Debug)]
pub enum AudioError {
    Device(String),
    Stream(String),
    Processing(String),
    QueueFull { accepted: usize, requested: usize },
}

impl std::fmt::Display for AudioError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Device(message) | Self::Stream(message) | Self::Processing(message) => {
                formatter.write_str(message)
            }
            Self::QueueFull {
                accepted,
                requested,
            } => write!(
                formatter,
                "playback queue accepted {accepted} of {requested} samples"
            ),
        }
    }
}

impl std::error::Error for AudioError {}

struct SharedState {
    running: AtomicBool,
    muted: AtomicBool,
    speech_active: AtomicBool,
    interruption_active: AtomicBool,
    asr_session_active: AtomicBool,
    cancel_epoch: AtomicU64,
    cancel_requested_micros: AtomicU64,
    cancel_latency_micros: AtomicU64,
    dropped_capture: AtomicU64,
    dropped_playback: AtomicU64,
    dropped_asr: AtomicU64,
    last_error: Mutex<Option<String>>,
    clock_origin: Instant,
}

impl SharedState {
    fn new() -> Self {
        Self {
            running: AtomicBool::new(true),
            muted: AtomicBool::new(false),
            speech_active: AtomicBool::new(false),
            interruption_active: AtomicBool::new(false),
            asr_session_active: AtomicBool::new(false),
            cancel_epoch: AtomicU64::new(0),
            cancel_requested_micros: AtomicU64::new(0),
            cancel_latency_micros: AtomicU64::new(0),
            dropped_capture: AtomicU64::new(0),
            dropped_playback: AtomicU64::new(0),
            dropped_asr: AtomicU64::new(0),
            last_error: Mutex::new(None),
            clock_origin: Instant::now(),
        }
    }

    fn elapsed_micros(&self) -> u64 {
        self.clock_origin
            .elapsed()
            .as_micros()
            .min(u64::MAX as u128) as u64
    }

    fn record_error(&self, error: impl Into<String>) {
        if let Ok(mut last_error) = self.last_error.lock() {
            *last_error = Some(error.into());
        }
    }
}

pub struct AudioEngine {
    input_stream: Option<Stream>,
    output_stream: Option<Stream>,
    processor_thread: Option<JoinHandle<()>>,
    playback_producer: HeapProd<f32>,
    asr_consumer: HeapCons<f32>,
    shared: Arc<SharedState>,
    input_device: String,
    input_device_id: String,
    output_device: String,
    output_device_id: String,
}

impl AudioEngine {
    pub fn enumerate_devices() -> Result<AudioDevices, AudioError> {
        let host = cpal::default_host();
        let default_input = host
            .default_input_device()
            .and_then(|device| device.id().ok())
            .map(|id| id.to_string());
        let default_output = host
            .default_output_device()
            .and_then(|device| device.id().ok())
            .map(|id| id.to_string());
        let inputs = host
            .input_devices()
            .map_err(|error| AudioError::Device(format!("enumerate input devices: {error}")))?
            .filter_map(|device| device_info(device, default_input.as_deref(), true))
            .collect::<Vec<_>>();
        let outputs = host
            .output_devices()
            .map_err(|error| AudioError::Device(format!("enumerate output devices: {error}")))?
            .filter_map(|device| device_info(device, default_output.as_deref(), false))
            .collect::<Vec<_>>();
        Ok(AudioDevices {
            inputs: sorted_devices(inputs),
            outputs: sorted_devices(outputs),
        })
    }

    pub fn start(config: AudioConfig) -> Result<Self, AudioError> {
        let host = cpal::default_host();
        let input_device = resolve_device(&host, config.input_device_id.as_deref(), true)?;
        let output_device = resolve_device(&host, config.output_device_id.as_deref(), false)?;
        let input_device_id = input_device
            .id()
            .map_err(|error| AudioError::Device(format!("read input device id: {error}")))?
            .to_string();
        let output_device_id = output_device
            .id()
            .map_err(|error| AudioError::Device(format!("read output device id: {error}")))?
            .to_string();
        let input_name = input_device
            .description()
            .map_err(|error| AudioError::Device(error.to_string()))?
            .name()
            .to_owned();
        let output_name = output_device
            .description()
            .map_err(|error| AudioError::Device(error.to_string()))?
            .name()
            .to_owned();
        let input_config = select_f32_48khz_input(&input_device)?;
        let output_config = select_f32_48khz_output(&output_device)?;

        let (mut capture_producer, capture_consumer) =
            HeapRb::<f32>::new(RING_CAPACITY_SAMPLES).split();
        let (playback_producer, mut playback_consumer) =
            HeapRb::<f32>::new(RING_CAPACITY_SAMPLES).split();
        let (mut reference_producer, reference_consumer) =
            HeapRb::<f32>::new(RING_CAPACITY_SAMPLES).split();
        let (asr_producer, asr_consumer) = HeapRb::<f32>::new(ASR_RING_CAPACITY_SAMPLES).split();
        let shared = Arc::new(SharedState::new());

        let input_channels = input_config.channels() as usize;
        let input_shared = shared.clone();
        let input_stream_config: StreamConfig = input_config.config();
        let input_stream = input_device
            .build_input_stream(
                &input_stream_config,
                move |data: &[f32], _| {
                    for frame in data.chunks_exact(input_channels) {
                        let mono = if input_shared.muted.load(Ordering::Acquire) {
                            0.0
                        } else {
                            frame.iter().sum::<f32>() / input_channels as f32
                        };
                        if reference_safe_push(&mut capture_producer, mono).is_err() {
                            input_shared.dropped_capture.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                },
                {
                    let shared = shared.clone();
                    move |error| shared.record_error(format!("input stream: {error}"))
                },
                None,
            )
            .map_err(|error| AudioError::Stream(format!("build input stream: {error}")))?;

        let output_channels = output_config.channels() as usize;
        let output_shared = shared.clone();
        let output_stream_config: StreamConfig = output_config.config();
        let mut observed_cancel_epoch = 0;
        let output_stream = output_device
            .build_output_stream(
                &output_stream_config,
                move |data: &mut [f32], _| {
                    let cancel_epoch = output_shared.cancel_epoch.load(Ordering::Acquire);
                    if cancel_epoch != observed_cancel_epoch {
                        observed_cancel_epoch = cancel_epoch;
                        playback_consumer.clear();
                        let requested = output_shared
                            .cancel_requested_micros
                            .load(Ordering::Acquire);
                        let latency = output_shared.elapsed_micros().saturating_sub(requested);
                        output_shared
                            .cancel_latency_micros
                            .store(latency.max(1), Ordering::Release);
                    }
                    for frame in data.chunks_exact_mut(output_channels) {
                        let sample = playback_consumer.try_pop().unwrap_or(0.0);
                        for channel in frame {
                            *channel = sample;
                        }
                        if reference_producer.try_push(sample).is_err() {
                            output_shared
                                .dropped_playback
                                .fetch_add(1, Ordering::Relaxed);
                        }
                    }
                },
                {
                    let shared = shared.clone();
                    move |error| shared.record_error(format!("output stream: {error}"))
                },
                None,
            )
            .map_err(|error| AudioError::Stream(format!("build output stream: {error}")))?;

        let processor_shared = shared.clone();
        let vad_model_path = config.vad_model_path.clone();
        let processor_thread = std::thread::Builder::new()
            .name("fasttalk-audio-processing".to_owned())
            .spawn(move || {
                run_processor(
                    capture_consumer,
                    reference_consumer,
                    asr_producer,
                    processor_shared,
                    config.aec_stream_delay_ms,
                    vad_model_path,
                );
            })
            .map_err(|error| AudioError::Stream(format!("start audio processor: {error}")))?;

        input_stream
            .play()
            .map_err(|error| AudioError::Stream(format!("start input stream: {error}")))?;
        output_stream
            .play()
            .map_err(|error| AudioError::Stream(format!("start output stream: {error}")))?;

        Ok(Self {
            input_stream: Some(input_stream),
            output_stream: Some(output_stream),
            processor_thread: Some(processor_thread),
            playback_producer,
            asr_consumer,
            shared,
            input_device: input_name,
            input_device_id,
            output_device: output_name,
            output_device_id,
        })
    }

    pub fn queue_playback(&mut self, samples_48khz_mono: &[f32]) -> Result<(), AudioError> {
        let accepted = self.queue_playback_partial(samples_48khz_mono);
        if accepted == samples_48khz_mono.len() {
            Ok(())
        } else {
            self.shared.dropped_playback.fetch_add(
                (samples_48khz_mono.len() - accepted) as u64,
                Ordering::Relaxed,
            );
            Err(AudioError::QueueFull {
                accepted,
                requested: samples_48khz_mono.len(),
            })
        }
    }

    pub fn queue_playback_partial(&mut self, samples_48khz_mono: &[f32]) -> usize {
        self.playback_producer.push_slice(samples_48khz_mono)
    }

    pub fn cancel_playback(&self) {
        self.shared
            .cancel_latency_micros
            .store(0, Ordering::Release);
        self.shared
            .cancel_requested_micros
            .store(self.shared.elapsed_micros(), Ordering::Release);
        self.shared.cancel_epoch.fetch_add(1, Ordering::AcqRel);
    }

    pub fn read_asr_samples(&mut self, output: &mut [f32]) -> usize {
        self.asr_consumer.pop_slice(output)
    }

    pub fn begin_asr_session(&mut self) {
        self.asr_consumer.clear();
        self.shared.dropped_asr.store(0, Ordering::Relaxed);
        self.shared
            .asr_session_active
            .store(true, Ordering::Release);
    }

    pub fn end_asr_session(&mut self) {
        self.shared
            .asr_session_active
            .store(false, Ordering::Release);
        self.asr_consumer.clear();
        self.shared.speech_active.store(false, Ordering::Release);
        self.shared
            .interruption_active
            .store(false, Ordering::Release);
    }

    pub fn set_muted(&self, muted: bool) {
        self.shared.muted.store(muted, Ordering::Release);
    }

    #[must_use]
    pub fn status(&self) -> AudioStatus {
        let cancellation = self.shared.cancel_latency_micros.load(Ordering::Acquire);
        AudioStatus {
            active: self.shared.running.load(Ordering::Acquire),
            muted: self.shared.muted.load(Ordering::Acquire),
            input_device_id: self.input_device_id.clone(),
            input_device: self.input_device.clone(),
            output_device_id: self.output_device_id.clone(),
            output_device: self.output_device.clone(),
            sample_rate_hz: DEVICE_SAMPLE_RATE,
            speech_active: self.shared.speech_active.load(Ordering::Acquire),
            interruption_active: self.shared.interruption_active.load(Ordering::Acquire),
            queued_playback_samples: self.playback_producer.occupied_len(),
            dropped_capture_samples: self.shared.dropped_capture.load(Ordering::Relaxed),
            dropped_playback_samples: self.shared.dropped_playback.load(Ordering::Relaxed),
            dropped_asr_samples: self.shared.dropped_asr.load(Ordering::Relaxed),
            last_cancel_to_callback_ms: (cancellation != 0)
                .then_some(cancellation as f64 / 1_000.0),
            last_error: self
                .shared
                .last_error
                .lock()
                .ok()
                .and_then(|error| error.clone()),
        }
    }

    pub fn stop(&mut self) {
        if !self.shared.running.swap(false, Ordering::AcqRel) {
            return;
        }
        self.input_stream.take();
        self.output_stream.take();
        if let Some(thread) = self.processor_thread.take() {
            let _ = thread.join();
        }
    }
}

fn resolve_device(
    host: &cpal::Host,
    requested_id: Option<&str>,
    input: bool,
) -> Result<cpal::Device, AudioError> {
    if let Some(requested_id) = requested_id {
        let id = requested_id
            .parse::<cpal::DeviceId>()
            .map_err(|error| AudioError::Device(format!("invalid audio device id: {error}")))?;
        return host.device_by_id(&id).ok_or_else(|| {
            AudioError::Device(format!(
                "selected {} device is no longer available: {requested_id}",
                if input { "input" } else { "output" }
            ))
        });
    }
    if input {
        host.default_input_device()
            .ok_or_else(|| AudioError::Device("no default input device available".to_owned()))
    } else {
        host.default_output_device()
            .ok_or_else(|| AudioError::Device("no default output device available".to_owned()))
    }
}

fn device_info(
    device: cpal::Device,
    default_id: Option<&str>,
    input: bool,
) -> Option<AudioDeviceInfo> {
    let id = device.id().ok()?.to_string();
    let name = device
        .description()
        .map(|description| description.name().to_owned())
        .unwrap_or_else(|_| id.clone());
    let is_compatible = if input {
        select_f32_48khz_input(&device).is_ok()
    } else {
        select_f32_48khz_output(&device).is_ok()
    };
    Some(AudioDeviceInfo {
        is_default: default_id == Some(id.as_str()),
        id,
        name,
        is_compatible,
    })
}

fn sorted_devices(mut devices: Vec<AudioDeviceInfo>) -> Vec<AudioDeviceInfo> {
    devices.sort_by(|left, right| {
        right
            .is_default
            .cmp(&left.is_default)
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.id.cmp(&right.id))
    });
    devices
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run_processor(
    mut capture_consumer: HeapCons<f32>,
    mut reference_consumer: HeapCons<f32>,
    mut asr_producer: HeapProd<f32>,
    shared: Arc<SharedState>,
    stream_delay_ms: i32,
    vad_model_path: Option<PathBuf>,
) {
    let mut processor = match CaptureProcessor::new(stream_delay_ms, vad_model_path.as_deref()) {
        Ok(processor) => processor,
        Err(error) => {
            shared.record_error(error);
            shared.running.store(false, Ordering::Release);
            return;
        }
    };
    let mut capture = [0.0; DEVICE_FRAME_SAMPLES];
    let mut reference = [0.0; DEVICE_FRAME_SAMPLES];

    while shared.running.load(Ordering::Acquire) {
        if capture_consumer.occupied_len() < DEVICE_FRAME_SAMPLES {
            std::thread::sleep(Duration::from_millis(1));
            continue;
        }
        capture_consumer.pop_slice(&mut capture);
        reference.fill(0.0);
        let maximum_reference_backlog = DEVICE_FRAME_SAMPLES * 10;
        let reference_backlog = reference_consumer.occupied_len();
        if reference_backlog > maximum_reference_backlog {
            reference_consumer.skip(reference_backlog - maximum_reference_backlog);
        }
        reference_consumer.pop_slice(&mut reference);
        let output =
            match processor.process_with_interruption(&capture, &reference, |interruption_active| {
                shared
                    .interruption_active
                    .store(interruption_active, Ordering::Release);
            }) {
                Ok(output) => output,
                Err(error) => {
                    shared.record_error(error);
                    shared.running.store(false, Ordering::Release);
                    break;
                }
            };
        shared
            .speech_active
            .store(output.speech_active, Ordering::Release);
        enqueue_asr_samples(&mut asr_producer, &shared, &output.asr_samples);
    }
}

fn enqueue_asr_samples(producer: &mut HeapProd<f32>, shared: &SharedState, samples: &[f32]) {
    if !shared.asr_session_active.load(Ordering::Acquire) {
        return;
    }
    let accepted = producer.push_slice(samples);
    shared
        .dropped_asr
        .fetch_add((samples.len() - accepted) as u64, Ordering::Relaxed);
}

fn reference_safe_push(producer: &mut HeapProd<f32>, sample: f32) -> Result<(), f32> {
    producer.try_push(sample)
}

fn select_f32_48khz_input(device: &cpal::Device) -> Result<SupportedStreamConfig, AudioError> {
    let configs = device
        .supported_input_configs()
        .map_err(|error| AudioError::Device(format!("enumerate input formats: {error}")))?;
    select_f32_48khz(configs).ok_or_else(|| {
        AudioError::Device("default input device has no 48 kHz f32 format".to_owned())
    })
}

fn select_f32_48khz_output(device: &cpal::Device) -> Result<SupportedStreamConfig, AudioError> {
    let configs = device
        .supported_output_configs()
        .map_err(|error| AudioError::Device(format!("enumerate output formats: {error}")))?;
    select_f32_48khz(configs).ok_or_else(|| {
        AudioError::Device("default output device has no 48 kHz f32 format".to_owned())
    })
}

fn select_f32_48khz(
    configs: impl Iterator<Item = cpal::SupportedStreamConfigRange>,
) -> Option<SupportedStreamConfig> {
    configs
        .filter(|config| {
            config.sample_format() == SampleFormat::F32
                && config.min_sample_rate() <= DEVICE_SAMPLE_RATE
                && config.max_sample_rate() >= DEVICE_SAMPLE_RATE
        })
        .min_by_key(cpal::SupportedStreamConfigRange::channels)
        .map(|config| config.with_sample_rate(DEVICE_SAMPLE_RATE))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_clock_reports_elapsed_time() {
        let shared = SharedState::new();
        assert!(shared.elapsed_micros() < 1_000_000);
    }

    #[test]
    fn asr_queue_ignores_capture_outside_a_conversation() {
        let shared = Arc::new(SharedState::new());
        let (mut producer, consumer) = HeapRb::<f32>::new(4).split();
        enqueue_asr_samples(&mut producer, &shared, &[1.0, 2.0, 3.0, 4.0, 5.0]);
        assert_eq!(consumer.occupied_len(), 0);
        assert_eq!(shared.dropped_asr.load(Ordering::Relaxed), 0);

        shared.asr_session_active.store(true, Ordering::Release);
        enqueue_asr_samples(&mut producer, &shared, &[1.0, 2.0, 3.0, 4.0, 5.0]);
        assert_eq!(consumer.occupied_len(), 4);
        assert_eq!(shared.dropped_asr.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn unavailable_selected_microphone_is_reported() {
        let error = AudioEngine::start(AudioConfig {
            input_device_id: Some("fasttalk-missing-input-device".to_owned()),
            ..AudioConfig::default()
        })
        .err()
        .expect("a nonexistent input device must fail");
        assert!(matches!(error, AudioError::Device(_)));
    }
}
