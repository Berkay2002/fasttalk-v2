use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    pub schema_version: u32,
    pub source_artifact: SourceArtifact,
    pub target: Target,
    pub sources: Vec<Source>,
    pub runtimes: Vec<Runtime>,
    pub models: Vec<Model>,
    pub evidence: EvidenceRequirements,
    pub gates: Gates,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceArtifact {
    pub path: PathBuf,
    pub sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Target {
    pub minimum_windows_build: u32,
    pub gpu_name: String,
    pub minimum_gpu_memory_mi_b: f64,
    pub compute_capability: String,
    pub cuda_toolkit_version: String,
    pub cmake_version_prefix: String,
    pub ninja_version_prefix: String,
    pub rust_target: String,
}

#[derive(Debug, Deserialize)]
pub struct Source {
    pub id: String,
    pub remote: String,
    pub revision: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Runtime {
    pub id: String,
    pub environment_variable: String,
    pub default_path: PathBuf,
    pub file_name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    pub id: String,
    pub repository: String,
    pub revision: String,
    pub environment_variable: String,
    pub file_name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceRequirements {
    pub minimum_warm_turn_samples: usize,
    pub minimum_generation_samples: usize,
    pub minimum_asr_partial_samples: usize,
    pub minimum_barge_in_samples: usize,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Gates {
    pub end_of_speech_to_first_audio_p50_ms: f64,
    pub end_of_speech_to_first_audio_p95_ms: f64,
    pub warm_llm_first_token_p95_ms: f64,
    pub minimum_generation_tokens_per_second: f64,
    pub maximum_asr_partial_update_ms: f64,
    pub maximum_barge_in_to_silence_ms: f64,
    pub maximum_combined_warmed_vram_mi_b: f64,
    pub minimum_soak_minutes: f64,
    pub maximum_soak_oom_count: u32,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Pass,
    Fail,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Check {
    pub id: String,
    pub category: String,
    pub status: CheckStatus,
    pub expected: String,
    pub actual: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
}

impl Check {
    fn new(
        id: impl Into<String>,
        category: impl Into<String>,
        pass: bool,
        expected: impl Into<String>,
        actual: impl Into<String>,
        remediation: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            category: category.into(),
            status: if pass {
                CheckStatus::Pass
            } else {
                CheckStatus::Fail
            },
            expected: expected.into(),
            actual: actual.into(),
            remediation: (!pass).then(|| remediation.into()),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreflightReport {
    pub schema_version: u32,
    pub captured_at_unix_ms: u128,
    pub pass: bool,
    pub checks: Vec<Check>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BenchmarkEvidence {
    pub schema_version: u32,
    pub profile: Profile,
    pub environment: BenchmarkEnvironment,
    pub samples: Samples,
    pub soak: Soak,
}

#[derive(Debug, Deserialize)]
pub struct Profile {
    pub llm: String,
    pub asr: String,
    pub tts: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BenchmarkEnvironment {
    pub desktop_applications_open: bool,
    pub network_disabled: bool,
    pub notes: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Samples {
    pub end_of_speech_to_first_audio_ms: Vec<f64>,
    pub warm_llm_first_token_ms: Vec<f64>,
    pub generation_tokens_per_second: Vec<f64>,
    pub asr_partial_update_ms: Vec<f64>,
    pub barge_in_to_silence_ms: Vec<f64>,
    pub combined_warmed_vram_mi_b: Vec<f64>,
    pub tts_real_time_factor: Vec<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Soak {
    pub duration_minutes: f64,
    pub oom_count: u32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BenchmarkReport {
    pub schema_version: u32,
    pub pass: bool,
    pub checks: Vec<Check>,
    pub observations: BTreeMap<String, f64>,
}

pub fn load_config(path: &Path) -> Result<Config, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("could not read {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("could not parse {}: {error}", path.display()))
}

pub fn load_evidence(path: &Path) -> Result<BenchmarkEvidence, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("could not read {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("could not parse {}: {error}", path.display()))
}

pub fn run_preflight(config: &Config, root: &Path) -> PreflightReport {
    let mut checks = Vec::new();

    checks.push(check_artifact(config, root));
    checks.push(check_windows(config));
    checks.extend(check_gpu(config));
    checks.push(check_cuda(config));
    checks.push(check_version_command(
        "tool.cmake",
        "toolchain",
        "cmake",
        &["--version"],
        &config.target.cmake_version_prefix,
        "Install the plan-pinned CMake 3.30.x toolchain.",
    ));
    checks.push(check_version_command(
        "tool.ninja",
        "toolchain",
        "ninja",
        &["--version"],
        &config.target.ninja_version_prefix,
        "Install the plan-pinned Ninja 1.13.x toolchain.",
    ));
    checks.push(check_rust(config));
    checks.push(check_visual_studio());

    for runtime in &config.runtimes {
        checks.push(check_runtime(runtime, root));
    }
    for model in &config.models {
        checks.push(check_model(model));
    }

    PreflightReport {
        schema_version: config.schema_version,
        captured_at_unix_ms: now_unix_ms(),
        pass: checks.iter().all(|check| check.status == CheckStatus::Pass),
        checks,
    }
}

fn check_artifact(config: &Config, root: &Path) -> Check {
    let path = root.join(&config.source_artifact.path);
    match sha256_file(&path) {
        Ok(actual) => Check::new(
            "source.artifact",
            "source",
            actual.eq_ignore_ascii_case(&config.source_artifact.sha256),
            config.source_artifact.sha256.clone(),
            actual,
            "Restore the exact htmlpub source artifact before changing implementation decisions.",
        ),
        Err(error) => Check::new(
            "source.artifact",
            "source",
            false,
            config.source_artifact.sha256.clone(),
            error,
            "Restore the exact htmlpub source artifact before changing implementation decisions.",
        ),
    }
}

fn check_windows(config: &Config) -> Check {
    let output = run(
        "powershell.exe",
        &[
            "-NoProfile",
            "-Command",
            "(Get-CimInstance Win32_OperatingSystem).BuildNumber",
        ],
    );
    let actual = output_text(&output);
    let build = actual.trim().parse::<u32>().ok();
    Check::new(
        "os.windows",
        "platform",
        output.as_ref().is_ok_and(Output::status_success)
            && build.is_some_and(|value| value >= config.target.minimum_windows_build),
        format!("Windows build >= {}", config.target.minimum_windows_build),
        actual,
        "Run the feasibility spike on the target Windows 11 machine.",
    )
}

fn check_gpu(config: &Config) -> Vec<Check> {
    let output = run(
        "nvidia-smi",
        &[
            "--query-gpu=name,driver_version,memory.total,memory.free,compute_cap",
            "--format=csv,noheader,nounits",
        ],
    );
    let actual = output_text(&output);
    let fields = actual
        .lines()
        .next()
        .map(|line| line.split(',').map(str::trim).collect::<Vec<_>>());
    let parsed = fields.filter(|values| values.len() == 5);

    if let Some(values) = parsed {
        let memory = values[2].parse::<f64>().ok();
        vec![
            Check::new(
                "gpu.model",
                "hardware",
                values[0] == config.target.gpu_name,
                config.target.gpu_name.clone(),
                values[0],
                "Run benchmarks on the plan's RTX 3090 target.",
            ),
            Check::new(
                "gpu.memory",
                "hardware",
                memory.is_some_and(|value| value >= config.target.minimum_gpu_memory_mi_b),
                format!(">= {} MiB", config.target.minimum_gpu_memory_mi_b),
                format!("{} MiB total, {} MiB free", values[2], values[3]),
                "Run benchmarks on a 24 GB RTX 3090 and close only abnormal GPU workloads.",
            ),
            Check::new(
                "gpu.compute",
                "hardware",
                values[4] == config.target.compute_capability,
                config.target.compute_capability.clone(),
                values[4],
                "Use the sm_86 RTX 3090 target required by the build plan.",
            ),
        ]
    } else {
        vec![Check::new(
            "gpu.probe",
            "hardware",
            false,
            "one NVIDIA GPU record",
            actual,
            "Install a compatible NVIDIA driver and ensure nvidia-smi is available.",
        )]
    }
}

fn check_cuda(config: &Config) -> Check {
    let version_dir = format!("v{}", config.target.cuda_toolkit_version);
    let pinned = Path::new(r"C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA")
        .join(version_dir)
        .join("bin")
        .join("nvcc.exe");
    let command = pinned.to_string_lossy().into_owned();
    let output = run(&command, &["--version"]);
    let actual = output_text(&output);
    let expected_fragment = format!("release {}", config.target.cuda_toolkit_version);
    Check::new(
        "tool.cuda",
        "toolchain",
        output.as_ref().is_ok_and(Output::status_success) && actual.contains(&expected_fragment),
        format!("CUDA Toolkit {}", config.target.cuda_toolkit_version),
        actual,
        format!(
            "Install CUDA Toolkit {} and keep its nvcc at {}.",
            config.target.cuda_toolkit_version,
            pinned.display()
        ),
    )
}

fn check_version_command(
    id: &str,
    category: &str,
    command: &str,
    args: &[&str],
    version_prefix: &str,
    remediation: &str,
) -> Check {
    let output = run(command, args);
    let actual = output_text(&output);
    Check::new(
        id,
        category,
        output.as_ref().is_ok_and(Output::status_success) && actual.contains(version_prefix),
        format!("version {version_prefix}x"),
        actual,
        remediation,
    )
}

fn check_rust(config: &Config) -> Check {
    let output = run("rustc", &["-Vv"]);
    let actual = output_text(&output);
    Check::new(
        "tool.rust",
        "toolchain",
        output.as_ref().is_ok_and(Output::status_success)
            && actual.contains("release: ")
            && actual.contains(&format!("host: {}", config.target.rust_target)),
        format!("latest stable Rust on {}", config.target.rust_target),
        actual,
        "Update stable Rust with rustup and select the x86_64 MSVC host.",
    )
}

fn check_visual_studio() -> Check {
    let vswhere = r"C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe";
    let output = run(
        vswhere,
        &[
            "-latest",
            "-products",
            "*",
            "-requires",
            "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
            "-property",
            "installationPath",
        ],
    );
    let actual = output_text(&output);
    Check::new(
        "tool.visual-studio",
        "toolchain",
        output.as_ref().is_ok_and(Output::status_success) && !actual.trim().is_empty(),
        "Visual Studio 2022 v143 C++ Build Tools",
        actual,
        "Install the Visual Studio 2022 Desktop development with C++ workload.",
    )
}

fn check_runtime(runtime: &Runtime, root: &Path) -> Check {
    let value = env::var_os(&runtime.environment_variable)
        .map(PathBuf::from)
        .or_else(|| Some(root.join(&runtime.default_path)));
    let pass = value.as_ref().is_some_and(|path| {
        path.is_file()
            && path
                .file_name()
                .is_some_and(|name| name.eq_ignore_ascii_case(&runtime.file_name))
    });
    let actual = value
        .as_ref()
        .map_or_else(|| "not set".to_owned(), |path| path.display().to_string());
    Check::new(
        format!("runtime.{}", runtime.id),
        "runtime",
        pass,
        runtime.file_name.clone(),
        actual,
        format!(
            "Build the pinned {} source and set {} to the executable path.",
            runtime.id, runtime.environment_variable
        ),
    )
}

fn check_model(model: &Model) -> Check {
    let value = env::var_os(&model.environment_variable).map(PathBuf::from);
    let pass = value.as_ref().is_some_and(|path| {
        path.is_file()
            && model.file_name.as_ref().is_none_or(|file_name| {
                path.file_name()
                    .is_some_and(|name| name.eq_ignore_ascii_case(file_name))
            })
    });
    let actual = value
        .as_ref()
        .map_or_else(|| "not set".to_owned(), |path| path.display().to_string());
    let expected = model
        .file_name
        .clone()
        .unwrap_or_else(|| format!("model file from {}", model.repository));
    Check::new(
        format!("model.{}", model.id),
        "model",
        pass,
        expected,
        actual,
        format!(
            "Fetch revision {} from {} and set {} to its local model file.",
            model.revision, model.repository, model.environment_variable
        ),
    )
}

pub fn evaluate(config: &Config, evidence: &BenchmarkEvidence) -> BenchmarkReport {
    let mut checks = Vec::new();
    let mut observations = BTreeMap::new();

    checks.push(Check::new(
        "evidence.schema",
        "evidence",
        evidence.schema_version == config.schema_version,
        config.schema_version.to_string(),
        evidence.schema_version.to_string(),
        "Regenerate the evidence file from the current template.",
    ));
    checks.push(Check::new(
        "environment.desktop-applications",
        "evidence",
        evidence.environment.desktop_applications_open,
        "normal desktop applications open",
        evidence.environment.desktop_applications_open.to_string(),
        "Repeat the warmed benchmark under a representative desktop load.",
    ));

    add_upper_percentile_checks(
        &mut checks,
        &mut observations,
        "latency.end-of-speech-to-first-audio",
        &evidence.samples.end_of_speech_to_first_audio_ms,
        config.evidence.minimum_warm_turn_samples,
        &[
            (
                "p50",
                0.50,
                config.gates.end_of_speech_to_first_audio_p50_ms,
            ),
            (
                "p95",
                0.95,
                config.gates.end_of_speech_to_first_audio_p95_ms,
            ),
        ],
    );
    add_upper_percentile_checks(
        &mut checks,
        &mut observations,
        "latency.warm-llm-first-token",
        &evidence.samples.warm_llm_first_token_ms,
        config.evidence.minimum_warm_turn_samples,
        &[("p95", 0.95, config.gates.warm_llm_first_token_p95_ms)],
    );
    add_lower_min_check(
        &mut checks,
        &mut observations,
        "throughput.llm-generation",
        &evidence.samples.generation_tokens_per_second,
        config.evidence.minimum_generation_samples,
        config.gates.minimum_generation_tokens_per_second,
    );
    add_upper_max_check(
        &mut checks,
        &mut observations,
        "latency.asr-partial-update",
        &evidence.samples.asr_partial_update_ms,
        config.evidence.minimum_asr_partial_samples,
        config.gates.maximum_asr_partial_update_ms,
    );
    add_upper_max_check(
        &mut checks,
        &mut observations,
        "latency.barge-in-to-silence",
        &evidence.samples.barge_in_to_silence_ms,
        config.evidence.minimum_barge_in_samples,
        config.gates.maximum_barge_in_to_silence_ms,
    );
    add_upper_max_check(
        &mut checks,
        &mut observations,
        "memory.combined-warmed-vram",
        &evidence.samples.combined_warmed_vram_mi_b,
        1,
        config.gates.maximum_combined_warmed_vram_mi_b,
    );
    checks.push(Check::new(
        "soak.duration",
        "benchmark",
        evidence.soak.duration_minutes >= config.gates.minimum_soak_minutes,
        format!(">= {} minutes", config.gates.minimum_soak_minutes),
        format!("{} minutes", evidence.soak.duration_minutes),
        "Run the combined conversation soak for the full required duration.",
    ));
    checks.push(Check::new(
        "soak.oom",
        "benchmark",
        evidence.soak.oom_count <= config.gates.maximum_soak_oom_count,
        format!("<= {} OOM events", config.gates.maximum_soak_oom_count),
        format!("{} OOM events", evidence.soak.oom_count),
        "Reject this profile or reduce its memory use before continuing.",
    ));

    if let Some(value) = finite_max(&evidence.samples.tts_real_time_factor) {
        observations.insert("ttsRealTimeFactorMax".to_owned(), value);
    }

    BenchmarkReport {
        schema_version: config.schema_version,
        pass: checks.iter().all(|check| check.status == CheckStatus::Pass),
        checks,
        observations,
    }
}

fn add_upper_percentile_checks(
    checks: &mut Vec<Check>,
    observations: &mut BTreeMap<String, f64>,
    id: &str,
    samples: &[f64],
    minimum_samples: usize,
    percentiles: &[(&str, f64, f64)],
) {
    let enough = valid_sample_count(samples) >= minimum_samples;
    for &(label, percentile_value, maximum) in percentiles {
        let value = percentile(samples, percentile_value);
        if let Some(value) = value {
            observations.insert(format!("{id}.{label}"), value);
        }
        checks.push(Check::new(
            format!("{id}.{label}"),
            "benchmark",
            enough && value.is_some_and(|value| value <= maximum),
            format!("<= {maximum} with >= {minimum_samples} samples"),
            value.map_or_else(|| format!("{} valid samples", valid_sample_count(samples)), |value| format!("{value} from {} valid samples", valid_sample_count(samples))),
            "Collect a complete warmed sample set and optimize or reject the profile if it remains over the gate.",
        ));
    }
}

fn add_lower_min_check(
    checks: &mut Vec<Check>,
    observations: &mut BTreeMap<String, f64>,
    id: &str,
    samples: &[f64],
    minimum_samples: usize,
    minimum: f64,
) {
    let count = valid_sample_count(samples);
    let value = finite_min(samples);
    if let Some(value) = value {
        observations.insert(format!("{id}.min"), value);
    }
    checks.push(Check::new(
        id,
        "benchmark",
        count >= minimum_samples && value.is_some_and(|value| value >= minimum),
        format!(">= {minimum} for every sample with >= {minimum_samples} samples"),
        value.map_or_else(|| format!("{count} valid samples"), |value| format!("minimum {value} from {count} valid samples")),
        "Collect the required warmed samples and reject the profile if its minimum throughput remains below the gate.",
    ));
}

fn add_upper_max_check(
    checks: &mut Vec<Check>,
    observations: &mut BTreeMap<String, f64>,
    id: &str,
    samples: &[f64],
    minimum_samples: usize,
    maximum: f64,
) {
    let count = valid_sample_count(samples);
    let value = finite_max(samples);
    if let Some(value) = value {
        observations.insert(format!("{id}.max"), value);
    }
    checks.push(Check::new(
        id,
        "benchmark",
        count >= minimum_samples && value.is_some_and(|value| value <= maximum),
        format!("<= {maximum} for every sample with >= {minimum_samples} samples"),
        value.map_or_else(|| format!("{count} valid samples"), |value| format!("maximum {value} from {count} valid samples")),
        "Collect the required warmed samples and optimize or reject the profile if its maximum remains over the gate.",
    ));
}

pub fn percentile(samples: &[f64], percentile: f64) -> Option<f64> {
    let mut values = samples
        .iter()
        .copied()
        .filter(|value| value.is_finite())
        .collect::<Vec<_>>();
    if values.is_empty() || !(0.0..=1.0).contains(&percentile) {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let rank = ((percentile * values.len() as f64).ceil() as usize).max(1) - 1;
    values.get(rank).copied()
}

fn valid_sample_count(samples: &[f64]) -> usize {
    samples.iter().filter(|value| value.is_finite()).count()
}

fn finite_min(samples: &[f64]) -> Option<f64> {
    samples
        .iter()
        .copied()
        .filter(|value| value.is_finite())
        .min_by(f64::total_cmp)
}

fn finite_max(samples: &[f64]) -> Option<f64> {
    samples
        .iter()
        .copied()
        .filter(|value| value.is_finite())
        .max_by(f64::total_cmp)
}

fn run(command: &str, args: &[&str]) -> Result<Output, String> {
    Command::new(command)
        .args(args)
        .output()
        .map_err(|error| format!("{command}: {error}"))
}

trait OutputStatus {
    fn status_success(&self) -> bool;
}

impl OutputStatus for Output {
    fn status_success(&self) -> bool {
        self.status.success()
    }
}

fn output_text(output: &Result<Output, String>) -> String {
    match output {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            format!("{stdout}{stderr}").trim().to_owned()
        }
        Err(error) => error.clone(),
    }
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("could not read {}: {error}", path.display()))?;
    let digest = Sha256::digest(bytes);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn now_unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> Config {
        serde_json::from_str(include_str!("../../../config/feasibility.json"))
            .expect("config should parse")
    }

    fn passing_evidence() -> BenchmarkEvidence {
        BenchmarkEvidence {
            schema_version: 1,
            profile: Profile {
                llm: "qwen3.6-27b-q4-k-m".to_owned(),
                asr: "nemotron-3.5-asr-streaming-0.6b-q8".to_owned(),
                tts: "magpie-tts-v2602-f16".to_owned(),
            },
            environment: BenchmarkEnvironment {
                desktop_applications_open: true,
                network_disabled: false,
                notes: String::new(),
            },
            samples: Samples {
                end_of_speech_to_first_audio_ms: vec![1000.0; 20],
                warm_llm_first_token_ms: vec![800.0; 20],
                generation_tokens_per_second: vec![21.0; 5],
                asr_partial_update_ms: vec![240.0; 20],
                barge_in_to_silence_ms: vec![140.0; 20],
                combined_warmed_vram_mi_b: vec![22_000.0],
                tts_real_time_factor: vec![0.4],
            },
            soak: Soak {
                duration_minutes: 30.0,
                oom_count: 0,
            },
        }
    }

    #[test]
    fn config_matches_source_gates() {
        let config = test_config();
        assert_eq!(config.gates.end_of_speech_to_first_audio_p50_ms, 1200.0);
        assert_eq!(config.gates.end_of_speech_to_first_audio_p95_ms, 1800.0);
        assert_eq!(config.gates.maximum_combined_warmed_vram_mi_b, 23_040.0);
        assert_eq!(config.gates.minimum_soak_minutes, 30.0);
    }

    #[test]
    fn nearest_rank_percentile_is_deterministic() {
        let values = (1..=20).map(f64::from).collect::<Vec<_>>();
        assert_eq!(percentile(&values, 0.50), Some(10.0));
        assert_eq!(percentile(&values, 0.95), Some(19.0));
        assert_eq!(percentile(&[], 0.95), None);
    }

    #[test]
    fn exact_gate_values_pass() {
        let config = test_config();
        let report = evaluate(&config, &passing_evidence());
        assert!(report.pass);
        assert!(
            report
                .checks
                .iter()
                .all(|check| check.status == CheckStatus::Pass)
        );
    }

    #[test]
    fn one_slow_barge_in_fails_the_profile() {
        let config = test_config();
        let mut evidence = passing_evidence();
        evidence.samples.barge_in_to_silence_ms[19] = 151.0;
        let report = evaluate(&config, &evidence);
        assert!(!report.pass);
        assert_eq!(
            report
                .checks
                .iter()
                .find(|check| check.id == "latency.barge-in-to-silence")
                .unwrap()
                .status,
            CheckStatus::Fail
        );
    }

    #[test]
    fn incomplete_sample_set_fails_the_profile() {
        let config = test_config();
        let mut evidence = passing_evidence();
        evidence.samples.warm_llm_first_token_ms.truncate(19);
        let report = evaluate(&config, &evidence);
        assert!(!report.pass);
    }
}
