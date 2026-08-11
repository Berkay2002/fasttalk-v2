use rustfft::FftPlanner;
use rustfft::num_complex::Complex;
use std::f64::consts::PI;
use std::path::Path;
use std::sync::Arc;
use tract_onnx::prelude::*;

const SAMPLE_RATE: usize = 16_000;
const WINDOW_SECONDS: usize = 8;
const AUDIO_SAMPLES: usize = SAMPLE_RATE * WINDOW_SECONDS;
const FFT_SIZE: usize = 400;
const HOP_SIZE: usize = 160;
const FREQUENCY_BINS: usize = FFT_SIZE / 2 + 1;
const MEL_BINS: usize = 80;
const FEATURE_FRAMES: usize = 800;
const MEL_FLOOR: f64 = 1e-10;
const NORMALIZATION_EPSILON: f32 = 1e-7;

pub struct SmartTurnDetector {
    model: Arc<TypedRunnableModel>,
}

impl SmartTurnDetector {
    pub fn new(model: &Path) -> Result<Self, String> {
        if !model.is_file() {
            return Err(format!("Smart Turn model is missing: {}", model.display()));
        }
        let model = tract_onnx::onnx()
            .model_for_path(model)
            .and_then(|model| model.into_optimized())
            .and_then(|model| model.into_runnable())
            .map_err(|error| format!("load Smart Turn model: {error}"))?;
        Ok(Self { model })
    }

    pub fn probability(&mut self, audio_16khz: &[f32]) -> Result<f32, String> {
        if audio_16khz.is_empty() {
            return Err("Smart Turn requires recorded turn audio".to_owned());
        }
        let features = whisper_features(audio_16khz);
        let input = Tensor::from_shape(&[1, MEL_BINS, FEATURE_FRAMES], &features)
            .map_err(|error| format!("create Smart Turn input: {error}"))?;
        let outputs = self
            .model
            .run(tvec!(input.into_tvalue()))
            .map_err(|error| format!("run Smart Turn model: {error}"))?;
        let values = outputs[0]
            .to_plain_array_view::<f32>()
            .map_err(|error| format!("read Smart Turn output: {error}"))?;
        let logit = values
            .iter()
            .next()
            .copied()
            .filter(|value| value.is_finite())
            .ok_or_else(|| "Smart Turn returned no finite output".to_owned())?;
        Ok(1.0 / (1.0 + (-logit).exp()))
    }

    pub fn is_complete(&mut self, audio_16khz: &[f32]) -> Result<bool, String> {
        self.probability(audio_16khz)
            .map(|probability| probability > 0.5)
    }
}

fn whisper_features(audio: &[f32]) -> Vec<f32> {
    let mut waveform = vec![0.0_f32; AUDIO_SAMPLES];
    let retained = audio.len().min(AUDIO_SAMPLES);
    waveform[AUDIO_SAMPLES - retained..].copy_from_slice(&audio[audio.len() - retained..]);
    normalize(&mut waveform);

    let padded = reflect_pad(&waveform, FFT_SIZE / 2);
    let window = (0..FFT_SIZE)
        .map(|index| 0.5 - 0.5 * (2.0 * PI * index as f64 / FFT_SIZE as f64).cos())
        .collect::<Vec<_>>();
    let filters = mel_filterbank();
    let mut planner = FftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(FFT_SIZE);
    let mut spectrum = vec![Complex::default(); FFT_SIZE];
    let mut features = vec![0.0_f64; MEL_BINS * FEATURE_FRAMES];

    for frame in 0..FEATURE_FRAMES {
        let offset = frame * HOP_SIZE;
        for index in 0..FFT_SIZE {
            spectrum[index] = Complex::new(padded[offset + index] * window[index], 0.0);
        }
        fft.process(&mut spectrum);
        for mel in 0..MEL_BINS {
            let energy = spectrum[..FREQUENCY_BINS]
                .iter()
                .enumerate()
                .map(|(bin, value)| value.norm_sqr() * filters[bin * MEL_BINS + mel])
                .sum::<f64>()
                .max(MEL_FLOOR);
            features[mel * FEATURE_FRAMES + frame] = energy.log10();
        }
    }

    let maximum = features.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    features
        .into_iter()
        .map(|value| ((value.max(maximum - 8.0) + 4.0) / 4.0) as f32)
        .collect()
}

fn normalize(waveform: &mut [f32]) {
    let mean = waveform.iter().sum::<f32>() / waveform.len() as f32;
    let variance = waveform
        .iter()
        .map(|sample| {
            let centered = *sample - mean;
            centered * centered
        })
        .sum::<f32>()
        / waveform.len() as f32;
    let scale = (variance + NORMALIZATION_EPSILON).sqrt();
    for sample in waveform {
        *sample = (*sample - mean) / scale;
    }
}

fn reflect_pad(waveform: &[f32], padding: usize) -> Vec<f64> {
    let mut padded = Vec::with_capacity(waveform.len() + padding * 2);
    padded.extend((1..=padding).rev().map(|index| waveform[index] as f64));
    padded.extend(waveform.iter().map(|sample| *sample as f64));
    padded.extend((1..=padding).map(|index| waveform[waveform.len() - 1 - index] as f64));
    padded
}

fn mel_filterbank() -> Vec<f64> {
    let mel_min = hertz_to_mel(0.0);
    let mel_max = hertz_to_mel(SAMPLE_RATE as f64 / 2.0);
    let filter_frequencies = (0..MEL_BINS + 2)
        .map(|index| {
            let mel = mel_min + (mel_max - mel_min) * index as f64 / (MEL_BINS + 1) as f64;
            mel_to_hertz(mel)
        })
        .collect::<Vec<_>>();
    let mut filters = vec![0.0; FREQUENCY_BINS * MEL_BINS];
    for bin in 0..FREQUENCY_BINS {
        let frequency = (SAMPLE_RATE / 2) as f64 * bin as f64 / (FREQUENCY_BINS - 1) as f64;
        for mel in 0..MEL_BINS {
            let lower = filter_frequencies[mel];
            let center = filter_frequencies[mel + 1];
            let upper = filter_frequencies[mel + 2];
            let down = (frequency - lower) / (center - lower);
            let up = (upper - frequency) / (upper - center);
            let area = 2.0 / (upper - lower);
            filters[bin * MEL_BINS + mel] = down.min(up).max(0.0) * area;
        }
    }
    filters
}

fn hertz_to_mel(frequency: f64) -> f64 {
    if frequency < 1_000.0 {
        3.0 * frequency / 200.0
    } else {
        15.0 + (frequency / 1_000.0).ln() * (27.0 / 6.4_f64.ln())
    }
}

fn mel_to_hertz(mel: f64) -> f64 {
    if mel < 15.0 {
        200.0 * mel / 3.0
    } else {
        1_000.0 * ((6.4_f64.ln() / 27.0) * (mel - 15.0)).exp()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whisper_features_have_the_smart_turn_shape() {
        let features = whisper_features(&vec![0.0; SAMPLE_RATE]);
        assert_eq!(features.len(), MEL_BINS * FEATURE_FRAMES);
        assert!(features.iter().all(|value| value.is_finite()));
    }

    #[test]
    #[ignore = "loads the pinned Smart Turn ONNX model"]
    fn pinned_model_returns_a_probability() {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut detector = SmartTurnDetector::new(
            &workspace.join(".cache/models/smart-turn/smart-turn-v3.2-cpu.onnx"),
        )
        .unwrap();
        let probability = detector.probability(&vec![0.0; SAMPLE_RATE]).unwrap();
        assert!((0.0..=1.0).contains(&probability));
    }
}
