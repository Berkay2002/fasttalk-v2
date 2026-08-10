[CmdletBinding()]
param(
    [string]$Manifest = "config/models.manifest.json",
    [string]$Signature = "config/models.manifest.sig",
    [string]$PublicKey = "config/models.manifest.pub",
    [string]$Output = "artifacts/feasibility/model-manifest-verification.json"
)

$ErrorActionPreference = "Stop"
$workspace = Split-Path -Parent $PSScriptRoot
$manifestPath = Join-Path $workspace $Manifest
$signaturePath = Join-Path $workspace $Signature
$publicKeyPath = Join-Path $workspace $PublicKey

& cargo run --quiet -p fasttalk-model-manager --bin fasttalk-model-sign -- `
    verify $publicKeyPath $manifestPath $signaturePath
if ($LASTEXITCODE -ne 0) { throw "Signed model manifest verification failed ($LASTEXITCODE)" }

$manifestValue = Get-Content -Raw -LiteralPath $manifestPath | ConvertFrom-Json
$groups = @()
$totalVerifiedBytes = [int64]0
foreach ($model in $manifestValue.models) {
    $root = Join-Path $workspace $model.legacyRoot
    $verifiedBytes = [int64]0
    foreach ($artifact in $model.artifacts) {
        $path = Join-Path $root $artifact.path
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
            throw "Model artifact is missing: $path"
        }
        $size = (Get-Item -LiteralPath $path).Length
        if ($size -ne $artifact.sizeBytes) {
            throw "Size mismatch for $($model.id)/$($artifact.path): $size"
        }
        $hash = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($hash -ne $artifact.sha256) {
            throw "SHA-256 mismatch for $($model.id)/$($artifact.path)"
        }
        $verifiedBytes += $size
    }
    foreach ($action in $model.postInstall) {
        if ($action.kind -eq "extractTar") {
            $marker = Join-Path (Join-Path $root $action.destination) ".fasttalk-verified"
            if (-not (Test-Path -LiteralPath $marker -PathType Leaf)) {
                throw "Verified extraction marker is missing: $marker"
            }
        }
    }
    $groups += [ordered]@{
        id = $model.id
        state = "ready"
        source = "legacy"
        artifactCount = @($model.artifacts).Count
        verifiedBytes = $verifiedBytes
    }
    $totalVerifiedBytes += $verifiedBytes
}

$report = [ordered]@{
    schemaVersion = 1
    release = $manifestValue.release
    signatureVerified = $true
    groups = $groups
    totalVerifiedBytes = $totalVerifiedBytes
}
$outputPath = Join-Path $workspace $Output
New-Item -ItemType Directory -Force -Path (Split-Path -Parent $outputPath) | Out-Null
$report | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $outputPath -Encoding utf8
Write-Host "Verified $($groups.Count) signed model groups and $($report.totalVerifiedBytes) bytes."
