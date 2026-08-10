use sonora::config::EchoCanceller;
use sonora::{AudioProcessing, Config, StreamConfig};

pub const DEVICE_SAMPLE_RATE: u32 = 48_000;
pub const ASR_SAMPLE_RATE: u32 = 16_000;
pub const DEVICE_FRAME_SAMPLES: usize = (DEVICE_SAMPLE_RATE / 100) as usize;
pub const ASR_FRAME_SAMPLES: usize = (ASR_SAMPLE_RATE / 100) as usize;

pub struct AecProcessor {
    processing: AudioProcessing,
    render_output: [f32; DEVICE_FRAME_SAMPLES],
}

impl AecProcessor {
    pub fn new(stream_delay_ms: i32) -> Result<Self, String> {
        let device = StreamConfig::new(DEVICE_SAMPLE_RATE, 1);
        let config = Config {
            echo_canceller: Some(EchoCanceller::default()),
            ..Default::default()
        };
        let mut processing = AudioProcessing::builder()
            .config(config)
            .capture_config(device)
            .render_config(device)
            .build();
        processing
            .set_stream_delay_ms(stream_delay_ms)
            .map_err(|error| format!("invalid AEC stream delay: {error:?}"))?;
        Ok(Self {
            processing,
            render_output: [0.0; DEVICE_FRAME_SAMPLES],
        })
    }

    pub fn process(
        &mut self,
        capture: &[f32; DEVICE_FRAME_SAMPLES],
        playback_reference: &[f32; DEVICE_FRAME_SAMPLES],
        output: &mut [f32; ASR_FRAME_SAMPLES],
    ) -> Result<(), String> {
        self.processing
            .process_render_f32(&[playback_reference], &mut [&mut self.render_output])
            .map_err(|error| format!("AEC render processing failed: {error:?}"))?;
        let input = StreamConfig::new(DEVICE_SAMPLE_RATE, 1);
        let output_config = StreamConfig::new(ASR_SAMPLE_RATE, 1);
        self.processing
            .process_capture_f32_with_config(&[capture], &input, &output_config, &mut [output])
            .map_err(|error| format!("AEC capture processing failed: {error:?}"))
    }
}

#[derive(Debug)]
pub struct SpeechDetector {
    threshold_rms: f32,
    attack_frames: u8,
    release_frames: u8,
    above: u8,
    below: u8,
    active: bool,
}

impl SpeechDetector {
    pub fn new(threshold_rms: f32, attack_frames: u8, release_frames: u8) -> Self {
        Self {
            threshold_rms,
            attack_frames,
            release_frames,
            above: 0,
            below: 0,
            active: false,
        }
    }

    pub fn update(&mut self, samples: &[f32]) -> bool {
        let mean_square =
            samples.iter().map(|sample| sample * sample).sum::<f32>() / samples.len().max(1) as f32;
        if mean_square.sqrt() >= self.threshold_rms {
            self.above = self.above.saturating_add(1);
            self.below = 0;
            if self.above >= self.attack_frames {
                self.active = true;
            }
        } else {
            self.below = self.below.saturating_add(1);
            self.above = 0;
            if self.below >= self.release_frames {
                self.active = false;
            }
        }
        self.active
    }
}

impl Default for SpeechDetector {
    fn default() -> Self {
        Self::new(0.015, 3, 10)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detector_uses_attack_and_release_hysteresis() {
        let mut detector = SpeechDetector::new(0.01, 2, 3);
        assert!(!detector.update(&[0.02; ASR_FRAME_SAMPLES]));
        assert!(detector.update(&[0.02; ASR_FRAME_SAMPLES]));
        assert!(detector.update(&[0.0; ASR_FRAME_SAMPLES]));
        assert!(detector.update(&[0.0; ASR_FRAME_SAMPLES]));
        assert!(!detector.update(&[0.0; ASR_FRAME_SAMPLES]));
    }

    #[test]
    fn aec_produces_finite_16khz_frames() {
        let mut processor = AecProcessor::new(40).unwrap();
        let mut output = [0.0; ASR_FRAME_SAMPLES];
        for frame_index in 0..20 {
            let mut render = [0.0; DEVICE_FRAME_SAMPLES];
            let mut capture = [0.0; DEVICE_FRAME_SAMPLES];
            for (sample_index, (render, capture)) in
                render.iter_mut().zip(capture.iter_mut()).enumerate()
            {
                let phase =
                    ((frame_index * DEVICE_FRAME_SAMPLES + sample_index) as f32 / 40.0).sin();
                *render = phase * 0.3;
                *capture = phase * 0.12;
            }
            processor.process(&capture, &render, &mut output).unwrap();
        }
        assert!(output.iter().all(|sample| sample.is_finite()));
    }
}
