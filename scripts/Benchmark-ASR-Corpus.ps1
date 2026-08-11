[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string]$Model,
    [Parameter(Mandatory)]
    [string]$Profile,
    [Parameter(Mandatory)]
    [string]$Output,
    [switch]$Offline
)

$ErrorActionPreference = "Stop"
$workspace = Split-Path -Parent $PSScriptRoot
$executable = Join-Path $workspace "runtime/asr/nemo-speech.exe"
$modelPath = (Resolve-Path (Join-Path $workspace $Model)).Path
$fixtures = Join-Path $workspace "tests/fixtures/audio"
$outputPath = Join-Path $workspace $Output
$artifactDir = Split-Path -Parent $outputPath
$transcriptDir = Join-Path $artifactDir "$([IO.Path]::GetFileNameWithoutExtension($Output))-transcripts"
New-Item -ItemType Directory -Force -Path $artifactDir, $transcriptDir | Out-Null
$stderr = Join-Path $artifactDir "$([IO.Path]::GetFileNameWithoutExtension($Output)).stderr.log"
$env:Path = "$(Join-Path $workspace 'runtime/cuda-13.3');$env:Path"

$expected = [ordered]@{
    "quiet-speech" = "Could you please lower the lights in this room"
    "hesitation" = "Um I think we should wait for a moment before deciding"
    "short-acknowledgement" = "Yes"
    "long-question" = "Could you explain how a local voice assistant keeps the microphone transcription language model and speech synthesis private while still responding quickly"
    "background-noise" = "Please tell me whether the train will arrive before noon"
    "speaker-playback" = "Can you hear my question clearly"
}

function Get-Words([string]$Text) {
    $normalized = $Text.ToLowerInvariant() -replace "[^a-z0-9']", " "
    return @($normalized -split "\s+" | Where-Object { $_ })
}

function Get-EditDistance([string[]]$Reference, [string[]]$Hypothesis) {
    $previous = 0..$Hypothesis.Count
    for ($i = 1; $i -le $Reference.Count; $i++) {
        $current = [int[]]::new($Hypothesis.Count + 1)
        $current[0] = $i
        for ($j = 1; $j -le $Hypothesis.Count; $j++) {
            $substitution = if ($Reference[$i - 1] -ceq $Hypothesis[$j - 1]) { 0 } else { 1 }
            $current[$j] = [math]::Min(
                [math]::Min($current[$j - 1] + 1, $previous[$j] + 1),
                $previous[$j - 1] + $substitution
            )
        }
        $previous = $current
    }
    return $previous[$Hypothesis.Count]
}

$arguments = @(
    "transcribe", $fixtures,
    "--model", $modelPath,
    "--device", "cuda:0",
    "--format", "json",
    "--output-dir", $transcriptDir,
    "--force"
)
if (-not $Offline) { $arguments += "--stream" }
& $executable @arguments 2> $stderr | Out-Null
if ($LASTEXITCODE -ne 0) {
    throw "ASR corpus benchmark failed ($LASTEXITCODE): $(Get-Content -Raw $stderr)"
}

$totalErrors = 0
$totalWords = 0
$cases = foreach ($entry in $expected.GetEnumerator()) {
    $resultPath = Join-Path $transcriptDir "$($entry.Key).json"
    $result = Get-Content -Raw -LiteralPath $resultPath | ConvertFrom-Json
    $referenceWords = @(Get-Words $entry.Value)
    $hypothesisWords = @(Get-Words $result.text)
    $errors = Get-EditDistance $referenceWords $hypothesisWords
    $totalErrors += $errors
    $totalWords += $referenceWords.Count
    [ordered]@{
        fixture = "$($entry.Key).wav"
        expected = $entry.Value
        actual = $result.text
        wordErrors = $errors
        referenceWords = $referenceWords.Count
        wordErrorRate = [math]::Round($errors / $referenceWords.Count, 4)
    }
}

$report = [ordered]@{
    schemaVersion = 1
    profile = $Profile
    model = [IO.Path]::GetRelativePath($workspace, $modelPath).Replace('\', '/')
    mode = if ($Offline) { "offline" } else { "stream" }
    corpus = "six prerecorded synthetic acoustic fixtures"
    fixtureCount = $cases.Count
    referenceWords = $totalWords
    wordErrors = $totalErrors
    wordErrorRate = [math]::Round($totalErrors / $totalWords, 4)
    cases = @($cases)
    limitation = "This small deterministic regression corpus is useful for cross-profile comparison, but it is not a substitute for a public multi-speaker WER benchmark."
}
$report | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $outputPath -Encoding utf8
$report | ConvertTo-Json -Depth 6
