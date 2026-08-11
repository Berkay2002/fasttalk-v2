[CmdletBinding()]
param(
    [string]$Output = "artifacts/feasibility/measured-evidence.json",
    [string]$LlamaPath = "artifacts/feasibility/llama-benchmark.json",
    [string]$ConversationPath = "artifacts/release/conversation-benchmark.json",
    [string]$SoakPath = $ConversationPath,
    [switch]$NetworkDisabled
)

$ErrorActionPreference = "Stop"
$workspace = Split-Path -Parent $PSScriptRoot
$resolvedLlamaPath = Join-Path $workspace $LlamaPath
if (-not (Test-Path -LiteralPath $resolvedLlamaPath -PathType Leaf)) {
    throw "LLM benchmark evidence is missing: $resolvedLlamaPath"
}
$llama = Get-Content -Raw $resolvedLlamaPath | ConvertFrom-Json
$magpie = Get-Content -Raw (Join-Path $workspace "artifacts/feasibility/magpie-benchmark.json") | ConvertFrom-Json
$asr = Get-Content -Raw (Join-Path $workspace "artifacts/feasibility/asr-benchmark.json") | ConvertFrom-Json
$resolvedConversationPath = Join-Path $workspace $ConversationPath
if (-not (Test-Path -LiteralPath $resolvedConversationPath -PathType Leaf)) {
    throw "Integrated conversation evidence is missing: $resolvedConversationPath"
}
$conversationEvidence = Get-Content -Raw $resolvedConversationPath | ConvertFrom-Json
$resolvedSoakPath = Join-Path $workspace $SoakPath
if (-not (Test-Path -LiteralPath $resolvedSoakPath -PathType Leaf)) {
    throw "Soak evidence is missing: $resolvedSoakPath"
}
$soakEvidence = Get-Content -Raw $resolvedSoakPath | ConvertFrom-Json

$evidence = [ordered]@{
    schemaVersion = 1
    profile = [ordered]@{
        llm = $llama.profile
        asr = $asr.profile
        tts = if ($conversationEvidence.ttsBackend -eq "magpie") { $magpie.profile } else { "kokoro-82m-int8-cpu" }
    }
    environment = [ordered]@{
        desktopApplicationsOpen = $true
        networkDisabled = [bool]$NetworkDisabled
        notes = "Integrated measurements use the pinned prerecorded JFK WAV over the production ASR WebSocket, streamed LLM-to-TTS handoff, and prerecorded speech onset through production AEC, Silero VAD, cancellation dispatch, and WASAPI output-callback acknowledgement."
    }
    samples = [ordered]@{
        endOfSpeechToFirstAudioMs = @($conversationEvidence.endOfSpeechToFirstAudioMs)
        warmLlmFirstTokenMs = @($conversationEvidence.warmLlmFirstTokenMs)
        generationTokensPerSecond = @($llama.generationTokensPerSecond)
        asrPartialUpdateMs = @($conversationEvidence.asrPartialUpdateMs)
        bargeInToSilenceMs = @($conversationEvidence.bargeInToSilenceMs)
        combinedWarmedVramMiB = @($conversationEvidence.warmedGpuMemoryMib)
        ttsRealTimeFactor = @($magpie.realTimeFactor)
    }
    soak = [ordered]@{
        durationMinutes = [double]$soakEvidence.soak.durationMinutes
        completedTurns = [int]$soakEvidence.soak.completedTurns
        turnFailureCount = [int]$soakEvidence.soak.turnFailureCount
        oomCount = [int]$soakEvidence.soak.oomCount
        workerFailureCount = [int]$soakEvidence.soak.workerFailureCount
    }
}

$outputPath = Join-Path $workspace $Output
$evidence | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $outputPath -Encoding utf8
$evidence | ConvertTo-Json -Depth 8
