[CmdletBinding()]
param(
    [string]$Installer,
    [int]$LaunchTimeoutSeconds = 30,
    [switch]$SanitizedEnvironment,
    [string]$Output
)

$ErrorActionPreference = "Stop"
$workspace = Split-Path -Parent $PSScriptRoot
$runtimeRoot = Join-Path $workspace "runtime"

function Get-FastTalkInstall {
    Get-ChildItem "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall" -ErrorAction SilentlyContinue |
        ForEach-Object { Get-ItemProperty $_.PSPath } |
        Where-Object DisplayName -EQ "FastTalk" |
        Select-Object -First 1
}

if (-not $Installer) {
    $Installer = Get-ChildItem (Join-Path $workspace "target\release\bundle\nsis") -Filter "*-setup.exe" |
        Sort-Object LastWriteTimeUtc -Descending |
        Select-Object -First 1 -ExpandProperty FullName
}
if (-not $Installer -or -not (Test-Path -LiteralPath $Installer -PathType Leaf)) {
    throw "NSIS installer not found"
}
if (Get-FastTalkInstall) {
    throw "FastTalk is already installed; refusing to replace an existing installation"
}

$install = $null
$verifiedRuntimeFiles = 0
$processEnvironment = if ($SanitizedEnvironment) {
    @{
        PATH = "$env:SystemRoot\System32;$env:SystemRoot"
        CUDA_PATH = $null
        CUDA_HOME = $null
        NODE_PATH = $null
        NODE_OPTIONS = $null
        PYTHONHOME = $null
        PYTHONPATH = $null
        RUSTUP_HOME = $null
        CARGO_HOME = $null
        VCPKG_ROOT = $null
    }
} else { $null }
try {
    $startInstaller = @{
        FilePath = $Installer
        ArgumentList = "/S"
        PassThru = $true
        Wait = $true
        WindowStyle = "Hidden"
    }
    if ($processEnvironment) { $startInstaller.Environment = $processEnvironment }
    $process = Start-Process @startInstaller
    if ($process.ExitCode -ne 0) { throw "Installer exited $($process.ExitCode)" }
    $install = Get-FastTalkInstall
    if (-not $install) { throw "Installer did not create a current-user uninstall entry" }
    $installRoot = $install.InstallLocation.Trim('"')

    $installedApp = Join-Path $installRoot "fasttalk-app.exe"
    $required = @(
        "runtime\llm\llama-server.exe",
        "runtime\asr\nemo-speech.exe",
        "runtime\tts\kokoro-worker.exe",
        "runtime\cuda-13.3\cublasLt64_13.dll",
        "prerequisites\vc_redist.x64.exe"
    )
    foreach ($relative in $required) {
        if (-not (Test-Path -LiteralPath (Join-Path $installRoot $relative) -PathType Leaf)) {
            throw "Installed payload is missing $relative"
        }
    }
    foreach ($source in Get-ChildItem -LiteralPath $runtimeRoot -Recurse -File) {
        $relative = [System.IO.Path]::GetRelativePath($runtimeRoot, $source.FullName)
        $installed = Join-Path (Join-Path $installRoot "runtime") $relative
        if (-not (Test-Path -LiteralPath $installed -PathType Leaf)) {
            throw "Installed runtime is missing $relative"
        }
        if ((Get-FileHash -Algorithm SHA256 $source.FullName).Hash -ne (Get-FileHash -Algorithm SHA256 $installed).Hash) {
            throw "Installed runtime hash mismatch: $relative"
        }
        $verifiedRuntimeFiles++
    }

    $startApp = @{ FilePath = $installedApp; PassThru = $true }
    if ($processEnvironment) { $startApp.Environment = $processEnvironment }
    $app = Start-Process @startApp
    $timer = [Diagnostics.Stopwatch]::StartNew()
    while ($timer.Elapsed.TotalSeconds -lt $LaunchTimeoutSeconds -and -not $app.HasExited -and $app.MainWindowHandle -eq 0) {
        Start-Sleep -Milliseconds 250
        $app.Refresh()
    }
    if ($app.HasExited -or $app.MainWindowHandle -eq 0 -or $app.MainWindowTitle -ne "FastTalk") {
        throw "Installed application did not open the FastTalk window"
    }
    $app.Kill($true)
    $app.WaitForExit()
} finally {
    if ($install) {
        $uninstaller = $install.UninstallString.Trim('"')
        if (Test-Path -LiteralPath $uninstaller) {
            $startUninstaller = @{
                FilePath = $uninstaller
                ArgumentList = "/S"
                PassThru = $true
                Wait = $true
                WindowStyle = "Hidden"
            }
            if ($processEnvironment) { $startUninstaller.Environment = $processEnvironment }
            $process = Start-Process @startUninstaller
            if ($process.ExitCode -ne 0) { throw "Uninstaller exited $($process.ExitCode)" }
            Start-Sleep -Seconds 2
        }
    }
}

if (Get-FastTalkInstall) { throw "Uninstaller left the current-user registry entry behind" }
if ($install -and (Test-Path -LiteralPath $install.InstallLocation.Trim('"'))) {
    throw "Uninstaller left the installation directory behind"
}

if ($Output) {
    $outputPath = Join-Path $workspace $Output
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $outputPath) | Out-Null
    [ordered]@{
        schemaVersion = 1
        installer = $Installer
        installerSha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $Installer).Hash.ToLowerInvariant()
        sanitizedEnvironment = [bool]$SanitizedEnvironment
        runtimeFilesVerified = $verifiedRuntimeFiles
        windowOpened = $true
        uninstallPassed = $true
    } | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $outputPath -Encoding utf8
}

Write-Host "Installer install, launch, payload, and uninstall checks passed."
