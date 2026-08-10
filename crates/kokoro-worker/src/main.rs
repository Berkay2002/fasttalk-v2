use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use clap::Parser;
use serde::{Deserialize, Serialize};
use sherpa_onnx::{
    GenerationConfig, OfflineTts, OfflineTtsConfig, OfflineTtsKokoroModelConfig,
    OfflineTtsModelConfig,
};
use std::convert::Infallible;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

const SAMPLE_RATE: i32 = 24_000;
const MAX_INPUT_BYTES: usize = 8_192;

#[derive(Debug, Parser)]
#[command(name = "fasttalk-kokoro-worker")]
struct Args {
    #[arg(long)]
    model_dir: PathBuf,
    #[arg(long, default_value_t = IpAddr::V4(Ipv4Addr::LOCALHOST))]
    host: IpAddr,
    #[arg(long, default_value_t = 18_082)]
    port: u16,
    #[arg(long, default_value_t = 4)]
    threads: i32,
}

#[derive(Clone)]
struct WorkerState {
    engine: Arc<OfflineTts>,
    speakers: i32,
}

#[derive(Debug, Deserialize)]
struct SpeechRequest {
    input: String,
    voice: Option<String>,
    response_format: Option<String>,
}

#[derive(Debug, Serialize)]
struct ReadyResponse {
    ready: bool,
    sample_rate: i32,
    speakers: i32,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
}

#[tokio::main]
async fn main() {
    if let Err(error) = run(Args::parse()).await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

async fn run(args: Args) -> Result<(), String> {
    if !args.host.is_loopback() {
        return Err("Kokoro worker must bind to a loopback address".to_owned());
    }
    if args.threads <= 0 {
        return Err("Kokoro worker thread count must be positive".to_owned());
    }
    let engine = tokio::task::spawn_blocking({
        let model_dir = args.model_dir.clone();
        move || load_engine(&model_dir, args.threads)
    })
    .await
    .map_err(|error| format!("Kokoro load task failed: {error}"))??;
    if engine.sample_rate() != SAMPLE_RATE {
        return Err(format!(
            "Kokoro model returned {} Hz instead of {SAMPLE_RATE} Hz",
            engine.sample_rate()
        ));
    }
    let state = WorkerState {
        speakers: engine.num_speakers(),
        engine: Arc::new(engine),
    };
    let router = Router::new()
        .route("/ready", get(ready))
        .route("/v1/audio/speech", post(speech))
        .with_state(state);
    let address = SocketAddr::new(args.host, args.port);
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .map_err(|error| format!("bind {address}: {error}"))?;
    println!("Kokoro worker ready on http://{address}");
    axum::serve(listener, router)
        .await
        .map_err(|error| format!("serve Kokoro: {error}"))
}

async fn ready(State(state): State<WorkerState>) -> Json<ReadyResponse> {
    Json(ReadyResponse {
        ready: true,
        sample_rate: SAMPLE_RATE,
        speakers: state.speakers,
    })
}

async fn speech(State(state): State<WorkerState>, Json(request): Json<SpeechRequest>) -> Response {
    let input = request.input.trim().to_owned();
    if input.is_empty() || input.len() > MAX_INPUT_BYTES {
        return error_response(
            StatusCode::BAD_REQUEST,
            format!("input must contain 1 to {MAX_INPUT_BYTES} UTF-8 bytes"),
        );
    }
    if request.response_format.as_deref().unwrap_or("pcm") != "pcm" {
        return error_response(
            StatusCode::BAD_REQUEST,
            "response_format must be pcm".to_owned(),
        );
    }
    let speaker = match request.voice.as_deref().unwrap_or("10").parse::<i32>() {
        Ok(speaker) if (0..state.speakers).contains(&speaker) => speaker,
        _ => {
            return error_response(
                StatusCode::BAD_REQUEST,
                format!("voice must be an integer in 0..{}", state.speakers),
            );
        }
    };

    let (sender, receiver) = mpsc::channel::<Result<Bytes, Infallible>>(8);
    tokio::task::spawn_blocking(move || {
        synthesize_to_response(&state.engine, &input, speaker, sender);
    });
    let mut response = Body::from_stream(ReceiverStream::new(receiver)).into_response();
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("audio/pcm"),
    );
    response
        .headers_mut()
        .insert("x-sample-rate", HeaderValue::from_static("24000"));
    response
}

fn synthesize_to_response(
    engine: &OfflineTts,
    text: &str,
    speaker: i32,
    sender: mpsc::Sender<Result<Bytes, Infallible>>,
) {
    let emitted = Arc::new(AtomicUsize::new(0));
    let callback_emitted = Arc::clone(&emitted);
    let callback_sender = sender.clone();
    let Some(audio) = engine.generate_with_config(
        text,
        &GenerationConfig {
            sid: speaker,
            ..GenerationConfig::default()
        },
        Some(move |samples: &[f32], _progress| {
            let start = callback_emitted.load(Ordering::Relaxed).min(samples.len());
            if start < samples.len()
                && callback_sender
                    .blocking_send(Ok(Bytes::from(encode_pcm16(&samples[start..]))))
                    .is_err()
            {
                return false;
            }
            callback_emitted.store(samples.len(), Ordering::Relaxed);
            true
        }),
    ) else {
        return;
    };
    let sent = emitted.load(Ordering::Relaxed).min(audio.samples().len());
    if sent < audio.samples().len() {
        let _ = sender.blocking_send(Ok(Bytes::from(encode_pcm16(&audio.samples()[sent..]))));
    }
}

fn encode_pcm16(samples: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(samples.len() * 2);
    for sample in samples {
        let pcm = (sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16;
        bytes.extend_from_slice(&pcm.to_le_bytes());
    }
    bytes
}

fn load_engine(model_dir: &Path, threads: i32) -> Result<OfflineTts, String> {
    for name in [
        "model.int8.onnx",
        "voices.bin",
        "tokens.txt",
        "espeak-ng-data",
    ] {
        let path = model_dir.join(name);
        if !path.exists() {
            return Err(format!("Kokoro asset is missing: {}", path.display()));
        }
    }
    let kokoro = OfflineTtsKokoroModelConfig {
        model: Some(path_text(&model_dir.join("model.int8.onnx"))?),
        voices: Some(path_text(&model_dir.join("voices.bin"))?),
        tokens: Some(path_text(&model_dir.join("tokens.txt"))?),
        data_dir: Some(path_text(&model_dir.join("espeak-ng-data"))?),
        lang: Some("en-us".to_owned()),
        ..OfflineTtsKokoroModelConfig::default()
    };
    OfflineTts::create(&OfflineTtsConfig {
        model: OfflineTtsModelConfig {
            kokoro,
            num_threads: threads,
            provider: Some("cpu".to_owned()),
            ..OfflineTtsModelConfig::default()
        },
        max_num_sentences: 1,
        ..OfflineTtsConfig::default()
    })
    .ok_or_else(|| "failed to load Kokoro model".to_owned())
}

fn path_text(path: &Path) -> Result<String, String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("path is not valid UTF-8: {}", path.display()))
}

fn error_response(status: StatusCode, error: String) -> Response {
    (status, Json(ErrorResponse { error })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::Instant;

    #[test]
    fn pcm_encoding_clamps_and_uses_little_endian() {
        assert_eq!(
            encode_pcm16(&[-2.0, 0.0, 0.5, 2.0]),
            [0x01, 0x80, 0, 0, 0, 0x40, 0xff, 0x7f]
        );
    }

    #[test]
    #[ignore = "loads the pinned CPU Kokoro model"]
    fn pinned_kokoro_model_synthesizes_and_cancels() {
        let model_dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.cache/models/kokoro-sherpa");
        let load_started = Instant::now();
        let engine = load_engine(&model_dir, 4).unwrap();
        println!(
            "kokoro_load_ms={:.3}",
            load_started.elapsed().as_secs_f64() * 1_000.0
        );

        let synthesis_started = Instant::now();
        let first_callback = Arc::new(Mutex::new(None));
        let callback_time = Arc::clone(&first_callback);
        let callback_count = Arc::new(AtomicUsize::new(0));
        let callback_counter = Arc::clone(&callback_count);
        let audio = engine
            .generate_with_config(
                "FastTalk can move speech synthesis to the CPU when GPU memory is constrained.",
                &GenerationConfig {
                    sid: 10,
                    ..GenerationConfig::default()
                },
                Some(move |samples: &[f32], _progress| {
                    callback_counter.fetch_add(1, Ordering::Relaxed);
                    if !samples.is_empty() {
                        callback_time
                            .lock()
                            .unwrap()
                            .get_or_insert(synthesis_started.elapsed());
                    }
                    true
                }),
            )
            .unwrap();
        assert!(audio.samples().len() > SAMPLE_RATE as usize);
        let elapsed = synthesis_started.elapsed();
        let first_callback = first_callback.lock().unwrap().unwrap();
        let audio_seconds = audio.samples().len() as f64 / SAMPLE_RATE as f64;
        println!(
            "kokoro_first_callback_ms={:.3}",
            first_callback.as_secs_f64() * 1_000.0
        );
        println!("kokoro_completed_ms={:.3}", elapsed.as_secs_f64() * 1_000.0);
        println!("kokoro_samples={}", audio.samples().len());
        println!(
            "kokoro_callbacks={}",
            callback_count.load(Ordering::Relaxed)
        );
        println!("kokoro_rtf={:.3}", elapsed.as_secs_f64() / audio_seconds);

        let cancel_started = Instant::now();
        let cancel_callback = Arc::new(Mutex::new(None));
        let cancel_time = Arc::clone(&cancel_callback);
        let cancelled = engine.generate_with_config(
            "This synthesis should stop as soon as its first audio callback arrives.",
            &GenerationConfig {
                sid: 10,
                ..GenerationConfig::default()
            },
            Some(move |samples: &[f32], _progress| {
                if samples.is_empty() {
                    return true;
                }
                cancel_time
                    .lock()
                    .unwrap()
                    .get_or_insert(cancel_started.elapsed());
                false
            }),
        );
        let cancel_callback = cancel_callback.lock().unwrap().unwrap();
        println!(
            "kokoro_cancel_callback_ms={:.3}",
            cancel_callback.as_secs_f64() * 1_000.0
        );
        println!(
            "kokoro_cancel_return_ms={:.3}",
            cancel_started.elapsed().as_secs_f64() * 1_000.0
        );
        assert!(cancelled.is_none() || cancelled.unwrap().samples().len() < audio.samples().len());
    }
}
