[CmdletBinding()]
param(
    [string]$Output = "artifacts/release/offline-runtime.json"
)

$ErrorActionPreference = "Stop"
$workspace = Split-Path -Parent $PSScriptRoot
$gate = Join-Path $workspace "target\debug\release-gate.exe"
if (-not (Test-Path -LiteralPath $gate -PathType Leaf)) {
    throw "Build release-gate before running the offline test"
}

$client = [Net.Sockets.TcpClient]::new()
try {
    $connection = $client.ConnectAsync("huggingface.co", 443)
    if ($connection.Wait([TimeSpan]::FromSeconds(3)) -and $client.Connected) {
        throw "Network access is still available; disconnect the host before claiming an offline result"
    }
} catch [AggregateException] {
    # A failed connection is the required precondition.
} finally {
    $client.Dispose()
}

$outputPath = Join-Path $workspace $Output
& $gate --turns 1 --skip-audio --output $outputPath
if ($LASTEXITCODE -ne 0) { throw "Offline conversation run failed ($LASTEXITCODE)" }

$evidence = Get-Content -Raw -LiteralPath $outputPath | ConvertFrom-Json
if ($evidence.turns -ne 1 -or @($evidence.transcripts).Count -ne 1) {
    throw "Offline evidence does not contain one completed conversation turn"
}
Write-Host "Offline native conversation passed with external network access unavailable."
