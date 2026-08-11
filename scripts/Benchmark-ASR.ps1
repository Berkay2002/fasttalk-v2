[CmdletBinding()]
param(
    [string]$Output = "artifacts/feasibility/asr-benchmark.json",
    [string]$Model = ".cache/models/nemotron-asr/nemotron-3.5-asr-streaming-0.6b.q8_0.gguf",
    [string]$Profile = "nemotron-3.5-asr-streaming-0.6b-q8",
    [ValidateSet("stream", "offline")]
    [string]$Mode = "stream",
    [double]$ChunkMs = 160.0,
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
$modelPath = (Resolve-Path (Join-Path $workspace $Model)).Path
$stderr = Join-Path $artifactDir "asr-benchmark.stderr.log"

$json = & $executable bench asr $wav --model $modelPath --concurrency 1 `
    --repetitions $Repetitions --warmup 1 --mode $Mode --device cuda:0 --json `
    2> $stderr | Out-String
if ($LASTEXITCODE -ne 0) { throw "ASR benchmark failed ($LASTEXITCODE): $(Get-Content -Raw $stderr)" }
$raw = $json | ConvertFrom-Json
$run = $raw.runs[0]
$chunks = $run.audio_seconds / ($ChunkMs / 1000.0)
$computeMsPerChunk = ($run.wall_seconds * 1000.0) / $chunks
$upperBoundMs = $ChunkMs + $computeMsPerChunk

$report = [ordered]@{
    schemaVersion = 1
    profile = $Profile
    model = [IO.Path]::GetRelativePath($workspace, $modelPath).Replace('\', '/')
    mode = $Mode
    repetitions = $Repetitions
    transcriptMismatches = $run.transcript_mismatches
    streamingRealTimeFactorX = [math]::Round($run.rtfx, 4)
    configuredChunkMs = $ChunkMs
    computeMsPerChunk = [math]::Round($computeMsPerChunk, 4)
    partialUpdateUpperBoundMs = [math]::Round($upperBoundMs, 4)
    partialUpdateMethod = "Configured $ChunkMs ms chunk plus measured average compute per chunk. This is a derived upper bound, not a WebSocket timestamp."
}
$report | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $outputPath -Encoding utf8
$report | ConvertTo-Json -Depth 5
