[CmdletBinding()]
param(
    [string]$OutputRoot = "artifacts/release/acoustic-scenarios"
)

$ErrorActionPreference = "Stop"
$workspace = Split-Path -Parent $PSScriptRoot
$gate = Join-Path $workspace "target\debug\release-gate.exe"
if (-not (Test-Path -LiteralPath $gate -PathType Leaf)) {
    throw "Build release-gate before running acoustic scenarios"
}

$fixtures = @(
    @{ Name = "quiet-speech.wav"; Expected = "lights" },
    @{ Name = "hesitation.wav"; Expected = "wait|moment" },
    @{ Name = "short-acknowledgement.wav"; Expected = "yes" },
    @{ Name = "long-question.wav"; Expected = "local|voice assistant" },
    @{ Name = "background-noise.wav"; Expected = "train|noon" },
    @{ Name = "speaker-playback.wav"; Expected = "hear|question"; Reference = "tests\fixtures\audio\speaker-playback-reference.wav" }
)
$results = foreach ($fixture in $fixtures) {
    $audio = Join-Path $workspace "tests\fixtures\audio\$($fixture.Name)"
    $output = Join-Path $workspace "$OutputRoot\$([IO.Path]::GetFileNameWithoutExtension($fixture.Name)).json"
    $arguments = @("--turns", "1", "--skip-audio", "--audio", $audio, "--output", $output)
    if ($fixture.Reference) {
        $arguments += @("--reference", (Join-Path $workspace $fixture.Reference))
    }
    $gateOutput = & $gate @arguments
    if ($LASTEXITCODE -ne 0) {
        throw "Acoustic scenario failed: $($fixture.Name)`n$($gateOutput -join [Environment]::NewLine)"
    }
    $evidence = Get-Content -Raw -LiteralPath $output | ConvertFrom-Json
    if (@($evidence.transcripts).Count -ne 1 -or -not $evidence.transcripts[0]) {
        throw "Acoustic scenario returned no transcript: $($fixture.Name)"
    }
    if ($evidence.transcripts[0] -notmatch $fixture.Expected) {
        throw "Acoustic scenario missed '$($fixture.Expected)': $($evidence.transcripts[0])"
    }
    if (@($evidence.synthesizedClauseCounts).Count -ne 1 -or $evidence.synthesizedClauseCounts[0] -lt 2) {
        throw "Acoustic scenario did not synthesize the full two-clause response: $($fixture.Name)"
    }
    [ordered]@{
        fixture = $fixture.Name
        transcript = $evidence.transcripts[0]
        synthesizedClauseCount = $evidence.synthesizedClauseCounts[0]
        capturePreprocessingScope = $evidence.capturePreprocessingScope
        endOfSpeechToFirstAudioMs = $evidence.endOfSpeechToFirstAudioMs[0]
        asrPartialMaximumMs = (@($evidence.asrPartialUpdateMs) | Measure-Object -Maximum).Maximum
    }
}

$summary = Join-Path $workspace "$OutputRoot\summary.json"
$results | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $summary -Encoding utf8
Write-Host "All acoustic scenarios completed: $summary"
