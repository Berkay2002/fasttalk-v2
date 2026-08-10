use fasttalk_runtime::{
    LoopbackHealthProbe, RestartPolicy, SupervisorError, WorkerSpec, WorkerStatus, WorkerSupervisor,
};
use serde::Serialize;
use std::ffi::OsString;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::Duration;

const LLM_PORT: u16 = 18_080;
const SPEECH_PORT: u16 = 18_081;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeRuntimeStatus {
    pub llm: Option<WorkerStatus>,
    pub speech: Option<WorkerStatus>,
}

pub struct NativeRuntime {
    root: PathBuf,
    llm: Option<WorkerSupervisor>,
    speech: Option<WorkerSupervisor>,
}

impl NativeRuntime {
    pub fn for_development_checkout() -> Self {
        Self {
            root: Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."),
            llm: None,
            speech: None,
        }
    }

    pub fn start(&mut self) -> Result<NativeRuntimeStatus, SupervisorError> {
        if self.llm.is_some() || self.speech.is_some() {
            return self.poll();
        }
        let mut llm = self.build_llm()?;
        let mut speech = self.build_speech()?;
        llm.start()?;
        if let Err(error) = speech.start() {
            let _ = llm.stop();
            return Err(error);
        }
        self.llm = Some(llm);
        self.speech = Some(speech);
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
        })
    }

    pub fn stop(&mut self) -> Result<NativeRuntimeStatus, SupervisorError> {
        if let Some(worker) = self.speech.as_mut() {
            worker.stop()?;
        }
        if let Some(worker) = self.llm.as_mut() {
            worker.stop()?;
        }
        let status = NativeRuntimeStatus {
            llm: self.llm.as_ref().map(WorkerSupervisor::status),
            speech: self.speech.as_ref().map(WorkerSupervisor::status),
        };
        self.llm = None;
        self.speech = None;
        Ok(status)
    }

    fn build_llm(&self) -> Result<WorkerSupervisor, SupervisorError> {
        let executable = self.root.join("runtime/llm/llama-server.exe");
        let model = self
            .root
            .join(".cache/models/qwen3.6-27b/Qwen3.6-27B-Q4_K_M.gguf");
        let arguments = os_arguments([
            "--model",
            path_text(&model)?,
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
        let asr = self
            .root
            .join(".cache/models/nemotron-asr/nemotron-3.5-asr-streaming-0.6b.q8_0.gguf");
        let tts = self
            .root
            .join(".cache/models/magpie-tts/magpie_tts_multilingual_357m.v2602.f16.gguf");
        let codec = self.root.join(
            ".cache/models/nano-codec/nemo_nano_codec_22khz_1.89kbps_21.5fps.decoder.f16.gguf",
        );
        let tokenizer = self.root.join(".cache/models/magpie-tts/extracted");
        let arguments = os_arguments([
            "serve",
            "--asr-model",
            path_text(&asr)?,
            "--tts-model",
            path_text(&tts)?,
            "--codec-model",
            path_text(&codec)?,
            "--tokenizer-dir",
            path_text(&tokenizer)?,
            "--host",
            "127.0.0.1",
            "--port",
            "18081",
            "--no-ui",
        ]);
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
    use fasttalk_runtime::WorkerState;
    use std::time::Instant;

    #[test]
    #[ignore = "loads the pinned GPU models and requires the FastTalk hardware profile"]
    fn pinned_native_workers_reach_ready() {
        let mut runtime = NativeRuntime::for_development_checkout();
        let mut status = runtime.start().unwrap();
        let deadline = Instant::now() + Duration::from_secs(240);
        while Instant::now() < deadline {
            if status.llm.as_ref().map(|worker| &worker.state) == Some(&WorkerState::Ready)
                && status.speech.as_ref().map(|worker| &worker.state) == Some(&WorkerState::Ready)
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
            std::thread::sleep(Duration::from_secs(1));
            status = runtime.poll().unwrap();
        }
        panic!("native workers did not become ready before the 240 second deadline: {status:?}");
    }
}
