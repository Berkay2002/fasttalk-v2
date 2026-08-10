[CmdletBinding()]
param(
    [string]$Output = "config/models.manifest.json",
    [string]$KokoroRoot = ".cache/models/kokoro-sherpa-v1.0"
)

$ErrorActionPreference = "Stop"
$workspace = Split-Path -Parent $PSScriptRoot
$feasibility = Get-Content -Raw -LiteralPath (Join-Path $workspace "config/feasibility.json") |
    ConvertFrom-Json

function Get-Model([string]$Id) {
    $model = $feasibility.models | Where-Object id -eq $Id
    if (-not $model) { throw "Model is missing from feasibility config: $Id" }
    return $model
}

function Get-Artifact([string]$Root, [object]$Model) {
    $path = Join-Path $workspace $Model.defaultPath
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "Pinned artifact is missing: $path"
    }
    $item = Get-Item -LiteralPath $path
    $hash = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($item.Length -ne $Model.sizeBytes -or $hash -ne $Model.sha256) {
        throw "Pinned artifact no longer matches feasibility config: $($Model.id)"
    }
    return [ordered]@{
        remotePath = if ($Model.remoteFile) { $Model.remoteFile } else { $Model.fileName }
        path = [IO.Path]::GetRelativePath((Join-Path $workspace $Root), $path).Replace('\', '/')
        sizeBytes = [int64]$item.Length
        sha256 = $hash
    }
}

function Get-TreeArtifacts([string]$Root) {
    $absoluteRoot = Join-Path $workspace $Root
    if (-not (Test-Path -LiteralPath $absoluteRoot -PathType Container)) {
        throw "Kokoro source tree is missing: $absoluteRoot"
    }
    $required = @("LICENSE", "model.int8.onnx", "tokens.txt", "voices.bin")
    foreach ($name in $required) {
        if (-not (Test-Path -LiteralPath (Join-Path $absoluteRoot $name) -PathType Leaf)) {
            throw "Kokoro source tree is missing $name"
        }
    }
    $files = @(
        $required | ForEach-Object { Get-Item -LiteralPath (Join-Path $absoluteRoot $_) }
        Get-ChildItem -LiteralPath (Join-Path $absoluteRoot "espeak-ng-data") -File -Recurse
    ) | Sort-Object FullName
    return @($files | ForEach-Object {
        $relative = [IO.Path]::GetRelativePath($absoluteRoot, $_.FullName).Replace('\', '/')
        [ordered]@{
            remotePath = $relative
            path = $relative
            sizeBytes = [int64]$_.Length
            sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        }
    })
}

$qwen = Get-Model "qwen3.5-9b-q5-k-m"
$asr = Get-Model "nemotron-3.5-asr-streaming-0.6b-q8"
$magpie = Get-Model "magpie-tts-v2602-f16"
$magpieTokenizer = Get-Model "magpie-tts-tokenizer"
$codec = Get-Model "nanocodec-22khz-f16"

$manifest = [ordered]@{
    schemaVersion = 1
    release = "2026.08.11.1"
    publicKeyId = "fasttalk-models-2026-01"
    models = @(
        [ordered]@{
            id = "qwen"
            displayName = "Qwen3.5 9B Q5_K_M compatibility profile"
            repository = $qwen.repository
            revision = $qwen.revision
            legacyRoot = ".cache/models/qwen3.5-9b"
            artifacts = @(Get-Artifact ".cache/models/qwen3.5-9b" $qwen)
            license = [ordered]@{
                id = "apache-2.0"
                name = "Apache License 2.0"
                url = "https://huggingface.co/Qwen/Qwen3.5-9B/blob/main/LICENSE"
            }
            postInstall = @()
        },
        [ordered]@{
            id = "nemotron-asr"
            displayName = "Nemotron 3.5 ASR Streaming 0.6B Q8"
            repository = $asr.repository
            revision = $asr.revision
            legacyRoot = ".cache/models/nemotron-asr"
            artifacts = @(Get-Artifact ".cache/models/nemotron-asr" $asr)
            license = [ordered]@{
                id = "openmdw-1.1"
                name = "Open Model and Data Weights License 1.1"
                url = "https://openmdw.ai/license/1-1/"
            }
            postInstall = @()
        },
        [ordered]@{
            id = "magpie-tts"
            displayName = "Magpie TTS v2602 and tokenizer"
            repository = $magpie.repository
            revision = $magpie.revision
            legacyRoot = ".cache/models/magpie-tts"
            artifacts = @(
                Get-Artifact ".cache/models/magpie-tts" $magpie
                Get-Artifact ".cache/models/magpie-tts" $magpieTokenizer
            )
            license = [ordered]@{
                id = "nvidia-open-model-license"
                name = "NVIDIA Open Model License"
                url = "https://www.nvidia.com/en-us/agreements/enterprise-software/nvidia-open-model-license"
            }
            postInstall = @(
                [ordered]@{
                    kind = "extractTar"
                    artifact = $magpieTokenizer.fileName
                    destination = "extracted"
                }
            )
        },
        [ordered]@{
            id = "nanocodec"
            displayName = "NVIDIA NanoCodec 22 kHz decoder"
            repository = $codec.repository
            revision = $codec.revision
            legacyRoot = ".cache/models/nano-codec"
            artifacts = @(Get-Artifact ".cache/models/nano-codec" $codec)
            license = [ordered]@{
                id = "nvidia-open-model-license"
                name = "NVIDIA Open Model License"
                url = "https://developer.download.nvidia.com/licenses/nvidia-open-model-license-agreement-june-2024.pdf"
            }
            postInstall = @()
        },
        [ordered]@{
            id = "kokoro"
            displayName = "Kokoro 82M INT8 CPU fallback"
            repository = "csukuangfj/kokoro-int8-multi-lang-v1_0"
            revision = "5d6cbe65546edb3ebae8bde976c8ad3438b3f34b"
            legacyRoot = $KokoroRoot
            artifacts = @(Get-TreeArtifacts $KokoroRoot)
            license = [ordered]@{
                id = "apache-2.0"
                name = "Apache License 2.0"
                url = "https://huggingface.co/csukuangfj/kokoro-int8-multi-lang-v1_0/blob/5d6cbe65546edb3ebae8bde976c8ad3438b3f34b/LICENSE"
            }
            postInstall = @()
        }
    )
}

$outputPath = Join-Path $workspace $Output
$manifest | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $outputPath -Encoding utf8
Write-Host "Wrote signed-manifest input with $($manifest.models.Count) model groups to $outputPath"
