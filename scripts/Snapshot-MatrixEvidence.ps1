[CmdletBinding()]
param(
    [string]$OutputDirectory = "benchmarks/2026-08-11-rtx3090-matrix/evidence"
)

$ErrorActionPreference = "Stop"
$workspace = (Resolve-Path (Split-Path -Parent $PSScriptRoot)).Path
$output = [IO.Path]::GetFullPath((Join-Path $workspace $OutputDirectory))
if (-not $output.StartsWith($workspace + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
    throw "Matrix evidence output must remain inside the workspace"
}

$sources = [ordered]@{
    "qwen36-q4-16k.json" = "benchmarks/2026-08-11-rtx3090-post-audit/evidence/qwen36-llm.json"
    "qwen35-q5-16k-p4.json" = "artifacts/matrix/qwen35-q5-16k.json"
    "qwen35-q5-32k-p1.json" = "artifacts/matrix/qwen35-q5-32k-p1.json"
    "qwen35-q8-32k-p1.json" = "artifacts/matrix/qwen35-q8-32k-p1.json"
    "qwen3-14b-q5-32k-p1.json" = "artifacts/matrix/qwen3-14b-q5-32k-p1.json"
    "nemotron35-asr-performance.json" = "benchmarks/2026-08-11-rtx3090-post-audit/evidence/asr.json"
    "nemotron35-asr-corpus.json" = "artifacts/matrix/nemotron35-asr-corpus.json"
    "nemotron-speech-en-asr-performance.json" = "artifacts/matrix/nemotron-speech-en-asr.json"
    "nemotron-speech-en-asr-corpus.json" = "artifacts/matrix/nemotron-speech-en-asr-corpus.json"
    "parakeet-asr-performance.json" = "artifacts/matrix/parakeet-ctc-asr.json"
    "parakeet-asr-corpus.json" = "artifacts/matrix/parakeet-ctc-asr-corpus.json"
    "magpie-streaming.json" = "benchmarks/2026-08-11-rtx3090-post-audit/evidence/magpie-streaming.json"
    "magpie-incremental-transport.json" = "benchmarks/2026-08-11-rtx3090-post-audit/evidence/magpie-streaming-transport.json"
    "kokoro-cpu.json" = "benchmarks/2026-08-11-rtx3090-post-audit/evidence/kokoro-compact.json"
    "qwen35-q5-32k-nemotron35-magpie-vram.json" = "artifacts/matrix/qwen35-q5-32k-nemotron35-magpie-vram.json"
    "qwen35-q5-32k-parakeet-asr-vram.json" = "artifacts/matrix/qwen35-q5-32k-parakeet-asr-vram.json"
    "qwen35-q5-32k-parakeet-magpie-vram.json" = "artifacts/matrix/qwen35-q5-32k-parakeet-magpie-vram.json"
    "qwen3-14b-q5-16k-parakeet-magpie-vram.json" = "artifacts/matrix/qwen3-14b-q5-16k-parakeet-magpie-vram.json"
    "qwen3-14b-q5-32k-parakeet-magpie-vram.json" = "artifacts/matrix/qwen3-14b-q5-32k-parakeet-magpie-vram.json"
    "selected-conversation-20-turn.json" = "artifacts/matrix/qwen35-q5-parakeet-32k-conversation-20-turn.json"
    "selected-acoustic-scenarios.json" = "artifacts/matrix/parakeet-acoustic-scenarios/summary.json"
    "selected-conversation-30-minute-soak.json" = "artifacts/matrix/qwen35-q5-parakeet-32k-conversation-30-minute-soak.json"
}

New-Item -ItemType Directory -Force -Path $output | Out-Null
$manifestFiles = @()
foreach ($entry in $sources.GetEnumerator()) {
    $source = Join-Path $workspace $entry.Value
    if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
        throw "Required matrix evidence is missing: $($entry.Value)"
    }
    $destination = Join-Path $output $entry.Key
    Copy-Item -LiteralPath $source -Destination $destination -Force
}

$listeningSource = Join-Path $workspace "artifacts/matrix/tts-listening"
$listeningOutput = Join-Path $output "tts-listening"
New-Item -ItemType Directory -Force -Path $listeningOutput | Out-Null
foreach ($file in Get-ChildItem -LiteralPath $listeningSource -File | Where-Object Extension -In ".wav", ".json") {
    Copy-Item -LiteralPath $file.FullName -Destination (Join-Path $listeningOutput $file.Name) -Force
}

foreach ($file in Get-ChildItem -LiteralPath $output -Recurse -File | Where-Object Name -NE "manifest.json") {
    $manifestFiles += [ordered]@{
        file = [IO.Path]::GetRelativePath($output, $file.FullName).Replace('\', '/')
        bytes = $file.Length
        sha256 = (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
    }
}
$manifest = [ordered]@{
    schemaVersion = 1
    runId = "2026-08-11-rtx3090-matrix"
    sourceCommit = (& git -C $workspace rev-parse HEAD).Trim()
    files = $manifestFiles
}
$manifest | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $output "manifest.json") -Encoding utf8
Write-Host "Snapshotted $($manifestFiles.Count) matrix evidence files to $output"
