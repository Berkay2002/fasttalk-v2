[CmdletBinding()]
param(
    [switch]$Unsigned,
    [switch]$SkipNativeBuild
)

$ErrorActionPreference = "Stop"
$workspace = Split-Path -Parent $PSScriptRoot
$appRoot = Join-Path $workspace "app"
$runtimeRoot = Join-Path $workspace "runtime"
$redist = Join-Path $appRoot "src-tauri\windows\redist\vc_redist.x64.exe"
$releaseRoot = Join-Path $workspace "artifacts\release"
$generatedRoot = Join-Path $workspace ".cache\release"
$tauriConfig = Join-Path $generatedRoot "tauri.signing.json"

function Assert-Command([string]$Name) {
    if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) {
        throw "Required command is unavailable: $Name"
    }
}

function Assert-RuntimePayload {
    $required = @(
        "llm\llama-server.exe",
        "asr\nemo-speech.exe",
        "tts\kokoro-worker.exe",
        "cuda-13.3\cublas64_13.dll",
        "cuda-13.3\cublasLt64_13.dll",
        "cuda-13.3\cudart64_13.dll",
        "cuda-13.3\nvJitLink_130_0.dll",
        "cuda-13.3\nvrtc64_130_0.dll",
        "cuda-13.3\nvrtc-builtins64_133.dll"
    )
    foreach ($relative in $required) {
        $path = Join-Path $runtimeRoot $relative
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
            throw "Release runtime is incomplete: $relative"
        }
    }
}

function Find-SignTool {
    $kits = "${env:ProgramFiles(x86)}\Windows Kits\10\bin"
    $tool = Get-ChildItem -LiteralPath $kits -Recurse -Filter "signtool.exe" -ErrorAction SilentlyContinue |
        Where-Object FullName -Like "*\x64\signtool.exe" |
        Sort-Object FullName -Descending |
        Select-Object -First 1
    if (-not $tool) { throw "Windows SDK signtool.exe is unavailable" }
    $tool.FullName
}

function Sign-RuntimePayload(
    [string]$Thumbprint,
    [string]$TimestampUrl,
    [bool]$MachineStore
) {
    $signTool = Find-SignTool
    $arguments = @("sign", "/sha1", $Thumbprint, "/fd", "SHA256", "/tr", $TimestampUrl, "/td", "SHA256")
    if ($MachineStore) { $arguments += "/sm" }
    $files = Get-ChildItem -LiteralPath $runtimeRoot -Recurse -File |
        Where-Object Extension -In ".exe", ".dll"
    foreach ($file in $files) {
        & $signTool @arguments $file.FullName
        if ($LASTEXITCODE -ne 0) { throw "Signing failed for $($file.FullName)" }
        $signature = Get-AuthenticodeSignature -LiteralPath $file.FullName
        if ($signature.Status -ne "Valid") {
            throw "Runtime signature verification failed for $($file.FullName): $($signature.Status)"
        }
    }
}

function Install-LatestVcRedist {
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $redist) | Out-Null
    $download = "$redist.download"
    try {
        Invoke-WebRequest -UseBasicParsing -Uri "https://aka.ms/vc14/vc_redist.x64.exe" -OutFile $download
        $signature = Get-AuthenticodeSignature -LiteralPath $download
        if ($signature.Status -ne "Valid" -or $signature.SignerCertificate.Subject -notlike "*Microsoft Corporation*") {
            throw "Downloaded Visual C++ runtime does not have a valid Microsoft signature"
        }
        Move-Item -LiteralPath $download -Destination $redist -Force
    } finally {
        if (Test-Path -LiteralPath $download) {
            Remove-Item -LiteralPath $download -Force
        }
    }
}

Assert-Command "cargo"
Assert-Command "git"
if (-not (Get-Command "node" -ErrorAction SilentlyContinue)) {
    $node = Get-ChildItem (Join-Path $workspace ".cache\toolchains") -Directory -Filter "node-v*-win-x64" |
        Sort-Object Name -Descending |
        Select-Object -First 1
    if (-not $node) { throw "Node.js is unavailable" }
    $env:Path = "$($node.FullName);$env:Path"
}
Assert-Command "npm"
$tauriCli = Join-Path $appRoot "node_modules\.bin\tauri.cmd"
if (-not (Test-Path -LiteralPath $tauriCli -PathType Leaf)) {
    throw "Project-local Tauri CLI is unavailable. Run npm install in app first."
}

if (-not $SkipNativeBuild) {
    & (Join-Path $PSScriptRoot "Build-NativeRuntimes.ps1")
    if ($LASTEXITCODE -ne 0) { throw "Native runtime build failed ($LASTEXITCODE)" }
}
Assert-RuntimePayload
Install-LatestVcRedist

New-Item -ItemType Directory -Force -Path $releaseRoot, $generatedRoot | Out-Null
if ($Unsigned) {
    '{"bundle":{"windows":{"certificateThumbprint":null}}}' |
        Set-Content -LiteralPath $tauriConfig -Encoding utf8
} else {
    if (git -C $workspace status --porcelain) {
        throw "Signed release builds require a clean git worktree"
    }
    $thumbprint = $env:FASTTALK_SIGNING_THUMBPRINT
    $timestampUrl = $env:FASTTALK_TIMESTAMP_URL
    if (-not $thumbprint) { throw "FASTTALK_SIGNING_THUMBPRINT is required for a release build" }
    if (-not $timestampUrl) { throw "FASTTALK_TIMESTAMP_URL is required for a release build" }
    $currentUserCertificate = Get-ChildItem Cert:\CurrentUser\My -CodeSigningCert |
        Where-Object Thumbprint -EQ $thumbprint |
        Where-Object NotAfter -GT (Get-Date) |
        Select-Object -First 1
    $machineCertificate = Get-ChildItem Cert:\LocalMachine\My -CodeSigningCert |
        Where-Object Thumbprint -EQ $thumbprint |
        Where-Object NotAfter -GT (Get-Date) |
        Select-Object -First 1
    $certificate = if ($currentUserCertificate) { $currentUserCertificate } else { $machineCertificate }
    if (-not $certificate) { throw "A valid code-signing certificate with the configured thumbprint was not found" }
    Sign-RuntimePayload $thumbprint $timestampUrl ([bool]$machineCertificate)
    @{
        bundle = @{
            windows = @{
                certificateThumbprint = $thumbprint
                digestAlgorithm = "sha256"
                timestampUrl = $timestampUrl
                tsp = $true
            }
        }
    } | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $tauriConfig -Encoding utf8
}

cargo test --workspace --locked --release
if ($LASTEXITCODE -ne 0) { throw "Workspace tests failed ($LASTEXITCODE)" }

Push-Location $appRoot
try {
    & $tauriCli build --bundles nsis --config $tauriConfig
    if ($LASTEXITCODE -ne 0) { throw "Tauri NSIS build failed ($LASTEXITCODE)" }
} finally {
    Pop-Location
}

$app = Join-Path $workspace "target\release\fasttalk-app.exe"
$installer = Get-ChildItem (Join-Path $workspace "target\release\bundle\nsis") -Filter "*-setup.exe" |
    Sort-Object LastWriteTimeUtc -Descending |
    Select-Object -First 1
if (-not (Test-Path -LiteralPath $app)) { throw "Release application executable was not produced" }
if (-not $installer) { throw "NSIS installer was not produced" }

$files = @((Get-Item -LiteralPath $app), $installer, (Get-Item -LiteralPath $redist))
$artifacts = foreach ($file in $files) {
    $signature = Get-AuthenticodeSignature -LiteralPath $file.FullName
    if (-not $Unsigned -and $signature.Status -ne "Valid") {
        throw "Release signature verification failed for $($file.FullName): $($signature.Status)"
    }
    [ordered]@{
        path = $file.FullName
        sizeBytes = $file.Length
        sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $file.FullName).Hash.ToLowerInvariant()
        signatureStatus = $signature.Status.ToString()
        signer = if ($signature.SignerCertificate) { $signature.SignerCertificate.Subject } else { $null }
    }
}
$runtimeArtifacts = foreach ($file in Get-ChildItem -LiteralPath $runtimeRoot -Recurse -File) {
    $signature = Get-AuthenticodeSignature -LiteralPath $file.FullName
    [ordered]@{
        path = [System.IO.Path]::GetRelativePath($workspace, $file.FullName)
        sizeBytes = $file.Length
        sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $file.FullName).Hash.ToLowerInvariant()
        signatureStatus = $signature.Status.ToString()
        signer = if ($signature.SignerCertificate) { $signature.SignerCertificate.Subject } else { $null }
    }
}

[ordered]@{
    builtAtUtc = [DateTime]::UtcNow.ToString("o")
    commit = (git -C $workspace rev-parse HEAD)
    signedRelease = -not $Unsigned
    artifacts = $artifacts
    runtime = $runtimeArtifacts
} | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath (Join-Path $releaseRoot "release-manifest.json") -Encoding utf8

Write-Host "Installer: $($installer.FullName)"
Write-Host "Manifest: $(Join-Path $releaseRoot 'release-manifest.json')"
