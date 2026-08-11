[CmdletBinding()]
param(
    [string]$OutputDirectory = "benchmarks/2026-08-11-rtx3090-post-audit/evidence"
)

$ErrorActionPreference = "Stop"
$workspace = (Resolve-Path (Split-Path -Parent $PSScriptRoot)).Path
$outputPath = [System.IO.Path]::GetFullPath((Join-Path $workspace $OutputDirectory))
if (-not $outputPath.StartsWith($workspace + [System.IO.Path]::DirectorySeparatorChar, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Benchmark evidence output must remain inside the workspace."
}

$sources = @(
    @{ Source = "artifacts/feasibility/preflight.json"; Name = "preflight.json"; Required = $true },
    @{ Source = "artifacts/feasibility/asr-benchmark.json"; Name = "asr.json"; Required = $true },
    @{ Source = "artifacts/feasibility/llama-benchmark.json"; Name = "qwen36-llm.json"; Required = $true },
    @{ Source = "artifacts/feasibility/magpie-benchmark.json"; Name = "magpie-buffered.json"; Required = $true },
    @{ Source = "artifacts/feasibility/magpie-streaming-benchmark.json"; Name = "magpie-streaming.json"; Required = $true },
    @{ Source = "artifacts/feasibility/magpie-streaming-transport.json"; Name = "magpie-streaming-transport.json"; Required = $true },
    @{ Source = "artifacts/feasibility/kokoro-v1-bundle-smoke.json"; Name = "kokoro-v1.json"; Required = $true },
    @{ Source = "artifacts/feasibility/kokoro-fallback-transport.json"; Name = "kokoro-compact.json"; Required = $true },
    @{ Source = "artifacts/feasibility/combined-vram.json"; Name = "qwen36-combined-vram.json"; Required = $true },
    @{ Source = "artifacts/feasibility/measured-evidence.json"; Name = "qwen36-pipeline-evidence.json"; Required = $true },
    @{ Source = "artifacts/feasibility/measured-report.json"; Name = "qwen36-pipeline-gates.json"; Required = $true },
    @{ Source = "artifacts/release/qwen3.5-benchmark.json"; Name = "qwen35-llm.json"; Required = $true },
    @{ Source = "artifacts/release/qwen3.5-asr-vram.json"; Name = "qwen35-asr-vram.json"; Required = $true },
    @{ Source = "artifacts/release/qwen3.5-combined-vram.json"; Name = "qwen35-combined-vram.json"; Required = $true },
    @{ Source = "artifacts/release/conversation-benchmark.json"; Name = "conversation-20-turn.json"; Required = $true },
    @{ Source = "artifacts/release/acoustic-scenarios/summary.json"; Name = "acoustic-scenarios.json"; Required = $true },
    @{ Source = "artifacts/release/conversation-30-minute-soak.json"; Name = "conversation-30-minute-soak.json"; Required = $true },
    @{ Source = "artifacts/release/measured-evidence.json"; Name = "corrected-pipeline-evidence.json"; Required = $true },
    @{ Source = "artifacts/release/gate-report.json"; Name = "release-gates.json"; Required = $true },
    @{ Source = "artifacts/release/installer-sanitized.json"; Name = "installer-sanitized.json"; Required = $true },
    @{ Source = "artifacts/release/release-manifest.json"; Name = "unsigned-release-manifest.json"; Required = $true }
)

New-Item -ItemType Directory -Force -Path $outputPath | Out-Null
$manifestFiles = @()
foreach ($entry in $sources) {
    $sourcePath = Join-Path $workspace $entry.Source
    if (-not (Test-Path -LiteralPath $sourcePath)) {
        if ($entry.Required) {
            throw "Required benchmark evidence is missing: $($entry.Source)"
        }
        continue
    }

    $destination = Join-Path $outputPath $entry.Name
    Copy-Item -LiteralPath $sourcePath -Destination $destination -Force
    $file = Get-Item -LiteralPath $destination
    $manifestFiles += [ordered]@{
        file = $entry.Name
        source = $entry.Source
        bytes = $file.Length
        sha256 = (Get-FileHash -LiteralPath $destination -Algorithm SHA256).Hash.ToLowerInvariant()
    }
}

$commit = (& git -C $workspace rev-parse HEAD).Trim()
$manifest = [ordered]@{
    schemaVersion = 1
    runId = "2026-08-11-rtx3090-post-audit"
    sourceCommit = $commit
    files = $manifestFiles
}
$manifest | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $outputPath "manifest.json") -Encoding utf8
Write-Host "Snapshotted $($manifestFiles.Count) benchmark evidence files to $outputPath"
