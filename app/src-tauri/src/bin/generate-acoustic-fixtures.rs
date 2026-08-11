use fasttalk_app_lib::native::{KOKORO_BASE_URL, NativeRuntime, NativeRuntimeStatus};
use fasttalk_pipeline::{CancellationToken, KokoroClient, TtsEvent};
use fasttalk_runtime::WorkerState;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::time::sleep;

const OUTPUT_SAMPLE_RATE: u32 = 16_000;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("fixture generation failed: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let mut runtime = NativeRuntime::for_development_checkout();
    let result = async {
        wait_for_workers(&mut runtime).await?;
        generate().await
    }
    .await;
    let stop_result = runtime.stop().map_err(|error| error.to_string());
    result?;
    stop_result?;
    Ok(())
}

async fn generate() -> Result<(), String> {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = workspace.join("tests/fixtures/audio");
    fs::create_dir_all(&output).map_err(|error| error.to_string())?;

    let quiet = scaled(
        synthesize("Could you please lower the lights in this room?").await?,
        0.16,
    );
    write_fixture(&output.join("quiet-speech.wav"), &with_tail(quiet))?;

    let mut hesitation = synthesize("Um.").await?;
    hesitation.extend(std::iter::repeat_n(0.0, OUTPUT_SAMPLE_RATE as usize / 2));
    hesitation.extend(synthesize("I think we should wait for a moment before deciding.").await?);
    write_fixture(&output.join("hesitation.wav"), &with_tail(hesitation))?;

    write_fixture(
        &output.join("short-acknowledgement.wav"),
        &with_tail(synthesize("Yes.").await?),
    )?;

    let long = synthesize(
        "Could you explain how a local voice assistant keeps the microphone, transcription, language model, and speech synthesis private while still responding quickly?",
    )
    .await?;
    write_fixture(&output.join("long-question.wav"), &with_tail(long))?;

    let noisy = add_deterministic_noise(
        synthesize("Please tell me whether the train will arrive before noon.").await?,
        0.025,
    );
    write_fixture(&output.join("background-noise.wav"), &with_tail(noisy))?;

    let mut foreground = vec![0.0; OUTPUT_SAMPLE_RATE as usize];
    foreground.extend(synthesize("Can you hear my question clearly?").await?);
    let playback = read_wav(
        &workspace.join(".cache/sources/nemo-speech.cpp/test_files/asr/wav/test/jfk.wav"),
    )?;
    let mut playback = playback;
    playback.truncate(foreground.len());
    let mut echo = delayed(&playback, OUTPUT_SAMPLE_RATE as usize * 40 / 1_000);
    echo.truncate(foreground.len());
    let speaker_playback = with_tail(mix(&foreground, &echo, 0.10));
    write_fixture(&output.join("speaker-playback.wav"), &speaker_playback)?;
    let mut playback_reference = playback;
    playback_reference.resize(speaker_playback.len(), 0.0);
    write_fixture(
        &output.join("speaker-playback-reference.wav"),
        &playback_reference,
    )?;

    println!("wrote acoustic fixtures to {}", output.display());
    Ok(())
}

async fn wait_for_workers(runtime: &mut NativeRuntime) -> Result<NativeRuntimeStatus, String> {
    let deadline = Instant::now() + Duration::from_secs(300);
    let mut status = runtime.start().map_err(|error| error.to_string())?;
    loop {
        let ready = [&status.llm, &status.speech, &status.kokoro]
            .into_iter()
            .flatten()
            .all(|worker| worker.state == WorkerState::Ready);
        if ready {
            return Ok(status);
        }
        if [&status.llm, &status.speech, &status.kokoro]
            .into_iter()
            .flatten()
            .any(|worker| worker.state == WorkerState::Failed)
        {
            return Err(format!("native worker failed while starting: {status:?}"));
        }
        if Instant::now() >= deadline {
            return Err("native workers were not ready after 300 seconds".to_owned());
        }
        sleep(Duration::from_secs(1)).await;
        status = runtime.poll().map_err(|error| error.to_string())?;
    }
}

async fn synthesize(text: &str) -> Result<Vec<f32>, String> {
    let client = KokoroClient::new(KOKORO_BASE_URL).map_err(|error| error.to_string())?;
    let (sender, mut receiver) = mpsc::channel(32);
    let input = text.to_owned();
    let task = tokio::spawn(async move {
        client
            .synthesize(&input, CancellationToken::new(), sender)
            .await
            .map_err(|error| error.to_string())
    });
    let mut samples_48khz = Vec::new();
    while let Some(event) = receiver.recv().await {
        match event {
            TtsEvent::Pcm48KhzMono(samples) => samples_48khz.extend(samples),
            TtsEvent::Completed => break,
        }
    }
    task.await.map_err(|error| error.to_string())??;
    if samples_48khz.is_empty() {
        return Err(format!("Kokoro returned no audio for {text:?}"));
    }
    Ok(samples_48khz
        .chunks_exact(3)
        .map(|chunk| (chunk[0] + chunk[1] + chunk[2]) / 3.0)
        .collect())
}

fn with_tail(mut samples: Vec<f32>) -> Vec<f32> {
    samples.extend(std::iter::repeat_n(
        0.0,
        OUTPUT_SAMPLE_RATE as usize * 2 / 5,
    ));
    samples
}

fn scaled(mut samples: Vec<f32>, gain: f32) -> Vec<f32> {
    for sample in &mut samples {
        *sample *= gain;
    }
    samples
}

fn add_deterministic_noise(mut samples: Vec<f32>, amplitude: f32) -> Vec<f32> {
    let mut state = 0x7a31_94d2_u32;
    for sample in &mut samples {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let noise = ((state >> 8) as f32 / 0x00ff_ffff as f32) * 2.0 - 1.0;
        *sample = (*sample + noise * amplitude).clamp(-1.0, 1.0);
    }
    samples
}

fn mix(foreground: &[f32], background: &[f32], background_gain: f32) -> Vec<f32> {
    let length = foreground.len().max(background.len());
    (0..length)
        .map(|index| {
            let foreground = foreground.get(index).copied().unwrap_or_default();
            let background = background.get(index).copied().unwrap_or_default();
            (foreground + background * background_gain).clamp(-1.0, 1.0)
        })
        .collect()
}

fn delayed(samples: &[f32], delay_samples: usize) -> Vec<f32> {
    std::iter::repeat_n(0.0, delay_samples)
        .chain(samples.iter().copied())
        .collect()
}

fn read_wav(path: &Path) -> Result<Vec<f32>, String> {
    let mut reader = hound::WavReader::open(path).map_err(|error| error.to_string())?;
    let spec = reader.spec();
    if spec.channels != 1 || spec.sample_rate != OUTPUT_SAMPLE_RATE {
        return Err(format!(
            "fixture source must be mono 16 kHz: {}",
            path.display()
        ));
    }
    reader
        .samples::<i16>()
        .map(|sample| {
            sample
                .map(|sample| sample as f32 / 32768.0)
                .map_err(|error| error.to_string())
        })
        .collect()
}

fn write_fixture(path: &PathBuf, samples: &[f32]) -> Result<(), String> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: OUTPUT_SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec).map_err(|error| error.to_string())?;
    for sample in samples {
        let pcm = (sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16;
        writer
            .write_sample(pcm)
            .map_err(|error| error.to_string())?;
    }
    writer.finalize().map_err(|error| error.to_string())
}
