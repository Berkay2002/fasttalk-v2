use fasttalk_app_lib::native::{
    KOKORO_BASE_URL, LLM_BASE_URL, NativeRuntime, NativeRuntimeStatus, PreferredTtsBackend,
    SPEECH_BASE_URL, SPEECH_REALTIME_URL,
};
use fasttalk_audio::{AudioConfig, AudioEngine, CaptureProcessor, DEVICE_FRAME_SAMPLES};
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
const ACTIVITY_FRAME_SAMPLES: usize = ASR_SAMPLE_RATE / 100;
const ACTIVITY_RMS_THRESHOLD: f32 = 0.015;
const READY_TIMEOUT: Duration = Duration::from_secs(300);
const TURN_TIMEOUT: Duration = Duration::from_secs(90);

#[derive(Debug)]
struct Args {
    turns: usize,
    soak_minutes: f64,
    audio: Option<PathBuf>,
    reference: Option<PathBuf>,
    output: PathBuf,
    skip_audio: bool,
}

impl Args {
    fn parse() -> Result<Self, String> {
        let mut turns: usize = 20;
        let mut soak_minutes: f64 = 0.0;
        let mut audio = None;
        let mut reference = None;
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
                "--audio" => audio = Some(PathBuf::from(value(&mut arguments, "--audio")?)),
                "--reference" => {
                    reference = Some(PathBuf::from(value(&mut arguments, "--reference")?));
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
            audio,
            reference,
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
    first_active_sample: usize,
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
    synthesized_clause_count: usize,
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

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct CancellationEvidence {
    llm_after_first_delta_ms: f64,
    tts_after_first_pcm_ms: f64,
    scope: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ConversationEvidence {
    schema_version: u32,
    prerecorded_audio: String,
    turns: usize,
    runtime_profile: String,
    tts_backend: PreferredTtsBackend,
    transcripts: Vec<String>,
    first_clauses: Vec<String>,
    synthesized_clause_counts: Vec<usize>,
    end_of_speech_to_first_audio_ms: Vec<f64>,
    warm_llm_first_token_ms: Vec<f64>,
    asr_partial_update_ms: Vec<f64>,
    barge_in_to_silence_ms: Vec<f64>,
    barge_in_measurement_scope: String,
    capture_preprocessing_scope: String,
    warmed_gpu_memory_mib: Vec<f64>,
    active_stream_cancellation: CancellationEvidence,
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
    let audio_path = args.audio.as_ref().map_or_else(
        || workspace.join(".cache/sources/nemo-speech.cpp/test_files/asr/wav/test/jfk.wav"),
        |path| {
            if path.is_absolute() {
                path.clone()
            } else {
                workspace.join(path)
            }
        },
    );
    let reference_path = args.reference.as_ref().map(|path| {
        if path.is_absolute() {
            path.clone()
        } else {
            workspace.join(path)
        }
    });
    let mut runtime = NativeRuntime::for_development_checkout();
    let vad_model_path = workspace.join(&runtime.profile().vad.legacy_path);
    let captured_audio = load_recorded_audio(&audio_path)?;
    let reference_audio = reference_path
        .as_deref()
        .map(load_recorded_audio)
        .transpose()?;
    let audio =
        preprocess_captured_audio(&captured_audio, reference_audio.as_ref(), &vad_model_path)?;
    let result = run_with_runtime(
        &args,
        &audio,
        &captured_audio,
        &vad_model_path,
        &mut runtime,
    )
    .await;
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
    captured_audio: &RecordedAudio,
    vad_model_path: &Path,
    runtime: &mut NativeRuntime,
) -> Result<(), String> {
    let status = wait_for_workers(runtime).await?;
    let mut evidence = ConversationEvidence {
        schema_version: 1,
        prerecorded_audio: audio.path.display().to_string(),
        turns: args.turns,
        runtime_profile: status.profile_id.clone(),
        tts_backend: status.tts_backend,
        transcripts: Vec::with_capacity(args.turns),
        first_clauses: Vec::with_capacity(args.turns),
        synthesized_clause_counts: Vec::with_capacity(args.turns),
        end_of_speech_to_first_audio_ms: Vec::with_capacity(args.turns),
        warm_llm_first_token_ms: Vec::with_capacity(args.turns),
        asr_partial_update_ms: Vec::new(),
        barge_in_to_silence_ms: Vec::new(),
        barge_in_measurement_scope: "prerecorded speech onset through production AEC and Silero VAD, cancellation dispatch, and WASAPI output callback acknowledgement".to_owned(),
        capture_preprocessing_scope: if args.reference.is_some() {
            "prerecorded capture and paired playback reference are upsampled to 48 kHz and passed through the production AEC and Silero frame processor before ASR injection"
        } else {
            "prerecorded capture is upsampled to 48 kHz and passed through the production AEC and Silero frame processor before ASR injection"
        }
        .to_owned(),
        warmed_gpu_memory_mib: Vec::new(),
        active_stream_cancellation: CancellationEvidence::default(),
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
            .synthesized_clause_counts
            .push(measurement.synthesized_clause_count);
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
        evidence.barge_in_to_silence_ms = measure_barge_in(
            captured_audio,
            &playback_fixture,
            vad_model_path,
            args.turns,
        )?;
    }
    evidence.active_stream_cancellation =
        measure_active_stream_cancellation(status.tts_backend).await?;
    let post_cancellation_status = runtime.poll().map_err(|error| error.to_string())?;
    if !workers_ready(&post_cancellation_status) {
        return Err(format!(
            "native workers were not ready after cancellation probes: {post_cancellation_status:?}"
        ));
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

async fn measure_active_stream_cancellation(
    backend: PreferredTtsBackend,
) -> Result<CancellationEvidence, String> {
    let llm = LlmClient::new(LLM_BASE_URL).map_err(|error| error.to_string())?;
    let llm_cancellation = CancellationToken::new();
    let (llm_events, mut llm_receiver) = mpsc::channel(128);
    let llm_task = tokio::spawn({
        let cancellation = llm_cancellation.clone();
        async move {
            llm.stream_reply(
                vec![ChatMessage {
                    role: "user".to_owned(),
                    content: "Write a detailed 400-word explanation of local speech pipelines."
                        .to_owned(),
                }],
                cancellation,
                llm_events,
            )
            .await
        }
    });
    loop {
        match timeout(Duration::from_secs(10), llm_receiver.recv()).await {
            Ok(Some(LlmEvent::Delta(_))) => break,
            Ok(Some(_)) => {}
            Ok(None) => {
                return Err("LLM cancellation probe ended before the first delta".to_owned());
            }
            Err(_) => {
                return Err("LLM cancellation probe emitted no delta within 10 seconds".to_owned());
            }
        }
    }
    let llm_cancel_started = Instant::now();
    llm_cancellation.cancel();
    let llm_result = timeout(Duration::from_secs(2), llm_task)
        .await
        .map_err(|_| "active LLM stream did not cancel within 2 seconds".to_owned())?
        .map_err(|error| format!("LLM cancellation task failed: {error}"))?;
    if !matches!(llm_result, Err(fasttalk_pipeline::PipelineError::Cancelled)) {
        return Err(format!(
            "active LLM stream returned an unexpected cancellation result: {llm_result:?}"
        ));
    }
    let llm_after_first_delta_ms = round3(llm_cancel_started.elapsed().as_secs_f64() * 1_000.0);

    let tts_cancellation = CancellationToken::new();
    let (tts_events, mut tts_receiver) = mpsc::channel(128);
    let text = "This long cancellation probe keeps local speech synthesis active long enough to interrupt it after its first audio frame. ".repeat(12);
    let tts_task = match backend {
        PreferredTtsBackend::Magpie => {
            let client = MagpieClient::new(SPEECH_BASE_URL).map_err(|error| error.to_string())?;
            let cancellation = tts_cancellation.clone();
            tokio::spawn(async move { client.synthesize(&text, cancellation, tts_events).await })
        }
        PreferredTtsBackend::Kokoro => {
            let client = KokoroClient::new(KOKORO_BASE_URL).map_err(|error| error.to_string())?;
            let cancellation = tts_cancellation.clone();
            tokio::spawn(async move { client.synthesize(&text, cancellation, tts_events).await })
        }
    };
    loop {
        match timeout(Duration::from_secs(30), tts_receiver.recv()).await {
            Ok(Some(TtsEvent::Pcm48KhzMono(samples))) if !samples.is_empty() => break,
            Ok(Some(_)) => {}
            Ok(None) => return Err("TTS cancellation probe ended before the first PCM".to_owned()),
            Err(_) => {
                return Err("TTS cancellation probe emitted no PCM within 30 seconds".to_owned());
            }
        }
    }
    let tts_cancel_started = Instant::now();
    tts_cancellation.cancel();
    let tts_result = timeout(Duration::from_secs(2), tts_task)
        .await
        .map_err(|_| "active TTS stream did not cancel within 2 seconds".to_owned())?
        .map_err(|error| format!("TTS cancellation task failed: {error}"))?;
    if !matches!(tts_result, Err(fasttalk_pipeline::PipelineError::Cancelled)) {
        return Err(format!(
            "active TTS stream returned an unexpected cancellation result: {tts_result:?}"
        ));
    }
    Ok(CancellationEvidence {
        llm_after_first_delta_ms,
        tts_after_first_pcm_ms: round3(tts_cancel_started.elapsed().as_secs_f64() * 1_000.0),
        scope: "client cancellation after the first live LLM delta and first live TTS PCM frame; workers must remain ready"
            .to_owned(),
    })
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
    let llm = LlmClient::new(LLM_BASE_URL).map_err(|error| error.to_string())?;
    let (llm_tx, mut llm_rx) = mpsc::channel(128);
    let llm_started = Instant::now();
    let cancellation = CancellationToken::new();
    let llm_task = tokio::spawn({
        let prompt = format!(
            "Return exactly these two sentences and no other text: \"I heard the transcript clearly. I will answer it locally.\" The transcript was: {}",
            transcription.text
        );
        let cancellation = cancellation.clone();
        async move {
            llm.stream_reply(
                vec![ChatMessage {
                    role: "user".to_owned(),
                    content: prompt,
                }],
                cancellation,
                llm_tx,
            )
            .await
        }
    });

    let (clauses, mut clause_receiver) = mpsc::channel(8);
    let generation = async {
        let mut first_token_ms = None;
        let mut answer = None;
        while let Some(event) = llm_rx.recv().await {
            match event {
                LlmEvent::Delta(_) => {
                    first_token_ms
                        .get_or_insert_with(|| llm_started.elapsed().as_secs_f64() * 1_000.0);
                }
                LlmEvent::Clause(clause) => clauses
                    .send(clause)
                    .await
                    .map_err(|_| "clause synthesizer stopped early".to_owned())?,
                LlmEvent::Completed(completed) => {
                    answer = Some(completed);
                    break;
                }
            }
        }
        drop(clauses);
        let generated = llm_task
            .await
            .map_err(|error| format!("LLM task failed: {error}"))?
            .map_err(|error| format!("LLM stream failed: {error}"))?;
        let answer = answer.unwrap_or(generated);
        if answer.trim().is_empty() {
            return Err("LLM completed without speech text".to_owned());
        }
        Ok::<_, String>((answer, first_token_ms))
    };

    let speech = async {
        let mut first_clause = None;
        let mut first_audio = None;
        let mut first_audio_at = None;
        let mut synthesized_clause_count = 0;
        while let Some(clause) = clause_receiver.recv().await {
            first_clause.get_or_insert_with(|| clause.clone());
            let (tts_tx, mut tts_rx) = mpsc::channel(32);
            let tts_task = spawn_tts(backend, clause, cancellation.clone(), tts_tx)?;
            let mut clause_samples = 0;
            while let Some(event) = tts_rx.recv().await {
                match event {
                    TtsEvent::Pcm48KhzMono(samples) if !samples.is_empty() => {
                        clause_samples += samples.len();
                        if first_audio.is_none() {
                            first_audio_at = Some(Instant::now());
                            first_audio = Some(samples);
                        }
                    }
                    TtsEvent::Pcm48KhzMono(_) => {}
                    TtsEvent::Completed => break,
                }
            }
            tts_task
                .await
                .map_err(|error| format!("TTS task failed: {error}"))??;
            if clause_samples == 0 {
                return Err("TTS completed a clause without PCM".to_owned());
            }
            synthesized_clause_count += 1;
        }
        Ok::<_, String>((
            first_clause.ok_or_else(|| "LLM emitted no clause".to_owned())?,
            first_audio.ok_or_else(|| "TTS completed without PCM".to_owned())?,
            first_audio_at.ok_or_else(|| "TTS emitted no timed PCM".to_owned())?,
            synthesized_clause_count,
        ))
    };

    let result = tokio::try_join!(generation, speech);
    if result.is_err() {
        cancellation.cancel();
    }
    let ((_, first_token_ms), (first_clause, first_audio, first_audio_at, clause_count)) = result?;
    if clause_count < 2 {
        return Err(format!(
            "streaming proof requires at least two synthesized clauses, got {clause_count}"
        ));
    }

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
        synthesized_clause_count: clause_count,
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
            let client = MagpieClient::new(SPEECH_BASE_URL).map_err(|error| error.to_string())?;
            Ok(tokio::spawn(async move {
                client
                    .synthesize(&text, cancellation, events)
                    .await
                    .map_err(|error| error.to_string())
            }))
        }
        PreferredTtsBackend::Kokoro => {
            let client = KokoroClient::new(KOKORO_BASE_URL).map_err(|error| error.to_string())?;
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
    let client = RealtimeAsrClient::new(SPEECH_REALTIME_URL).map_err(|error| error.to_string())?;
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

fn measure_barge_in(
    speech: &RecordedAudio,
    playback: &[f32],
    vad_model_path: &Path,
    repetitions: usize,
) -> Result<Vec<f64>, String> {
    if playback.is_empty() {
        return Err("barge-in fixture contains no PCM".to_owned());
    }
    let mut engine = AudioEngine::start(AudioConfig {
        vad_model_path: Some(vad_model_path.to_path_buf()),
        ..AudioConfig::default()
    })
    .map_err(|error| error.to_string())?;
    let mut fixture = Vec::with_capacity(96_000);
    while fixture.len() < 96_000 {
        fixture.extend_from_slice(playback);
    }
    fixture.truncate(96_000);
    let mut measurements = Vec::with_capacity(repetitions);
    for _ in 0..repetitions {
        let mut processor = CaptureProcessor::new(40, Some(vad_model_path))?;
        let accepted = engine.queue_playback_partial(&fixture);
        if accepted < 24_000 {
            return Err(format!("audio queue accepted only {accepted} samples"));
        }
        std::thread::sleep(Duration::from_millis(100));
        let pre_roll = ASR_SAMPLE_RATE / 10;
        let start_sample = speech.first_active_sample.saturating_sub(pre_roll);
        let mut speech_onset = None;
        let mut reference_offset = 0;
        let cancellation = CancellationToken::new();
        let mut detected = false;
        for source_start in (start_sample..speech.samples.len()).step_by(ASR_SAMPLE_RATE / 100) {
            let source_end = (source_start + ASR_SAMPLE_RATE / 100).min(speech.samples.len());
            if speech_onset.is_none()
                && (source_start..source_end).contains(&speech.first_active_sample)
            {
                speech_onset = Some(Instant::now());
            }
            let mut reference = [0.0; DEVICE_FRAME_SAMPLES];
            let mut capture = [0.0; DEVICE_FRAME_SAMPLES];
            for index in 0..DEVICE_FRAME_SAMPLES {
                reference[index] = fixture[(reference_offset + index) % fixture.len()];
                let source_index = source_start + index / 3;
                let foreground = speech
                    .samples
                    .get(source_index)
                    .copied()
                    .unwrap_or_default();
                capture[index] = foreground + reference[index] * 0.15;
            }
            reference_offset = (reference_offset + DEVICE_FRAME_SAMPLES) % fixture.len();
            let frame = processor.process(&capture, &reference)?;
            if frame.speech_active {
                cancellation.cancel();
                engine.cancel_playback();
                detected = true;
                break;
            }
            if speech_onset.is_some() {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        if !detected || !cancellation.is_cancelled() {
            return Err("Silero did not detect the prerecorded barge-in speech".to_owned());
        }
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if engine.status().last_cancel_to_callback_ms.is_some() {
                let onset = speech_onset
                    .ok_or_else(|| "barge-in speech onset was not observed".to_owned())?;
                measurements.push(round3(onset.elapsed().as_secs_f64() * 1_000.0));
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
    measure_turn(audio, backend).await.map(|_| ())
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
    let (first_active_sample, last_active_sample) = active_speech_bounds(&samples)
        .ok_or_else(|| "prerecorded audio contains no active speech frames".to_owned())?;
    Ok(RecordedAudio {
        path: path.to_path_buf(),
        samples,
        first_active_sample,
        last_active_sample,
    })
}

fn preprocess_captured_audio(
    audio: &RecordedAudio,
    reference_audio: Option<&RecordedAudio>,
    vad_model_path: &Path,
) -> Result<RecordedAudio, String> {
    let mut processor = CaptureProcessor::new(40, Some(vad_model_path))?;
    let mut samples = Vec::with_capacity(audio.samples.len());
    let mut speech_seen = false;
    for (frame_index, source) in audio.samples.chunks(ACTIVITY_FRAME_SAMPLES).enumerate() {
        let mut capture = [0.0; DEVICE_FRAME_SAMPLES];
        let mut reference = [0.0; DEVICE_FRAME_SAMPLES];
        for (index, sample) in capture.iter_mut().enumerate() {
            *sample = source.get(index / 3).copied().unwrap_or_default();
            let reference_index = frame_index * ACTIVITY_FRAME_SAMPLES + index / 3;
            reference[index] = reference_audio
                .and_then(|audio| audio.samples.get(reference_index))
                .copied()
                .unwrap_or_default();
        }
        let frame = processor.process(&capture, &reference)?;
        speech_seen |= frame.speech_active;
        samples.extend_from_slice(&frame.asr_samples[..source.len()]);
    }
    if !speech_seen {
        return Err("Silero did not detect speech in the prerecorded capture".to_owned());
    }
    let (first_active_sample, last_active_sample) = active_speech_bounds(&samples)
        .ok_or_else(|| "processed capture contains no active speech frames".to_owned())?;
    Ok(RecordedAudio {
        path: audio.path.clone(),
        samples,
        first_active_sample,
        last_active_sample,
    })
}

fn active_speech_bounds(samples: &[f32]) -> Option<(usize, usize)> {
    let active_frames = samples
        .chunks(ACTIVITY_FRAME_SAMPLES)
        .enumerate()
        .filter(|(_, frame)| frame_rms(frame) >= ACTIVITY_RMS_THRESHOLD)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let first_active_frame = active_frames.first().copied()?;
    let last_active_frame = active_frames.last().copied()?;
    let first_active_sample = first_active_frame * ACTIVITY_FRAME_SAMPLES;
    let last_active_sample = ((last_active_frame + 1) * ACTIVITY_FRAME_SAMPLES)
        .min(samples.len())
        .saturating_sub(1);
    Some((first_active_sample, last_active_sample))
}

fn frame_rms(samples: &[f32]) -> f32 {
    (samples.iter().map(|sample| sample * sample).sum::<f32>() / samples.len().max(1) as f32).sqrt()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_bounds_ignore_low_level_preroll_noise() {
        let mut samples = vec![0.01; ACTIVITY_FRAME_SAMPLES * 3];
        samples.extend(vec![0.1; ACTIVITY_FRAME_SAMPLES * 2]);
        samples.extend(vec![0.0; ACTIVITY_FRAME_SAMPLES]);
        assert_eq!(
            active_speech_bounds(&samples),
            Some((ACTIVITY_FRAME_SAMPLES * 3, ACTIVITY_FRAME_SAMPLES * 5 - 1))
        );
    }
}
