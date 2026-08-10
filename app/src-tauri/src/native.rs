use fasttalk_runtime::{
    LoopbackHealthProbe, RestartPolicy, SupervisorError, WorkerSpec, WorkerStatus, WorkerSupervisor,
};
use serde::Serialize;
use std::ffi::OsString;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const LLM_PORT: u16 = 18_080;
const SPEECH_PORT: u16 = 18_081;
const KOKORO_PORT: u16 = 18_082;
const MAX_WARMED_VRAM_MIB: u64 = 23_040;
const MEASURED_COMBINED_WORKER_VRAM_MIB: u64 = 19_584;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PreferredTtsBackend {
    Magpie,
    Kokoro,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VramAdmission {
    pub current_used_mib: Option<u64>,
    pub projected_warmed_mib: Option<u64>,
    pub limit_mib: u64,
    pub backend: PreferredTtsBackend,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeRuntimeStatus {
    pub llm: Option<WorkerStatus>,
    pub speech: Option<WorkerStatus>,
    pub kokoro: Option<WorkerStatus>,
    pub tts_backend: PreferredTtsBackend,
    pub vram_admission: VramAdmission,
}

pub struct NativeRuntime {
    root: PathBuf,
    models: NativeModelPaths,
    llm: Option<WorkerSupervisor>,
    speech: Option<WorkerSupervisor>,
    kokoro: Option<WorkerSupervisor>,
    vram_admission: VramAdmission,
}

#[derive(Clone, Debug)]
pub struct NativeModelPaths {
    pub qwen: PathBuf,
    pub asr: PathBuf,
    pub magpie: PathBuf,
    pub nanocodec: PathBuf,
    pub magpie_tokenizer: PathBuf,
    pub kokoro: PathBuf,
}

impl NativeRuntime {
    pub fn for_development_checkout() -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let models = NativeModelPaths {
            qwen: root.join(".cache/models/qwen3.6-27b/Qwen3.6-27B-Q4_K_M.gguf"),
            asr: root.join(".cache/models/nemotron-asr/nemotron-3.5-asr-streaming-0.6b.q8_0.gguf"),
            magpie: root
                .join(".cache/models/magpie-tts/magpie_tts_multilingual_357m.v2602.f16.gguf"),
            nanocodec: root.join(
                ".cache/models/nano-codec/nemo_nano_codec_22khz_1.89kbps_21.5fps.decoder.f16.gguf",
            ),
            magpie_tokenizer: root.join(".cache/models/magpie-tts/extracted"),
            kokoro: root.join(".cache/models/kokoro-sherpa-v1.0"),
        };
        Self {
            root,
            models,
            llm: None,
            speech: None,
            kokoro: None,
            vram_admission: fallback_admission("GPU memory has not been measured yet"),
        }
    }

    pub fn configure_models(&mut self, models: NativeModelPaths) -> Result<(), SupervisorError> {
        if self.llm.is_some() || self.speech.is_some() || self.kokoro.is_some() {
            return Err(SupervisorError::InvalidSpec(
                "cannot change model paths while native workers are running".to_owned(),
            ));
        }
        self.models = models;
        Ok(())
    }

    pub fn start(&mut self) -> Result<NativeRuntimeStatus, SupervisorError> {
        if self.llm.is_some() || self.speech.is_some() || self.kokoro.is_some() {
            return self.poll();
        }
        self.vram_admission = admit_vram();
        let mut llm = self.build_llm()?;
        let mut speech = self.build_speech()?;
        let mut kokoro = self.build_kokoro()?;
        llm.start()?;
        if let Err(error) = speech.start() {
            let _ = llm.stop();
            return Err(error);
        }
        if let Err(error) = kokoro.start() {
            let _ = speech.stop();
            let _ = llm.stop();
            return Err(error);
        }
        self.llm = Some(llm);
        self.speech = Some(speech);
        self.kokoro = Some(kokoro);
        self.poll()
    }

    pub fn poll(&mut self) -> Result<NativeRuntimeStatus, SupervisorError> {
        Ok(NativeRuntimeStatus {
            llm: self.llm.as_mut().map(WorkerSupervisor::poll).transpose()?,
            speech: self
                .speech
                .as_mut()
                .map(WorkerSupervisor::poll)
                .transpose()?,
            kokoro: self
                .kokoro
                .as_mut()
                .map(WorkerSupervisor::poll)
                .transpose()?,
            tts_backend: self.vram_admission.backend,
            vram_admission: self.vram_admission.clone(),
        })
    }

    pub fn stop(&mut self) -> Result<NativeRuntimeStatus, SupervisorError> {
        if let Some(worker) = self.kokoro.as_mut() {
            worker.stop()?;
        }
        if let Some(worker) = self.speech.as_mut() {
            worker.stop()?;
        }
        if let Some(worker) = self.llm.as_mut() {
            worker.stop()?;
        }
        let status = NativeRuntimeStatus {
            llm: self.llm.as_ref().map(WorkerSupervisor::status),
            speech: self.speech.as_ref().map(WorkerSupervisor::status),
            kokoro: self.kokoro.as_ref().map(WorkerSupervisor::status),
            tts_backend: self.vram_admission.backend,
            vram_admission: self.vram_admission.clone(),
        };
        self.llm = None;
        self.speech = None;
        self.kokoro = None;
        Ok(status)
    }

    fn build_llm(&self) -> Result<WorkerSupervisor, SupervisorError> {
        let executable = self.root.join("runtime/llm/llama-server.exe");
        let model = &self.models.qwen;
        let arguments = os_arguments([
            "--model",
            path_text(model)?,
            "--ctx-size",
            "16384",
            "--gpu-layers",
            "all",
            "--flash-attn",
            "on",
            "--reasoning",
            "off",
            "--host",
            "127.0.0.1",
            "--port",
            "18080",
            "--no-webui",
            "--metrics",
        ]);
        WorkerSupervisor::new(
            WorkerSpec {
                id: "llm".to_owned(),
                working_directory: executable
                    .parent()
                    .ok_or_else(|| SupervisorError::InvalidSpec("invalid LLM path".to_owned()))?
                    .to_path_buf(),
                executable,
                arguments,
                environment: worker_environment(&self.root),
                endpoint_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
            },
            Box::new(
                LoopbackHealthProbe::new(
                    SocketAddr::from((Ipv4Addr::LOCALHOST, LLM_PORT)),
                    "/health",
                    Duration::from_millis(200),
                )
                .map_err(SupervisorError::InvalidSpec)?,
            ),
            RestartPolicy::default(),
        )
    }

    fn build_speech(&self) -> Result<WorkerSupervisor, SupervisorError> {
        let executable = self.root.join("runtime/asr/nemo-speech.exe");
        let asr = &self.models.asr;
        let tts = &self.models.magpie;
        let codec = &self.models.nanocodec;
        let tokenizer = &self.models.magpie_tokenizer;
        let mut arguments = os_arguments([
            "serve",
            "--asr-model",
            path_text(asr)?,
            "--host",
            "127.0.0.1",
            "--port",
            "18081",
            "--no-ui",
        ]);
        if self.vram_admission.backend == PreferredTtsBackend::Magpie {
            arguments.extend(os_arguments([
                "--tts-model",
                path_text(tts)?,
                "--codec-model",
                path_text(codec)?,
                "--tokenizer-dir",
                path_text(tokenizer)?,
            ]));
        }
        WorkerSupervisor::new(
            WorkerSpec {
                id: "speech".to_owned(),
                working_directory: executable
                    .parent()
                    .ok_or_else(|| SupervisorError::InvalidSpec("invalid speech path".to_owned()))?
                    .to_path_buf(),
                executable,
                arguments,
                environment: worker_environment(&self.root),
                endpoint_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
            },
            Box::new(
                LoopbackHealthProbe::new(
                    SocketAddr::from((Ipv4Addr::LOCALHOST, SPEECH_PORT)),
                    "/ready",
                    Duration::from_millis(200),
                )
                .map_err(SupervisorError::InvalidSpec)?,
            ),
            RestartPolicy::default(),
        )
    }

    fn build_kokoro(&self) -> Result<WorkerSupervisor, SupervisorError> {
        let executable = self.root.join("runtime/tts/kokoro-worker.exe");
        let model_dir = &self.models.kokoro;
        let arguments = os_arguments([
            "--model-dir",
            path_text(model_dir)?,
            "--host",
            "127.0.0.1",
            "--port",
            "18082",
            "--threads",
            "4",
        ]);
        WorkerSupervisor::new(
            WorkerSpec {
                id: "kokoro".to_owned(),
                working_directory: executable
                    .parent()
                    .ok_or_else(|| SupervisorError::InvalidSpec("invalid Kokoro path".to_owned()))?
                    .to_path_buf(),
                executable,
                arguments,
                environment: Vec::new(),
                endpoint_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
            },
            Box::new(
                LoopbackHealthProbe::new(
                    SocketAddr::from((Ipv4Addr::LOCALHOST, KOKORO_PORT)),
                    "/ready",
                    Duration::from_millis(200),
                )
                .map_err(SupervisorError::InvalidSpec)?,
            ),
            RestartPolicy::default(),
        )
    }
}

fn admit_vram() -> VramAdmission {
    admission_for(query_used_vram_mib())
}

fn admission_for(current_used_mib: Option<u64>) -> VramAdmission {
    let Some(current_used_mib) = current_used_mib else {
        return fallback_admission(
            "NVIDIA memory usage could not be measured, so GPU TTS was disabled",
        );
    };
    let projected_warmed_mib = current_used_mib.saturating_add(MEASURED_COMBINED_WORKER_VRAM_MIB);
    if projected_warmed_mib <= MAX_WARMED_VRAM_MIB {
        VramAdmission {
            current_used_mib: Some(current_used_mib),
            projected_warmed_mib: Some(projected_warmed_mib),
            limit_mib: MAX_WARMED_VRAM_MIB,
            backend: PreferredTtsBackend::Magpie,
            reason: "measured desktop usage leaves enough room for warmed Magpie TTS".to_owned(),
        }
    } else {
        VramAdmission {
            current_used_mib: Some(current_used_mib),
            projected_warmed_mib: Some(projected_warmed_mib),
            limit_mib: MAX_WARMED_VRAM_MIB,
            backend: PreferredTtsBackend::Kokoro,
            reason: "projected warmed usage exceeds the GPU memory policy; using CPU TTS"
                .to_owned(),
        }
    }
}

fn fallback_admission(reason: &str) -> VramAdmission {
    VramAdmission {
        current_used_mib: None,
        projected_warmed_mib: None,
        limit_mib: MAX_WARMED_VRAM_MIB,
        backend: PreferredTtsBackend::Kokoro,
        reason: reason.to_owned(),
    }
}

fn query_used_vram_mib() -> Option<u64> {
    let mut command = Command::new("nvidia-smi");
    command.args([
        "--query-gpu=memory.used",
        "--format=csv,noheader,nounits",
        "--id=0",
    ]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let output = command.output().ok()?;
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

fn path_text(path: &Path) -> Result<&str, SupervisorError> {
    path.to_str().ok_or_else(|| {
        SupervisorError::InvalidSpec(format!(
            "runtime path is not valid UTF-8: {}",
            path.display()
        ))
    })
}

fn os_arguments<'a>(arguments: impl IntoIterator<Item = &'a str>) -> Vec<OsString> {
    arguments.into_iter().map(OsString::from).collect()
}

fn worker_environment(root: &Path) -> Vec<(OsString, OsString)> {
    let cuda = root.join("runtime/cuda-13.3");
    let mut path = cuda.into_os_string();
    if let Some(existing) = std::env::var_os("PATH") {
        path.push(";");
        path.push(existing);
    }
    vec![(OsString::from("PATH"), path)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use fasttalk_pipeline::{
        AsrEvent, CancellationToken, ChatMessage, KokoroClient, LlmClient, LlmEvent, MagpieClient,
        RealtimeAsrClient, TtsEvent,
    };
    use fasttalk_runtime::WorkerState;
    use std::time::Instant;
    use tokio::sync::mpsc;

    #[test]
    fn vram_policy_uses_cpu_tts_above_the_measured_limit() {
        let exact = admission_for(Some(
            MAX_WARMED_VRAM_MIB - MEASURED_COMBINED_WORKER_VRAM_MIB,
        ));
        assert_eq!(exact.backend, PreferredTtsBackend::Magpie);
        assert_eq!(exact.projected_warmed_mib, Some(MAX_WARMED_VRAM_MIB));

        let over = admission_for(Some(
            MAX_WARMED_VRAM_MIB - MEASURED_COMBINED_WORKER_VRAM_MIB + 1,
        ));
        assert_eq!(over.backend, PreferredTtsBackend::Kokoro);
        assert_eq!(admission_for(None).backend, PreferredTtsBackend::Kokoro);
    }

    #[test]
    #[ignore = "loads the pinned GPU models and requires the FastTalk hardware profile"]
    fn pinned_native_workers_reach_ready() {
        let mut runtime = NativeRuntime::for_development_checkout();
        let mut status = runtime.start().unwrap();
        let deadline = Instant::now() + Duration::from_secs(240);
        while Instant::now() < deadline {
            if status.llm.as_ref().map(|worker| &worker.state) == Some(&WorkerState::Ready)
                && status.speech.as_ref().map(|worker| &worker.state) == Some(&WorkerState::Ready)
                && status.kokoro.as_ref().map(|worker| &worker.state) == Some(&WorkerState::Ready)
            {
                runtime.stop().unwrap();
                return;
            }
            assert_ne!(
                status.llm.as_ref().map(|worker| &worker.state),
                Some(&WorkerState::Failed)
            );
            assert_ne!(
                status.speech.as_ref().map(|worker| &worker.state),
                Some(&WorkerState::Failed)
            );
            assert_ne!(
                status.kokoro.as_ref().map(|worker| &worker.state),
                Some(&WorkerState::Failed)
            );
            std::thread::sleep(Duration::from_secs(1));
            status = runtime.poll().unwrap();
        }
        panic!("native workers did not become ready before the 240 second deadline: {status:?}");
    }

    #[tokio::test]
    #[ignore = "loads pinned GPU models and exercises live loopback inference"]
    async fn live_pipeline_clients_stream_pinned_models() {
        let mut runtime = NativeRuntime::for_development_checkout();
        wait_until_ready(&mut runtime).await;

        let asr_started = Instant::now();
        let transcript = exercise_asr().await;
        let asr_ms = asr_started.elapsed().as_secs_f64() * 1_000.0;
        assert!(transcript.to_ascii_lowercase().contains("country"));

        let (_, _, llm_cold_first_delta_ms) =
            exercise_llm("Reply with one short sentence confirming that the model is ready.").await;
        let (answer, clauses, llm_warm_first_delta_ms) =
            exercise_llm("Reply with one short sentence about local speech software.").await;
        assert!(!answer.trim().is_empty());
        assert!(!clauses.is_empty());
        assert!(llm_warm_first_delta_ms <= 900.0);

        let tts = MagpieClient::new("http://127.0.0.1:18081").unwrap();
        let (tts_tx, mut tts_rx) = mpsc::channel(64);
        let tts_started = Instant::now();
        let tts_task = tokio::spawn(async move {
            tts.synthesize(
                "FastTalk streams this sentence while it is synthesized.",
                CancellationToken::new(),
                tts_tx,
            )
            .await
        });
        let mut first_pcm_ms = None;
        let mut pcm_samples = 0;
        while let Some(event) = tts_rx.recv().await {
            match event {
                TtsEvent::Pcm48KhzMono(samples) => {
                    first_pcm_ms
                        .get_or_insert_with(|| tts_started.elapsed().as_secs_f64() * 1_000.0);
                    pcm_samples += samples.len();
                }
                TtsEvent::Completed => break,
            }
        }
        tts_task.await.unwrap().unwrap();
        assert!(pcm_samples > 24_000);

        let kokoro = KokoroClient::new("http://127.0.0.1:18082").unwrap();
        let (kokoro_tx, mut kokoro_rx) = mpsc::channel(64);
        let kokoro_started = Instant::now();
        let kokoro_task = tokio::spawn(async move {
            kokoro
                .synthesize(
                    "FastTalk can move speech synthesis to the CPU when GPU memory is constrained.",
                    CancellationToken::new(),
                    kokoro_tx,
                )
                .await
        });
        let mut kokoro_first_pcm_ms = None;
        let mut kokoro_samples = 0;
        while let Some(event) = kokoro_rx.recv().await {
            match event {
                TtsEvent::Pcm48KhzMono(samples) => {
                    kokoro_first_pcm_ms
                        .get_or_insert_with(|| kokoro_started.elapsed().as_secs_f64() * 1_000.0);
                    kokoro_samples += samples.len();
                }
                TtsEvent::Completed => break,
            }
        }
        kokoro_task.await.unwrap().unwrap();
        assert!(kokoro_samples > 24_000);

        println!("asr_final_ms={asr_ms:.3}");
        println!("llm_cold_first_delta_ms={llm_cold_first_delta_ms:.3}");
        println!("llm_warm_first_delta_ms={llm_warm_first_delta_ms:.3}");
        println!("tts_first_pcm_ms={:.3}", first_pcm_ms.unwrap());
        println!("tts_pcm_48khz_samples={pcm_samples}");
        println!("kokoro_first_pcm_ms={:.3}", kokoro_first_pcm_ms.unwrap());
        println!("kokoro_pcm_48khz_samples={kokoro_samples}");
        runtime.stop().unwrap();
    }

    async fn wait_until_ready(runtime: &mut NativeRuntime) {
        let mut status = runtime.start().unwrap();
        let deadline = Instant::now() + Duration::from_secs(240);
        while Instant::now() < deadline {
            if status.llm.as_ref().map(|worker| &worker.state) == Some(&WorkerState::Ready)
                && status.speech.as_ref().map(|worker| &worker.state) == Some(&WorkerState::Ready)
                && status.kokoro.as_ref().map(|worker| &worker.state) == Some(&WorkerState::Ready)
            {
                return;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
            status = runtime.poll().unwrap();
        }
        panic!("native workers did not become ready: {status:?}");
    }

    async fn exercise_asr() -> String {
        let wav = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.cache/sources/nemo-speech.cpp/test_files/asr/wav/test/jfk.wav");
        let mut reader = hound::WavReader::open(wav).unwrap();
        assert_eq!(reader.spec().sample_rate, 16_000);
        assert_eq!(reader.spec().channels, 1);
        let samples = reader
            .samples::<i16>()
            .map(|sample| sample.unwrap() as f32 / 32768.0)
            .collect::<Vec<_>>();
        let client = RealtimeAsrClient::new("ws://127.0.0.1:18081/v1/realtime").unwrap();
        let (mut sender, mut receiver) = client.connect().await.unwrap();
        let receive_task = tokio::spawn(async move {
            let mut final_transcript = None;
            while let Some(event) = receiver.next_event().await {
                match event.unwrap() {
                    AsrEvent::Final(transcript) => final_transcript = Some(transcript),
                    AsrEvent::Committed => return final_transcript.unwrap_or_default(),
                    _ => {}
                }
            }
            final_transcript.unwrap_or_default()
        });
        for chunk in samples.chunks(1_600) {
            sender.send_f32(chunk).await.unwrap();
        }
        sender.commit().await.unwrap();
        let transcript = tokio::time::timeout(Duration::from_secs(30), receive_task)
            .await
            .unwrap()
            .unwrap();
        sender.close().await.unwrap();
        transcript
    }

    async fn exercise_llm(prompt: &str) -> (String, Vec<String>, f64) {
        let llm = LlmClient::new("http://127.0.0.1:18080").unwrap();
        let (events, mut receiver) = mpsc::channel(64);
        let started = Instant::now();
        let prompt = prompt.to_owned();
        let task = tokio::spawn(async move {
            llm.stream_reply(
                vec![ChatMessage {
                    role: "user".to_owned(),
                    content: prompt,
                }],
                CancellationToken::new(),
                events,
            )
            .await
        });
        let mut first_delta_ms = None;
        let mut clauses = Vec::new();
        while let Some(event) = receiver.recv().await {
            match event {
                LlmEvent::Delta(_) => {
                    first_delta_ms.get_or_insert_with(|| started.elapsed().as_secs_f64() * 1_000.0);
                }
                LlmEvent::Clause(clause) => clauses.push(clause),
                LlmEvent::Completed(_) => break,
            }
        }
        let answer = task.await.unwrap().unwrap();
        (answer, clauses, first_delta_ms.unwrap())
    }
}
