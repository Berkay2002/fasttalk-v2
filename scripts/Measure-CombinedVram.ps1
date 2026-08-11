[CmdletBinding()]
param(
    [string]$LlamaModel = ".cache/models/qwen3.6-27b/Qwen3.6-27B-Q4_K_M.gguf",
    [string]$Profile = "qwen3.6-27b-q4-k-m-16k-non-thinking-with-speech",
    [string]$Output = "artifacts/feasibility/combined-vram.json",
    [int]$ContextSize = 16384,
    [int]$Parallel = 4,
    [string]$AsrModel = ".cache/models/nemotron-asr/nemotron-3.5-asr-streaming-0.6b.q8_0.gguf",
    [string]$TtsModel = ".cache/models/magpie-tts/magpie_tts_multilingual_357m.v2602.f16.gguf",
    [string]$CodecModel = ".cache/models/nano-codec/nemo_nano_codec_22khz_1.89kbps_21.5fps.decoder.f16.gguf",
    [string]$TokenizerDirectory = ".cache/models/magpie-tts/extracted",
    [int]$LlamaPort = 18080,
    [int]$SpeechPort = 18081,
    [switch]$AsrOnly
)

$ErrorActionPreference = "Stop"
$workspace = Split-Path -Parent $PSScriptRoot
$artifactDir = Join-Path $workspace "artifacts/feasibility"
$outputPath = Join-Path $workspace $Output
$outputDir = Split-Path -Parent $outputPath
New-Item -ItemType Directory -Force -Path $outputDir | Out-Null
$llamaModelPath = (Resolve-Path (Join-Path $workspace $LlamaModel)).Path
$asrModelPath = (Resolve-Path (Join-Path $workspace $AsrModel)).Path
$env:Path = "$(Join-Path $workspace 'runtime/cuda-13.3');$env:Path"

function Get-GpuMemoryMiB {
    return [double](nvidia-smi --query-gpu=memory.used --format=csv,noheader,nounits | Select-Object -First 1)
}

function Wait-Endpoint([string]$Uri, [System.Diagnostics.Process]$Process, [string]$Log) {
    for ($attempt = 0; $attempt -lt 120; $attempt++) {
        if ($Process.HasExited) {
            throw "Worker exited $($Process.ExitCode): $(Get-Content -Raw $Log)"
        }
        try {
            $response = Invoke-RestMethod $Uri -TimeoutSec 2
            if ($response.status -in @("ok", "ready")) { return $response }
            if ($response.ready -eq $true) { return $response }
        } catch {
            # Readiness failures are expected during model loading.
        }
        Start-Sleep -Seconds 1
    }
    throw "Readiness timed out for $Uri."
}

$baseline = Get-GpuMemoryMiB
$llamaLog = Join-Path $artifactDir "combined-llama.stderr.log"
$speechLog = Join-Path $artifactDir "combined-speech.stderr.log"
$llama = Start-Process -FilePath (Join-Path $workspace "runtime/llm/llama-server.exe") `
    -ArgumentList @(
        "--model", $llamaModelPath,
        "--ctx-size", $ContextSize.ToString(), "--parallel", $Parallel.ToString(),
        "--gpu-layers", "all", "--flash-attn", "on",
        "--reasoning", "off", "--host", "127.0.0.1", "--port", $LlamaPort.ToString(),
        "--no-webui"
    ) -PassThru -WindowStyle Hidden -RedirectStandardError $llamaLog `
    -RedirectStandardOutput (Join-Path $artifactDir "combined-llama.stdout.log")
$speech = $null

try {
    $null = Wait-Endpoint "http://127.0.0.1:$LlamaPort/health" $llama $llamaLog
    $warmup = @{
        model = "qwen3.6-27b"
        messages = @(@{ role = "user"; content = "Reply with exactly: ready" })
        max_tokens = 8
        temperature = 0
    } | ConvertTo-Json -Depth 5
    Invoke-RestMethod "http://127.0.0.1:$LlamaPort/v1/chat/completions" `
        -Method Post -ContentType "application/json" -Body $warmup -TimeoutSec 120 | Out-Null

    $speechArguments = @(
        "serve",
        "--asr-model", $asrModelPath,
        "--host", "127.0.0.1", "--port", $SpeechPort.ToString(), "--no-ui"
    )
    if (-not $AsrOnly) {
        $speechArguments += @(
            "--tts-model", (Resolve-Path (Join-Path $workspace $TtsModel)).Path,
            "--codec-model", (Resolve-Path (Join-Path $workspace $CodecModel)).Path,
            "--tokenizer-dir", (Resolve-Path (Join-Path $workspace $TokenizerDirectory)).Path
        )
    }
    $speech = Start-Process -FilePath (Join-Path $workspace "runtime/asr/nemo-speech.exe") `
        -ArgumentList $speechArguments -PassThru -WindowStyle Hidden -RedirectStandardError $speechLog `
        -RedirectStandardOutput (Join-Path $artifactDir "combined-speech.stdout.log")
    $ready = Wait-Endpoint "http://127.0.0.1:$SpeechPort/ready" $speech $speechLog
    $warmed = Get-GpuMemoryMiB

    $report = [ordered]@{
        schemaVersion = 1
        profile = $Profile
        llamaModel = [IO.Path]::GetRelativePath($workspace, $llamaModelPath).Replace('\', '/')
        contextSize = $ContextSize
        parallel = $Parallel
        asrModel = [IO.Path]::GetRelativePath($workspace, $asrModelPath).Replace('\', '/')
        speechProfile = if ($AsrOnly) { "asr-only" } else { "asr-and-magpie" }
        baselineGpuMemoryMiB = $baseline
        warmedGpuMemoryMiB = $warmed
        combinedWorkerGpuMemoryMiB = $warmed - $baseline
        speechReady = $ready
    }
    $report | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $outputPath -Encoding utf8
    $report | ConvertTo-Json -Depth 8
} finally {
    if ($speech -and -not $speech.HasExited) { $speech.Kill($true) }
    if (-not $llama.HasExited) { $llama.Kill($true) }
    if ($speech) { $speech.WaitForExit() }
    $llama.WaitForExit()
}
