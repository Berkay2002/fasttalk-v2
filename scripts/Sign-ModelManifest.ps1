[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string]$Seed,
    [string]$Manifest = "config/models.manifest.json",
    [string]$PublicKey = "config/models.manifest.pub",
    [string]$Signature = "config/models.manifest.sig"
)

$ErrorActionPreference = "Stop"
$workspace = Split-Path -Parent $PSScriptRoot
$seedPath = (Resolve-Path -LiteralPath $Seed).Path
$manifestPath = (Resolve-Path -LiteralPath (Join-Path $workspace $Manifest)).Path
$publicKeyPath = Join-Path $workspace $PublicKey
$signaturePath = Join-Path $workspace $Signature

& cargo run --quiet -p fasttalk-model-manager --bin fasttalk-model-sign -- `
    public-key $seedPath $publicKeyPath
if ($LASTEXITCODE -ne 0) { throw "Public-key generation failed ($LASTEXITCODE)" }

& cargo run --quiet -p fasttalk-model-manager --bin fasttalk-model-sign -- `
    sign $seedPath $manifestPath $signaturePath
if ($LASTEXITCODE -ne 0) { throw "Manifest signing failed ($LASTEXITCODE)" }

& cargo run --quiet -p fasttalk-model-manager --bin fasttalk-model-sign -- `
    verify $publicKeyPath $manifestPath $signaturePath
if ($LASTEXITCODE -ne 0) { throw "Manifest signature verification failed ($LASTEXITCODE)" }

Write-Host "Signed $manifestPath"
