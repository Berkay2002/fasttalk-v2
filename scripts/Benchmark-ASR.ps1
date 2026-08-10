[CmdletBinding()]
param(
    [string]$Output = "artifacts/feasibility/asr-benchmark.json",
    [int]$Repetitions = 20
)

$ErrorActionPreference = "Stop"
$workspace = Split-Path -Parent $PSScriptRoot
$outputPath = Join-Path $workspace $Output
$artifactDir = Split-Path -Parent $outputPath
New-Item -ItemType Directory -Force -Path $artifactDir | Out-Null
$env:Path = "$(Join-Path $workspace 'runtime/cuda-13.3');$env:Path"
$executable = Join-Path $workspace "runtime/asr/nemo-speech.exe"
$wav = Join-Path $workspace ".cache/sources/nemo-speech.cpp/test_files/asr/wav/test/jfk.wav"
$model = Join-Path $workspace ".cache/models/nemotron-asr/nemotron-3.5-asr-streaming-0.6b.q8_0.gguf"
$stderr = Join-Path $artifactDir "asr-benchmark.stderr.log"

$json = & $executable bench asr $wav --model $model --concurrency 1 `
    --repetitions $Repetitions --warmup 1 --mode stream --device cuda:0 --json `
    2> $stderr | Out-String
if ($LASTEXITCODE -ne 0) { throw "ASR benchmark failed ($LASTEXITCODE): $(Get-Content -Raw $stderr)" }
$raw = $json | ConvertFrom-Json
$run = $raw.runs[0]
$chunkMs = 160.0
$chunks = $run.audio_seconds / ($chunkMs / 1000.0)
$computeMsPerChunk = ($run.wall_seconds * 1000.0) / $chunks
$upperBoundMs = $chunkMs + $computeMsPerChunk

$report = [ordered]@{
    schemaVersion = 1
    profile = "nemotron-3.5-asr-streaming-0.6b-q8"
    repetitions = $Repetitions
    transcriptMismatches = $run.transcript_mismatches
    streamingRealTimeFactorX = [math]::Round($run.rtfx, 4)
    configuredChunkMs = $chunkMs
    computeMsPerChunk = [math]::Round($computeMsPerChunk, 4)
    partialUpdateUpperBoundMs = [math]::Round($upperBoundMs, 4)
    partialUpdateMethod = "Configured 160 ms streaming chunk plus measured average compute per chunk. This is a derived upper bound, not a WebSocket timestamp."
}
$report | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $outputPath -Encoding utf8
$report | ConvertTo-Json -Depth 5
