[CmdletBinding()]
param(
    [string]$Config = "config/feasibility.json",
    [string]$SourceRoot = ".cache/sources",
    [string]$BuildRoot = ".cache/build",
    [string]$RuntimeRoot = "runtime",
    [int]$Jobs = 0
)

$ErrorActionPreference = "Stop"
$workspace = Split-Path -Parent $PSScriptRoot
$settings = Get-Content -Raw -LiteralPath (Join-Path $workspace $Config) | ConvertFrom-Json
$sources = Join-Path $workspace $SourceRoot
$build = Join-Path $workspace $BuildRoot
$runtime = Join-Path $workspace $RuntimeRoot
$llamaSource = Join-Path $sources "llama.cpp"
$nemoSource = Join-Path $sources "nemo-speech.cpp"
$vcpkgSource = Join-Path $sources "vcpkg"

if ($Jobs -le 0) { $Jobs = [Environment]::ProcessorCount }

foreach ($source in $settings.sources) {
    $target = Join-Path $sources $source.id
    if (-not (Test-Path -LiteralPath (Join-Path $target ".git"))) {
        throw "Missing pinned checkout: $target. Run scripts/Initialize-NativeSources.ps1 first."
    }
    $actual = git -C $target rev-parse HEAD
    if ($actual -ne $source.revision) {
        throw "Revision mismatch for $($source.id): expected $($source.revision), got $actual"
    }
    if ($source.id -ne "nemo-speech.cpp" -and (git -C $target status --porcelain)) {
        throw "Pinned source checkout is dirty: $target"
    }
}

$cudaPath = [Environment]::GetEnvironmentVariable("CUDA_PATH", "Machine")
if (-not $cudaPath) {
    $cudaPath = "C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA\v$($settings.target.cudaToolkitVersion)"
}
$nvcc = Join-Path $cudaPath "bin\nvcc.exe"
if (-not (Test-Path -LiteralPath $nvcc)) { throw "Pinned nvcc not found: $nvcc" }
$nvccVersion = & $nvcc --version 2>&1 | Out-String
if ($nvccVersion -notmatch "release $([regex]::Escape($settings.target.cudaToolkitVersion))") {
    throw "Expected CUDA $($settings.target.cudaToolkitVersion), got: $nvccVersion"
}
$env:CUDA_PATH = $cudaPath
$env:CUDACXX = $nvcc
$env:Path = "$(Join-Path $cudaPath 'bin');$env:Path"

$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$vsPath = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if (-not $vsPath) { throw "Visual Studio 2022 C++ Build Tools not found." }
$vcvars = Join-Path $vsPath "VC\Auxiliary\Build\vcvars64.bat"
cmd /c "`"$vcvars`" >NUL 2>&1 && set" | ForEach-Object {
    if ($_ -match "^([^=]+)=(.*)$") { Set-Item -Path "env:$($matches[1])" -Value $matches[2] }
}

New-Item -ItemType Directory -Force -Path $build, $runtime | Out-Null

$vcpkgExe = Join-Path $vcpkgSource "vcpkg.exe"
if (-not (Test-Path -LiteralPath $vcpkgExe)) {
    & (Join-Path $vcpkgSource "bootstrap-vcpkg.bat") -disableMetrics
    if ($LASTEXITCODE -ne 0) { throw "vcpkg bootstrap failed ($LASTEXITCODE)" }
}
$env:VCPKG_DISABLE_METRICS = "1"
$env:VCPKG_MAX_CONCURRENCY = $Jobs
& $vcpkgExe install sentencepiece:x64-windows --clean-after-build
if ($LASTEXITCODE -ne 0) { throw "vcpkg SentencePiece install failed ($LASTEXITCODE)" }

$llamaBuild = Join-Path $build "llama.cpp"
cmake -S $llamaSource -B $llamaBuild -G Ninja `
    -DCMAKE_BUILD_TYPE=Release `
    -DCMAKE_CUDA_ARCHITECTURES=86-real `
    -DGGML_CUDA=ON `
    -DGGML_NATIVE=OFF `
    -DLLAMA_BUILD_SERVER=ON `
    -DLLAMA_BUILD_TESTS=OFF `
    -DLLAMA_BUILD_EXAMPLES=OFF `
    -DGGML_BUILD_TESTS=OFF
if ($LASTEXITCODE -ne 0) { throw "llama.cpp configure failed ($LASTEXITCODE)" }
cmake --build $llamaBuild --config Release --target llama-server --parallel $Jobs
if ($LASTEXITCODE -ne 0) { throw "llama.cpp build failed ($LASTEXITCODE)" }

$nemoBuild = Join-Path $build "nemo-speech.cpp-vcpkg"
$nemoPatch = Join-Path $workspace "patches\nemo-speech\0001-link-absl-flags.patch"
git -C $nemoSource apply --check $nemoPatch 2>$null
if ($LASTEXITCODE -eq 0) {
    git -C $nemoSource apply $nemoPatch
    if ($LASTEXITCODE -ne 0) { throw "FastTalk NeMo patch failed to apply ($LASTEXITCODE)" }
} else {
    git -C $nemoSource apply --reverse --check $nemoPatch 2>$null
    if ($LASTEXITCODE -ne 0) { throw "FastTalk NeMo patch is neither applicable nor already applied." }
}
& (Join-Path $nemoSource "scripts\windows\apply-ggml-patches.ps1")
if ($LASTEXITCODE -ne 0) { throw "NeMo-Speech.cpp ggml patch setup failed ($LASTEXITCODE)" }
cmake -S $nemoSource -B $nemoBuild -G Ninja `
    -DCMAKE_BUILD_TYPE=Release `
    -DCMAKE_CUDA_ARCHITECTURES=86-real `
    -DCMAKE_TOOLCHAIN_FILE="$(Join-Path $vcpkgSource 'scripts\buildsystems\vcpkg.cmake')" `
    -DVCPKG_TARGET_TRIPLET=x64-windows `
    -DGGML_CUDA=ON `
    -DNEMO_SPEECH_BUILD_HTTP=ON `
    -DNEMO_SPEECH_BUILD_GRPC=OFF `
    -DNEMO_SPEECH_BUILD_NMT=OFF `
    -DNEMO_SPEECH_WITH_FLASHLIGHT=OFF `
    -DNEMO_SPEECH_WITH_NORM=OFF
if ($LASTEXITCODE -ne 0) { throw "NeMo-Speech.cpp configure failed ($LASTEXITCODE)" }
cmake --build $nemoBuild --config Release --parallel $Jobs
if ($LASTEXITCODE -ne 0) { throw "NeMo-Speech.cpp build failed ($LASTEXITCODE)" }

$llamaBin = Join-Path $llamaBuild "bin"
$nemoBin = Join-Path $nemoBuild "bin"
$llamaRuntime = Join-Path $runtime "llm"
$nemoRuntime = Join-Path $runtime "asr"
New-Item -ItemType Directory -Force -Path $llamaRuntime, $nemoRuntime | Out-Null
Copy-Item -Path (Join-Path $llamaBin "*") -Destination $llamaRuntime -Recurse -Force
Copy-Item -Path (Join-Path $nemoBin "*") -Destination $nemoRuntime -Recurse -Force

$llamaExe = Join-Path $llamaRuntime "llama-server.exe"
$nemoExe = Join-Path $nemoRuntime "nemo-speech.exe"
foreach ($executable in $llamaExe, $nemoExe) {
    if (-not (Test-Path -LiteralPath $executable)) { throw "Expected executable missing: $executable" }
}

Write-Host "llama-server: $llamaExe"
Write-Host "nemo-speech: $nemoExe"
