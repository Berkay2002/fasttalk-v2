use fasttalk_app_lib::native::{NativeRuntime, NativeRuntimeStatus, PreferredTtsBackend};
use fasttalk_audio::{AudioConfig, AudioEngine};
use fasttalk_pipeline::{
    AsrEvent, CancellationToken, ChatMessage, KokoroClient, LlmClient, LlmEvent, MagpieClient,
    RealtimeAsrClient, TtsEvent,
};
use fasttalk_runtime::WorkerState;
use serde::Serialize;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::time::{sleep, timeout};

const ASR_SAMPLE_RATE: usize = 16_000;
const ASR_CHUNK_SAMPLES: usize = 2_560;
const READY_TIMEOUT: Duration = Duration::from_secs(300);
const TURN_TIMEOUT: Duration = Duration::from_secs(90);

#[derive(Debug)]
struct Args {
    turns: usize,
    soak_minutes: f64,
    output: PathBuf,
    skip_audio: bool,
}

impl Args {
    fn parse() -> Result<Self, String> {
        let mut turns: usize = 20;
        let mut soak_minutes: f64 = 0.0;
        let mut output = PathBuf::from("artifacts/release/conversation-benchmark.json");
        let mut skip_audio = false;
        let mut arguments = env::args().skip(1);
        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "--turns" => {
                    turns = value(&mut arguments, "--turns")?
                        .parse()
                        .map_err(|_| "--turns must be a positive integer".to_owned())?;
                }
                "--soak-minutes" => {
                    soak_minutes = value(&mut arguments, "--soak-minutes")?
                        .parse()
                        .map_err(|_| "--soak-minutes must be a number".to_owned())?;
                }
                "--output" => output = PathBuf::from(value(&mut arguments, "--output")?),
                "--skip-audio" => skip_audio = true,
                unknown => return Err(format!("unknown argument: {unknown}")),
            }
        }
        if turns == 0 {
            return Err("--turns must be greater than zero".to_owned());
        }
        if !soak_minutes.is_finite() || soak_minutes < 0.0 {
            return Err("--soak-minutes must be finite and non-negative".to_owned());
        }
        Ok(Self {
            turns,
            soak_minutes,
            output,
            skip_audio,
        })
    }
}

fn value(arguments: &mut impl Iterator<Item = String>, name: &str) -> Result<String, String> {
    arguments
        .next()
        .ok_or_else(|| format!("{name} requires a value"))
}

#[derive(Debug)]
struct RecordedAudio {
    path: PathBuf,
    samples: Vec<f32>,
    last_active_sample: usize,
}

#[derive(Debug)]
struct Transcription {
    text: String,
    speech_ended_at: Instant,
    partial_update_ms: Vec<f64>,
}

#[derive(Debug)]
struct TurnMeasurement {
    transcript: String,
    asr_partial_update_ms: Vec<f64>,
    end_of_speech_to_first_audio_ms: f64,
    warm_llm_first_token_ms: f64,
    first_clause: String,
    first_audio: Vec<f32>,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct SoakEvidence {
    requested_minutes: f64,
    duration_minutes: f64,
    completed_turns: usize,
    turn_failure_count: usize,
    oom_count: usize,
    worker_failure_count: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ConversationEvidence {
    schema_version: u32,
    prerecorded_audio: String,
    turns: usize,
    tts_backend: PreferredTtsBackend,
    transcripts: Vec<String>,
    first_clauses: Vec<String>,
    end_of_speech_to_first_audio_ms: Vec<f64>,
    warm_llm_first_token_ms: Vec<f64>,
    asr_partial_update_ms: Vec<f64>,
    barge_in_to_silence_ms: Vec<f64>,
    warmed_gpu_memory_mib: Vec<f64>,
    soak: SoakEvidence,
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("release gate failed: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let args = Args::parse()?;
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let audio = load_recorded_audio(
        &workspace.join(".cache/sources/nemo-speech.cpp/test_files/asr/wav/test/jfk.wav"),
    )?;
    let mut runtime = NativeRuntime::for_development_checkout();
    let result = run_with_runtime(&args, &audio, &mut runtime).await;
    if let Err(error) = &result {
        match runtime.poll() {
            Ok(status) => eprintln!("native status after failure ({error}): {status:?}"),
            Err(status_error) => {
                eprintln!("native status unavailable after failure ({error}): {status_error}")
            }
        }
    }
    let stop_result = runtime.stop().map_err(|error| error.to_string());
    result?;
    stop_result?;
    Ok(())
}

async fn run_with_runtime(
    args: &Args,
    audio: &RecordedAudio,
    runtime: &mut NativeRuntime,
) -> Result<(), String> {
    let status = wait_for_workers(runtime).await?;
    let mut evidence = ConversationEvidence {
        schema_version: 1,
        prerecorded_audio: audio.path.display().to_string(),
        turns: args.turns,
        tts_backend: status.tts_backend,
        transcripts: Vec::with_capacity(args.turns),
        first_clauses: Vec::with_capacity(args.turns),
        end_of_speech_to_first_audio_ms: Vec::with_capacity(args.turns),
        warm_llm_first_token_ms: Vec::with_capacity(args.turns),
        asr_partial_update_ms: Vec::new(),
        barge_in_to_silence_ms: Vec::new(),
        warmed_gpu_memory_mib: Vec::new(),
        soak: SoakEvidence {
            requested_minutes: args.soak_minutes,
            ..SoakEvidence::default()
        },
    };
    let mut playback_fixture = Vec::new();

    for index in 0..args.turns {
        let measurement = timeout(TURN_TIMEOUT, measure_turn(audio, status.tts_backend))
            .await
            .map_err(|_| {
                format!(
                    "turn {} exceeded {} seconds",
                    index + 1,
                    TURN_TIMEOUT.as_secs()
                )
            })??;
        println!(
            "turn {}/{}: {:.1} ms to audio, {:.1} ms TTFT",
            index + 1,
            args.turns,
            measurement.end_of_speech_to_first_audio_ms,
            measurement.warm_llm_first_token_ms
        );
        if playback_fixture.is_empty() {
            playback_fixture = measurement.first_audio.clone();
        }
        evidence.transcripts.push(measurement.transcript);
        evidence.first_clauses.push(measurement.first_clause);
        evidence
            .asr_partial_update_ms
            .extend(measurement.asr_partial_update_ms.into_iter().map(round3));
        evidence
            .end_of_speech_to_first_audio_ms
            .push(round3(measurement.end_of_speech_to_first_audio_ms));
        evidence
            .warm_llm_first_token_ms
            .push(round3(measurement.warm_llm_first_token_ms));
        evidence
            .warmed_gpu_memory_mib
            .extend(query_gpu_memory_mib());
    }

    if !args.skip_audio {
        evidence.barge_in_to_silence_ms = measure_barge_in(&playback_fixture, args.turns)?;
    }
    if args.soak_minutes > 0.0 {
        evidence.soak = run_soak(audio, status.tts_backend, runtime, args.soak_minutes).await;
    }

    if let Some(parent) = args.output.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let mut json = serde_json::to_string_pretty(&evidence).map_err(|error| error.to_string())?;
    json.push('\n');
    fs::write(&args.output, json).map_err(|error| error.to_string())?;
    println!("wrote {}", args.output.display());
    Ok(())
}

async fn wait_for_workers(runtime: &mut NativeRuntime) -> Result<NativeRuntimeStatus, String> {
    let deadline = Instant::now() + READY_TIMEOUT;
    let mut status = runtime.start().map_err(|error| error.to_string())?;
    loop {
        if workers_ready(&status) {
            return Ok(status);
        }
        if worker_failed(&status) {
            return Err(format!("native worker failed while starting: {status:?}"));
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "native workers were not ready after {} seconds",
                READY_TIMEOUT.as_secs()
            ));
        }
        sleep(Duration::from_secs(1)).await;
        status = runtime.poll().map_err(|error| error.to_string())?;
    }
}

fn workers_ready(status: &NativeRuntimeStatus) -> bool {
    status.llm.as_ref().map(|worker| &worker.state) == Some(&WorkerState::Ready)
        && status.speech.as_ref().map(|worker| &worker.state) == Some(&WorkerState::Ready)
        && status.kokoro.as_ref().map(|worker| &worker.state) == Some(&WorkerState::Ready)
}

fn worker_failed(status: &NativeRuntimeStatus) -> bool {
    [&status.llm, &status.speech, &status.kokoro]
        .into_iter()
        .flatten()
        .any(|worker| worker.state == WorkerState::Failed)
}

async fn measure_turn(
    audio: &RecordedAudio,
    backend: PreferredTtsBackend,
) -> Result<TurnMeasurement, String> {
    let transcription = transcribe(audio).await?;
    let llm = LlmClient::new("http://127.0.0.1:18080").map_err(|error| error.to_string())?;
    let (llm_tx, mut llm_rx) = mpsc::channel(128);
    let llm_started = Instant::now();
    let llm_task = tokio::spawn({
        let prompt = format!(
            "Reply with one short sentence about this transcript: {}",
            transcription.text
        );
        async move {
            llm.stream_reply(
                vec![ChatMessage {
                    role: "user".to_owned(),
                    content: prompt,
                }],
                CancellationToken::new(),
                llm_tx,
            )
            .await
        }
    });

    let mut first_token_ms = None;
    let first_clause = loop {
        match llm_rx.recv().await {
            Some(LlmEvent::Delta(_)) => {
                first_token_ms.get_or_insert_with(|| llm_started.elapsed().as_secs_f64() * 1_000.0);
            }
            Some(LlmEvent::Clause(clause)) => break clause,
            Some(LlmEvent::Completed(answer)) if !answer.trim().is_empty() => break answer,
            Some(LlmEvent::Completed(_)) | None => {
                return Err("LLM completed without speech text".to_owned());
            }
        }
    };
    let llm_drain = tokio::spawn(async move { while llm_rx.recv().await.is_some() {} });

    let (tts_tx, mut tts_rx) = mpsc::channel(32);
    let tts_task = spawn_tts(
        backend,
        first_clause.clone(),
        CancellationToken::new(),
        tts_tx,
    )?;
    let mut first_audio = None;
    let mut first_audio_at = None;
    while let Some(event) = tts_rx.recv().await {
        match event {
            TtsEvent::Pcm48KhzMono(samples) if !samples.is_empty() => {
                if first_audio.is_none() {
                    first_audio_at = Some(Instant::now());
                    first_audio = Some(samples);
                }
            }
            TtsEvent::Pcm48KhzMono(_) => {}
            TtsEvent::Completed => break,
        }
    }
    let first_audio = first_audio.ok_or_else(|| "TTS completed without PCM".to_owned())?;
    let first_audio_at = first_audio_at.ok_or_else(|| "TTS emitted no timed PCM".to_owned())?;
    llm_task
        .await
        .map_err(|error| format!("LLM task failed: {error}"))?
        .map_err(|error| format!("LLM stream failed: {error}"))?;
    llm_drain
        .await
        .map_err(|error| format!("LLM event drain failed: {error}"))?;
    tts_task
        .await
        .map_err(|error| format!("TTS task failed: {error}"))?
        .map_err(|error| format!("TTS stream failed: {error}"))?;

    Ok(TurnMeasurement {
        transcript: transcription.text,
        asr_partial_update_ms: transcription.partial_update_ms,
        end_of_speech_to_first_audio_ms: first_audio_at
            .duration_since(transcription.speech_ended_at)
            .as_secs_f64()
            * 1_000.0,
        warm_llm_first_token_ms: first_token_ms
            .ok_or_else(|| "LLM emitted no content delta".to_owned())?,
        first_clause,
        first_audio,
    })
}

fn spawn_tts(
    backend: PreferredTtsBackend,
    text: String,
    cancellation: CancellationToken,
    events: mpsc::Sender<TtsEvent>,
) -> Result<tokio::task::JoinHandle<Result<(), String>>, String> {
    match backend {
        PreferredTtsBackend::Magpie => {
            let client =
                MagpieClient::new("http://127.0.0.1:18081").map_err(|error| error.to_string())?;
            Ok(tokio::spawn(async move {
                client
                    .synthesize(&text, cancellation, events)
                    .await
                    .map_err(|error| error.to_string())
            }))
        }
        PreferredTtsBackend::Kokoro => {
            let client =
                KokoroClient::new("http://127.0.0.1:18082").map_err(|error| error.to_string())?;
            Ok(tokio::spawn(async move {
                client
                    .synthesize(&text, cancellation, events)
                    .await
                    .map_err(|error| error.to_string())
            }))
        }
    }
}

async fn transcribe(audio: &RecordedAudio) -> Result<Transcription, String> {
    let client = RealtimeAsrClient::new("ws://127.0.0.1:18081/v1/realtime")
        .map_err(|error| error.to_string())?;
    let (mut sender, mut receiver) = client.connect().await.map_err(|error| error.to_string())?;
    loop {
        match receiver.next_event().await {
            Some(Ok(AsrEvent::SessionReady)) => break,
            Some(Ok(_)) => {}
            Some(Err(error)) => return Err(error.to_string()),
            None => return Err("ASR stream closed before session.ready".to_owned()),
        }
    }

    let last_audio_sent = Arc::new(Mutex::new(Instant::now()));
    let receiver_clock = last_audio_sent.clone();
    let receiver_task = tokio::spawn(async move {
        let mut final_transcript = String::new();
        let mut partial_update_ms = Vec::new();
        while let Some(event) = receiver.next_event().await {
            match event.map_err(|error| error.to_string())? {
                AsrEvent::Partial(text) if !text.trim().is_empty() => {
                    let last_sent = *receiver_clock
                        .lock()
                        .map_err(|_| "ASR timing lock is poisoned".to_owned())?;
                    partial_update_ms.push(last_sent.elapsed().as_secs_f64() * 1_000.0);
                }
                AsrEvent::Final(text) => final_transcript = text,
                AsrEvent::Committed => return Ok((final_transcript, partial_update_ms)),
                _ => {}
            }
        }
        Err("ASR stream closed before commit".to_owned())
    });

    let mut speech_ended_at = None;
    for (index, chunk) in audio.samples.chunks(ASR_CHUNK_SAMPLES).enumerate() {
        sleep(Duration::from_secs_f64(
            chunk.len() as f64 / ASR_SAMPLE_RATE as f64,
        ))
        .await;
        sender
            .send_f32(chunk)
            .await
            .map_err(|error| error.to_string())?;
        let sent_at = Instant::now();
        *last_audio_sent
            .lock()
            .map_err(|_| "ASR timing lock is poisoned".to_owned())? = sent_at;
        let start = index * ASR_CHUNK_SAMPLES;
        let end = start + chunk.len();
        if (start..end).contains(&audio.last_active_sample) {
            let trailing = end - audio.last_active_sample - 1;
            speech_ended_at = sent_at.checked_sub(Duration::from_secs_f64(
                trailing as f64 / ASR_SAMPLE_RATE as f64,
            ));
        }
    }
    sender.commit().await.map_err(|error| error.to_string())?;
    let (text, partial_update_ms) = receiver_task.await.map_err(|error| error.to_string())??;
    sender.close().await.map_err(|error| error.to_string())?;
    if text.trim().is_empty() {
        return Err("ASR returned an empty final transcript".to_owned());
    }
    Ok(Transcription {
        text,
        speech_ended_at: speech_ended_at
            .ok_or_else(|| "prerecorded audio contains no active speech".to_owned())?,
        partial_update_ms,
    })
}

fn measure_barge_in(samples: &[f32], repetitions: usize) -> Result<Vec<f64>, String> {
    if samples.is_empty() {
        return Err("barge-in fixture contains no PCM".to_owned());
    }
    let mut engine =
        AudioEngine::start(AudioConfig::default()).map_err(|error| error.to_string())?;
    let mut fixture = Vec::with_capacity(96_000);
    while fixture.len() < 96_000 {
        fixture.extend_from_slice(samples);
    }
    fixture.truncate(96_000);
    let mut measurements = Vec::with_capacity(repetitions);
    for _ in 0..repetitions {
        let accepted = engine.queue_playback_partial(&fixture);
        if accepted < 24_000 {
            return Err(format!("audio queue accepted only {accepted} samples"));
        }
        std::thread::sleep(Duration::from_millis(100));
        engine.cancel_playback();
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if let Some(latency) = engine.status().last_cancel_to_callback_ms {
                measurements.push(round3(latency));
                break;
            }
            if Instant::now() >= deadline {
                return Err("WASAPI callback did not acknowledge playback cancellation".to_owned());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    engine.stop();
    Ok(measurements)
}

async fn run_soak(
    audio: &RecordedAudio,
    backend: PreferredTtsBackend,
    runtime: &mut NativeRuntime,
    requested_minutes: f64,
) -> SoakEvidence {
    let started = Instant::now();
    let target = Duration::from_secs_f64(requested_minutes * 60.0);
    let mut evidence = SoakEvidence {
        requested_minutes,
        ..SoakEvidence::default()
    };
    while started.elapsed() < target {
        match timeout(TURN_TIMEOUT, run_full_turn(audio, backend)).await {
            Ok(Ok(())) => evidence.completed_turns += 1,
            Ok(Err(error)) => {
                evidence.turn_failure_count += 1;
                if error.to_ascii_lowercase().contains("out of memory") {
                    evidence.oom_count += 1;
                }
                eprintln!("soak turn failed: {error}");
            }
            Err(_) => {
                evidence.turn_failure_count += 1;
                eprintln!("soak turn exceeded {} seconds", TURN_TIMEOUT.as_secs());
            }
        }
        match runtime.poll() {
            Ok(status) if worker_failed(&status) => evidence.worker_failure_count += 1,
            Err(error) => {
                evidence.worker_failure_count += 1;
                eprintln!("worker poll failed during soak: {error}");
            }
            _ => {}
        }
    }
    evidence.duration_minutes = round3(started.elapsed().as_secs_f64() / 60.0);
    evidence
}

async fn run_full_turn(audio: &RecordedAudio, backend: PreferredTtsBackend) -> Result<(), String> {
    let transcription = transcribe(audio).await?;
    let client = LlmClient::new("http://127.0.0.1:18080").map_err(|error| error.to_string())?;
    let (events, mut receiver) = mpsc::channel(128);
    let task = tokio::spawn(async move {
        client
            .stream_reply(
                vec![ChatMessage {
                    role: "user".to_owned(),
                    content: format!("Reply in one short sentence: {}", transcription.text),
                }],
                CancellationToken::new(),
                events,
            )
            .await
            .map_err(|error| error.to_string())
    });
    let mut answer = String::new();
    while let Some(event) = receiver.recv().await {
        if let LlmEvent::Completed(text) = event {
            answer = text;
            break;
        }
    }
    task.await.map_err(|error| error.to_string())??;
    if answer.trim().is_empty() {
        return Err("soak LLM response was empty".to_owned());
    }
    let (events, mut receiver) = mpsc::channel(32);
    let task = spawn_tts(backend, answer, CancellationToken::new(), events)?;
    let mut samples = 0;
    while let Some(event) = receiver.recv().await {
        match event {
            TtsEvent::Pcm48KhzMono(chunk) => samples += chunk.len(),
            TtsEvent::Completed => break,
        }
    }
    task.await.map_err(|error| error.to_string())??;
    if samples == 0 {
        return Err("soak TTS response contained no PCM".to_owned());
    }
    Ok(())
}

fn load_recorded_audio(path: &Path) -> Result<RecordedAudio, String> {
    let mut reader = hound::WavReader::open(path).map_err(|error| error.to_string())?;
    let spec = reader.spec();
    if spec.sample_rate != ASR_SAMPLE_RATE as u32 || spec.channels != 1 {
        return Err(format!(
            "prerecorded audio must be mono 16 kHz, got {} channels at {} Hz",
            spec.channels, spec.sample_rate
        ));
    }
    let samples = reader
        .samples::<i16>()
        .map(|sample| sample.map(|value| value as f32 / i16::MAX as f32))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    let last_active_sample = samples
        .iter()
        .rposition(|sample| sample.abs() >= 0.01)
        .ok_or_else(|| {
            "prerecorded audio contains no samples above the speech threshold".to_owned()
        })?;
    Ok(RecordedAudio {
        path: path.to_path_buf(),
        samples,
        last_active_sample,
    })
}

fn query_gpu_memory_mib() -> Option<f64> {
    let output = Command::new("nvidia-smi")
        .args([
            "--query-gpu=memory.used",
            "--format=csv,noheader,nounits",
            "--id=0",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()?
        .lines()
        .next()?
        .trim()
        .parse()
        .ok()
}

fn round3(value: f64) -> f64 {
    (value * 1_000.0).round() / 1_000.0
}
