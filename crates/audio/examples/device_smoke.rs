use fasttalk_audio::{AudioConfig, AudioEngine};
use std::time::{Duration, Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut audio = AudioEngine::start(AudioConfig::default())?;
    audio.queue_playback(&vec![0.0; 48_000])?;

    let deadline = Instant::now() + Duration::from_secs(2);
    let mut processed_samples = 0;
    let mut samples = [0.0; 1_600];
    while Instant::now() < deadline {
        processed_samples += audio.read_asr_samples(&mut samples);
        std::thread::sleep(Duration::from_millis(10));
    }

    audio.cancel_playback();
    std::thread::sleep(Duration::from_millis(200));
    let status = audio.status();
    println!("input_device={}", status.input_device);
    println!("output_device={}", status.output_device);
    println!("processed_asr_samples={processed_samples}");
    println!(
        "cancel_to_callback_ms={}",
        status
            .last_cancel_to_callback_ms
            .map_or_else(|| "unobserved".to_owned(), |value| format!("{value:.3}"))
    );
    println!("dropped_capture_samples={}", status.dropped_capture_samples);
    println!(
        "dropped_playback_samples={}",
        status.dropped_playback_samples
    );
    println!("dropped_asr_samples={}", status.dropped_asr_samples);
    println!(
        "last_error={}",
        status.last_error.as_deref().unwrap_or_default()
    );

    if processed_samples < 16_000 {
        return Err("fewer than one second of processed capture samples".into());
    }
    if status.last_cancel_to_callback_ms.is_none() {
        return Err("output callback did not observe cancellation".into());
    }
    if status.last_error.is_some() {
        return Err("audio stream reported an error".into());
    }
    Ok(())
}
