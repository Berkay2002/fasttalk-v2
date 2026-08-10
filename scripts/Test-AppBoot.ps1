[CmdletBinding()]
param(
    [int]$TimeoutSeconds = 120
)

$ErrorActionPreference = "Stop"
$workspace = Split-Path -Parent $PSScriptRoot
$appRoot = Join-Path $workspace "app"
$node = Get-ChildItem (Join-Path $workspace ".cache/toolchains") -Directory `
    -Filter "node-v*-win-x64" | Sort-Object Name -Descending | Select-Object -First 1
if (-not $node) { throw "Project Node runtime not found under .cache/toolchains." }
$env:Path = "$($node.FullName);$env:Path"
$artifactDir = Join-Path $workspace "artifacts/feasibility"
$stdout = Join-Path $artifactDir "tauri-boot.stdout.log"
$stderr = Join-Path $artifactDir "tauri-boot.stderr.log"
$existingIds = @(Get-Process "fasttalk-app" -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Id)
$existingNodeIds = @(Get-Process "node" -ErrorAction SilentlyContinue |
    Where-Object Path -Like "$($node.FullName)*" |
    Select-Object -ExpandProperty Id)
$runner = Start-Process -FilePath (Join-Path $node.FullName "npm.cmd") `
    -ArgumentList @("run", "tauri", "--", "dev", "--no-watch") `
    -WorkingDirectory $appRoot -PassThru -WindowStyle Hidden `
    -RedirectStandardOutput $stdout -RedirectStandardError $stderr
$app = $null

try {
    $timer = [System.Diagnostics.Stopwatch]::StartNew()
    while ($timer.Elapsed.TotalSeconds -lt $TimeoutSeconds) {
        if ($runner.HasExited) {
            throw "Tauri dev runner exited $($runner.ExitCode): $(Get-Content -Raw $stderr)"
        }
        $app = Get-Process "fasttalk-app" -ErrorAction SilentlyContinue |
            Where-Object Id -NotIn $existingIds |
            Select-Object -First 1
        if ($app -and $app.MainWindowHandle -ne 0) {
            Write-Host "FastTalk window booted (PID $($app.Id), title '$($app.MainWindowTitle)')."
            exit 0
        }
        Start-Sleep -Milliseconds 500
    }
    throw "FastTalk window did not appear within $TimeoutSeconds seconds."
} finally {
    if ($app -and -not $app.HasExited) { $app.Kill($true) }
    if (-not $runner.HasExited) { $runner.Kill($true) }
    Get-Process "node" -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -like "$($node.FullName)*" -and $_.Id -notin $existingNodeIds } |
        ForEach-Object { $_.Kill($true) }
    if ($app) { $app.WaitForExit() }
    $runner.WaitForExit()
}
