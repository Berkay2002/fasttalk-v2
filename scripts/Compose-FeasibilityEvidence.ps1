[CmdletBinding()]
param(
    [string]$Output = "artifacts/feasibility/measured-evidence.json"
)

$ErrorActionPreference = "Stop"
$workspace = Split-Path -Parent $PSScriptRoot
$llama = Get-Content -Raw (Join-Path $workspace "artifacts/feasibility/llama-benchmark.json") | ConvertFrom-Json
$magpie = Get-Content -Raw (Join-Path $workspace "artifacts/feasibility/magpie-benchmark.json") | ConvertFrom-Json
$asr = Get-Content -Raw (Join-Path $workspace "artifacts/feasibility/asr-benchmark.json") | ConvertFrom-Json
$vram = Get-Content -Raw (Join-Path $workspace "artifacts/feasibility/combined-vram.json") | ConvertFrom-Json

$turnLatency = for ($index = 0; $index -lt 20; $index++) {
    [math]::Round($llama.warmLlmFirstTokenMs[$index] + $magpie.firstAudioMs[$index], 3)
}
$asrUpperBound = @(for ($index = 0; $index -lt 20; $index++) {
    [double]$asr.partialUpdateUpperBoundMs
})

$evidence = [ordered]@{
    schemaVersion = 1
    profile = [ordered]@{
        llm = $llama.profile
        asr = $asr.profile
        tts = $magpie.profile
    }
    environment = [ordered]@{
        desktopApplicationsOpen = $true
        networkDisabled = $false
        notes = "Measured locally on the target RTX 3090. Turn latency pairs warm LLM TTFT with Magpie first audio. ASR partial latency is a documented derived upper bound. Barge-in uses HTTP response cancellation, not acoustic playback silence. The 30 minute integrated soak has not run."
    }
    samples = [ordered]@{
        endOfSpeechToFirstAudioMs = @($turnLatency)
        warmLlmFirstTokenMs = @($llama.warmLlmFirstTokenMs)
        generationTokensPerSecond = @($llama.generationTokensPerSecond)
        asrPartialUpdateMs = $asrUpperBound
        bargeInToSilenceMs = @($magpie.responseCancellationMs)
        combinedWarmedVramMiB = @([double]$vram.combinedWorkerGpuMemoryMiB)
        ttsRealTimeFactor = @($magpie.realTimeFactor)
    }
    soak = [ordered]@{
        durationMinutes = 0.0
        oomCount = 0
    }
}

$outputPath = Join-Path $workspace $Output
$evidence | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $outputPath -Encoding utf8
$evidence | ConvertTo-Json -Depth 8
