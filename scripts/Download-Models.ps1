[CmdletBinding()]
param(
    [string]$Config = "config/feasibility.json",
    [string[]]$ModelId = @(
        "nemotron-3.5-asr-streaming-0.6b-q8",
        "magpie-tts-v2602-f16",
        "magpie-tts-tokenizer",
        "nanocodec-22khz-f16",
        "kokoro-82m",
        "qwen3.6-27b-q4-k-m"
    )
)

$ErrorActionPreference = "Stop"
$workspace = Split-Path -Parent $PSScriptRoot
$settings = Get-Content -Raw -LiteralPath (Join-Path $workspace $Config) | ConvertFrom-Json
$hf = Get-Command hf -ErrorAction SilentlyContinue
if (-not $hf) {
    $hfPath = Join-Path $env:USERPROFILE ".local\bin\hf.exe"
    if (-not (Test-Path -LiteralPath $hfPath)) { throw "hf CLI not found." }
    $hf = Get-Item -LiteralPath $hfPath
}

foreach ($id in $ModelId) {
    $model = $settings.models | Where-Object id -eq $id
    if (-not $model) { throw "Unknown model id: $id" }
    if (-not $model.fileName -or -not $model.defaultPath -or -not $model.sha256 -or -not $model.sizeBytes) {
        throw "Model $id does not have a verified downloadable artifact in $Config."
    }

    $destination = Join-Path $workspace $model.defaultPath
    $localDir = Split-Path -Parent $destination
    New-Item -ItemType Directory -Force -Path $localDir | Out-Null

    $remoteFile = if ($model.remoteFile) { $model.remoteFile } else { $model.fileName }
    if ($model.bundleIncludes) {
        $downloadArguments = @(
            "download", $model.repository,
            "--revision", $model.revision,
            "--local-dir", $localDir,
            "--include"
        ) + @($model.bundleIncludes)
        & $hf.Source @downloadArguments
    } else {
        & $hf.Source download $model.repository $remoteFile `
            --revision $model.revision `
            --local-dir $localDir
    }
    if ($LASTEXITCODE -ne 0) { throw "Download failed for $id ($LASTEXITCODE)" }
    $downloaded = Join-Path $localDir $remoteFile
    if ($downloaded -ne $destination -and (Test-Path -LiteralPath $downloaded)) {
        Move-Item -LiteralPath $downloaded -Destination $destination -Force
    }
    if (-not (Test-Path -LiteralPath $destination)) { throw "Downloaded model is missing: $destination" }

    $actualSize = (Get-Item -LiteralPath $destination).Length
    if ($actualSize -ne $model.sizeBytes) {
        throw "Size mismatch for $id. Expected $($model.sizeBytes), got $actualSize."
    }
    $actualHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $destination).Hash.ToLowerInvariant()
    if ($actualHash -ne $model.sha256) {
        throw "SHA-256 mismatch for $id. Expected $($model.sha256), got $actualHash."
    }
    if ($model.extractTo) {
        $extractTo = Join-Path $workspace $model.extractTo
        if (Test-Path -LiteralPath $extractTo) {
            Remove-Item -LiteralPath $extractTo -Recurse -Force
        }
        New-Item -ItemType Directory -Force -Path $extractTo | Out-Null
        tar -xf $destination -C $extractTo
        if ($LASTEXITCODE -ne 0) { throw "Extraction failed for $id ($LASTEXITCODE)" }
        Set-Content -LiteralPath (Join-Path $extractTo ".fasttalk-verified") `
            -Value "verified" -Encoding utf8
    }
    Write-Host "$id verified: $destination"
}
