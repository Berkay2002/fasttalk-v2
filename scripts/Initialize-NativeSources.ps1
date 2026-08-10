[CmdletBinding()]
param(
    [string]$Config = "config/feasibility.json",
    [string]$SourceRoot = ".cache/sources"
)

$ErrorActionPreference = "Stop"
$workspace = Split-Path -Parent $PSScriptRoot
$configPath = Join-Path $workspace $Config
$sourcePath = Join-Path $workspace $SourceRoot
$settings = Get-Content -Raw -LiteralPath $configPath | ConvertFrom-Json

New-Item -ItemType Directory -Force -Path $sourcePath | Out-Null

foreach ($source in $settings.sources) {
    $target = Join-Path $sourcePath $source.id
    if (-not (Test-Path -LiteralPath (Join-Path $target ".git"))) {
        git init $target
        if ($LASTEXITCODE -ne 0) { throw "git init failed for $($source.id)" }
        git -C $target remote add origin $source.remote
        if ($LASTEXITCODE -ne 0) { throw "git remote add failed for $($source.id)" }
    }

    $dirty = git -C $target status --porcelain
    if ($dirty) { throw "Pinned source checkout is dirty: $target" }

    $remote = git -C $target remote get-url origin
    if ($remote -ne $source.remote) {
        throw "Unexpected origin for $($source.id): $remote"
    }

    git -C $target fetch --depth 1 origin $source.revision
    if ($LASTEXITCODE -ne 0) { throw "git fetch failed for $($source.id)" }
    git -C $target checkout --detach $source.revision
    if ($LASTEXITCODE -ne 0) { throw "git checkout failed for $($source.id)" }

    if ($source.id -eq "nemo-speech.cpp") {
        git -C $target submodule update --init --recursive --depth 1
        if ($LASTEXITCODE -ne 0) { throw "git submodule update failed for $($source.id)" }
    }

    $actual = git -C $target rev-parse HEAD
    if ($actual -ne $source.revision) {
        throw "Revision mismatch for $($source.id): expected $($source.revision), got $actual"
    }
    Write-Host "$($source.id): $actual"
}

