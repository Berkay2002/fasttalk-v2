use crate::{PipelineError, validate_loopback_endpoint};
use futures_util::StreamExt;
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
    calculate_cutoff,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const MAGPIE_SAMPLE_RATE: u32 = 22_050;
const PLAYBACK_SAMPLE_RATE: u32 = 48_000;
const KOKORO_SAMPLE_RATE: u32 = 24_000;
const RESAMPLER_INPUT_CHUNK: usize = 256;

#[derive(Clone, Debug, PartialEq)]
pub enum TtsEvent {
    Pcm48KhzMono(Vec<f32>),
    Completed,
}

pub struct MagpieClient {
    client: reqwest::Client,
    endpoint: String,
}

impl MagpieClient {
    pub fn new(base_url: &str) -> Result<Self, PipelineError> {
        validate_loopback_endpoint(base_url, "http")?;
        Ok(Self {
            client: reqwest::Client::builder()
                .no_proxy()
                .build()
                .map_err(PipelineError::Http)?,
            endpoint: format!("{}/v1/audio/speech", base_url.trim_end_matches('/')),
        })
    }

    pub async fn synthesize(
        &self,
        text: &str,
        cancellation: CancellationToken,
        events: mpsc::Sender<TtsEvent>,
    ) -> Result<(), PipelineError> {
        let response = self
            .client
            .post(&self.endpoint)
            .json(&serde_json::json!({
                "input": text,
                "voice": "0",
                "language": "en-US",
                "sample_rate": MAGPIE_SAMPLE_RATE,
                "response_format": "pcm"
            }))
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(PipelineError::Worker {
                status: status.as_u16(),
                message: response.text().await.unwrap_or_default(),
            });
        }
        let mut stream = response.bytes_stream();
        let mut trailing_byte = None;
        let mut resampler = StreamingResampler::new(MAGPIE_SAMPLE_RATE)?;
        loop {
            let chunk = tokio::select! {
                _ = cancellation.cancelled() => {
                    tokio::spawn(async move {
                        while let Some(chunk) = stream.next().await {
                            if chunk.is_err() {
                                break;
                            }
                        }
                    });
                    return Err(PipelineError::Cancelled);
                },
                chunk = stream.next() => chunk,
            };
            let Some(chunk) = chunk else { break };
            let chunk = chunk?;
            let samples = resampler.push(&decode_pcm16(&chunk, &mut trailing_byte))?;
            if !samples.is_empty() {
                events
                    .send(TtsEvent::Pcm48KhzMono(samples))
                    .await
                    .map_err(|_| PipelineError::Cancelled)?;
            }
        }
        if trailing_byte.is_some() {
            return Err(PipelineError::Protocol(
                "TTS worker returned an odd number of PCM16 bytes".to_owned(),
            ));
        }
        let tail = resampler.finish()?;
        if !tail.is_empty() {
            events
                .send(TtsEvent::Pcm48KhzMono(tail))
                .await
                .map_err(|_| PipelineError::Cancelled)?;
        }
        events
            .send(TtsEvent::Completed)
            .await
            .map_err(|_| PipelineError::Cancelled)
    }
}

pub struct KokoroClient {
    client: reqwest::Client,
    endpoint: String,
}

impl KokoroClient {
    pub fn new(base_url: &str) -> Result<Self, PipelineError> {
        validate_loopback_endpoint(base_url, "http")?;
        Ok(Self {
            client: reqwest::Client::builder()
                .no_proxy()
                .build()
                .map_err(PipelineError::Http)?,
            endpoint: format!("{}/v1/audio/speech", base_url.trim_end_matches('/')),
        })
    }

    pub async fn synthesize(
        &self,
        text: &str,
        cancellation: CancellationToken,
        events: mpsc::Sender<TtsEvent>,
    ) -> Result<(), PipelineError> {
        let response = self
            .client
            .post(&self.endpoint)
            .json(&serde_json::json!({
                "input": text,
                "voice": "10",
                "language": "en-US",
                "sample_rate": KOKORO_SAMPLE_RATE,
                "response_format": "pcm"
            }))
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(PipelineError::Worker {
                status: status.as_u16(),
                message: response.text().await.unwrap_or_default(),
            });
        }
        let mut stream = response.bytes_stream();
        let mut trailing_byte = None;
        let mut resampler = StreamingResampler::new(KOKORO_SAMPLE_RATE)?;
        loop {
            let chunk = tokio::select! {
                _ = cancellation.cancelled() => return Err(PipelineError::Cancelled),
                chunk = stream.next() => chunk,
            };
            let Some(chunk) = chunk else { break };
            let chunk = chunk?;
            send_resampled(
                &events,
                resampler.push(&decode_pcm16(&chunk, &mut trailing_byte))?,
            )
            .await?;
        }
        if trailing_byte.is_some() {
            return Err(PipelineError::Protocol(
                "Kokoro worker returned an odd number of PCM16 bytes".to_owned(),
            ));
        }
        send_resampled(&events, resampler.finish()?).await?;
        events
            .send(TtsEvent::Completed)
            .await
            .map_err(|_| PipelineError::Cancelled)
    }
}

async fn send_resampled(
    events: &mpsc::Sender<TtsEvent>,
    samples: Vec<f32>,
) -> Result<(), PipelineError> {
    if samples.is_empty() {
        return Ok(());
    }
    events
        .send(TtsEvent::Pcm48KhzMono(samples))
        .await
        .map_err(|_| PipelineError::Cancelled)
}

struct StreamingResampler {
    inner: SincFixedIn<f32>,
    input_sample_rate: u32,
    pending: Vec<f32>,
    pending_offset: usize,
    input_samples: usize,
    output_samples: usize,
}

impl StreamingResampler {
    fn new(input_sample_rate: u32) -> Result<Self, PipelineError> {
        let sinc_len = 128;
        let window = WindowFunction::Blackman2;
        let parameters = SincInterpolationParameters {
            sinc_len,
            f_cutoff: calculate_cutoff(sinc_len, window),
            oversampling_factor: 128,
            interpolation: SincInterpolationType::Quadratic,
            window,
        };
        let inner = SincFixedIn::new(
            PLAYBACK_SAMPLE_RATE as f64 / input_sample_rate as f64,
            1.0,
            parameters,
            RESAMPLER_INPUT_CHUNK,
            1,
        )
        .map_err(|error| PipelineError::Protocol(format!("TTS resampler init failed: {error}")))?;
        Ok(Self {
            inner,
            input_sample_rate,
            pending: Vec::with_capacity(RESAMPLER_INPUT_CHUNK * 2),
            pending_offset: 0,
            input_samples: 0,
            output_samples: 0,
        })
    }

    fn push(&mut self, samples: &[f32]) -> Result<Vec<f32>, PipelineError> {
        self.input_samples += samples.len();
        self.pending.extend_from_slice(samples);
        let mut output = Vec::new();
        while self.pending.len() - self.pending_offset >= RESAMPLER_INPUT_CHUNK {
            let end = self.pending_offset + RESAMPLER_INPUT_CHUNK;
            let channel = &self.pending[self.pending_offset..end];
            let resampled = self.inner.process(&[channel], None).map_err(|error| {
                PipelineError::Protocol(format!("TTS resampling failed: {error}"))
            })?;
            output.extend_from_slice(&resampled[0]);
            self.pending_offset = end;
        }
        if self.pending_offset >= RESAMPLER_INPUT_CHUNK * 8 {
            self.pending.drain(..self.pending_offset);
            self.pending_offset = 0;
        }
        self.output_samples += output.len();
        Ok(output)
    }

    fn finish(mut self) -> Result<Vec<f32>, PipelineError> {
        let remaining = &self.pending[self.pending_offset..];
        let partial = self
            .inner
            .process_partial((!remaining.is_empty()).then_some(&[remaining][..]), None)
            .map_err(|error| {
                PipelineError::Protocol(format!("TTS resampler flush failed: {error}"))
            })?;
        let mut output = partial[0].clone();
        let expected_total = ((self.input_samples as u128 * PLAYBACK_SAMPLE_RATE as u128)
            / self.input_sample_rate as u128) as usize;
        let remaining_output = expected_total.saturating_sub(self.output_samples);
        output.truncate(remaining_output);
        if output.len() < remaining_output {
            let flushed = self
                .inner
                .process_partial::<&[f32]>(None, None)
                .map_err(|error| {
                    PipelineError::Protocol(format!("TTS resampler drain failed: {error}"))
                })?;
            let needed = remaining_output - output.len();
            output.extend_from_slice(&flushed[0][..needed.min(flushed[0].len())]);
        }
        output.truncate(remaining_output);
        Ok(output)
    }
}

fn decode_pcm16(bytes: &[u8], trailing_byte: &mut Option<u8>) -> Vec<f32> {
    let mut samples = Vec::with_capacity((bytes.len() + usize::from(trailing_byte.is_some())) / 2);
    let mut chunks = bytes.chunks_exact(2);
    if let Some(low) = trailing_byte.take() {
        if let Some((&high, rest)) = bytes.split_first() {
            samples.push(i16::from_le_bytes([low, high]) as f32 / 32768.0);
            chunks = rest.chunks_exact(2);
        } else {
            *trailing_byte = Some(low);
            return samples;
        }
    }
    for chunk in &mut chunks {
        samples.push(i16::from_le_bytes([chunk[0], chunk[1]]) as f32 / 32768.0);
    }
    *trailing_byte = chunks.remainder().first().copied();
    samples
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm_decoder_preserves_odd_byte_across_chunks() {
        let mut trailing = None;
        assert_eq!(decode_pcm16(&[0x00], &mut trailing), Vec::<f32>::new());
        assert_eq!(trailing, Some(0));
        let samples = decode_pcm16(&[0x40, 0x00], &mut trailing);
        assert_eq!(samples, [0.5]);
        assert_eq!(trailing, Some(0));
    }

    #[test]
    fn streaming_resampler_preserves_one_second_duration() {
        let input = (0..MAGPIE_SAMPLE_RATE)
            .map(|index| {
                (index as f32 * 440.0 * std::f32::consts::TAU / MAGPIE_SAMPLE_RATE as f32).sin()
            })
            .collect::<Vec<_>>();
        let mut resampler = StreamingResampler::new(MAGPIE_SAMPLE_RATE).unwrap();
        let mut output = Vec::new();
        for chunk in input.chunks(317) {
            output.extend(resampler.push(chunk).unwrap());
        }
        output.extend(resampler.finish().unwrap());
        assert_eq!(output.len(), PLAYBACK_SAMPLE_RATE as usize);
        assert!(output.iter().all(|sample| sample.is_finite()));
    }

    #[test]
    fn streaming_resampler_preserves_short_clause_durations() {
        for input_len in [1, 10, 255, 256, 257, 1_000] {
            let input = vec![0.25; input_len];
            let mut resampler = StreamingResampler::new(MAGPIE_SAMPLE_RATE).unwrap();
            let mut output = resampler.push(&input).unwrap();
            output.extend(resampler.finish().unwrap());
            let expected = input_len * PLAYBACK_SAMPLE_RATE as usize / MAGPIE_SAMPLE_RATE as usize;
            assert_eq!(output.len(), expected, "input length {input_len}");
        }
    }
}
