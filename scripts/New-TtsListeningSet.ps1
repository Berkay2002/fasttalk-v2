[CmdletBinding()]
param(
    [string]$OutputDirectory = "artifacts/matrix/tts-listening",
    [int]$MagpiePort = 18081,
    [int]$KokoroPort = 18082
)

$ErrorActionPreference = "Stop"
$workspace = Split-Path -Parent $PSScriptRoot
$output = Join-Path $workspace $OutputDirectory
New-Item -ItemType Directory -Force -Path $output | Out-Null
$env:Path = "$(Join-Path $workspace 'runtime/cuda-13.3');$env:Path"
$client = [System.Net.Http.HttpClient]::new()
$client.Timeout = [TimeSpan]::FromMinutes(2)

$magpie = Start-Process -FilePath (Join-Path $workspace "runtime/asr/nemo-speech.exe") `
    -ArgumentList @(
        "serve",
        "--tts-model", (Join-Path $workspace ".cache/models/magpie-tts/magpie_tts_multilingual_357m.v2602.f16.gguf"),
        "--codec-model", (Join-Path $workspace ".cache/models/nano-codec/nemo_nano_codec_22khz_1.89kbps_21.5fps.decoder.f16.gguf"),
        "--tokenizer-dir", (Join-Path $workspace ".cache/models/magpie-tts/extracted"),
        "--host", "127.0.0.1", "--port", $MagpiePort.ToString(), "--no-ui"
    ) -PassThru -WindowStyle Hidden `
    -RedirectStandardOutput (Join-Path $output "magpie.stdout.log") `
    -RedirectStandardError (Join-Path $output "magpie.stderr.log")
$kokoro = Start-Process -FilePath (Join-Path $workspace "runtime/tts/kokoro-worker.exe") `
    -ArgumentList @(
        "--model-dir", (Join-Path $workspace ".cache/models/kokoro-sherpa-v1.0"),
        "--host", "127.0.0.1", "--port", $KokoroPort.ToString(), "--threads", "4"
    ) -PassThru -WindowStyle Hidden `
    -RedirectStandardOutput (Join-Path $output "kokoro.stdout.log") `
    -RedirectStandardError (Join-Path $output "kokoro.stderr.log")

function Wait-Ready([string]$Uri, [System.Diagnostics.Process]$Process, [string]$Name) {
    for ($attempt = 0; $attempt -lt 180; $attempt++) {
        if ($Process.HasExited) { throw "$Name exited with code $($Process.ExitCode)" }
        try {
            $ready = Invoke-RestMethod $Uri -TimeoutSec 2
            if ($ready.ready -eq $true) { return $ready }
        } catch {
            # Model loading makes readiness fail briefly.
        }
        Start-Sleep -Milliseconds 500
    }
    throw "$Name readiness timed out"
}

function Get-Pcm([string]$Uri, [string]$Text, [string]$Voice) {
    $payload = @{
        input = $Text
        voice = $Voice
        language = "en-US"
        response_format = "pcm"
    } | ConvertTo-Json -Compress
    $content = [System.Net.Http.StringContent]::new(
        $payload,
        [Text.Encoding]::UTF8,
        "application/json"
    )
    $response = $client.PostAsync($Uri, $content).GetAwaiter().GetResult()
    try {
        $null = $response.EnsureSuccessStatusCode()
        return $response.Content.ReadAsByteArrayAsync().GetAwaiter().GetResult()
    } finally {
        $response.Dispose()
        $content.Dispose()
    }
}

function Write-PcmWav([string]$Path, [byte[]]$Pcm, [int]$SampleRate) {
    $stream = [System.IO.File]::Create($Path)
    $writer = [System.IO.BinaryWriter]::new($stream)
    try {
        $writer.Write([Text.Encoding]::ASCII.GetBytes("RIFF"))
        $writer.Write([int](36 + $Pcm.Length))
        $writer.Write([Text.Encoding]::ASCII.GetBytes("WAVEfmt "))
        $writer.Write([int]16)
        $writer.Write([int16]1)
        $writer.Write([int16]1)
        $writer.Write([int]$SampleRate)
        $writer.Write([int]($SampleRate * 2))
        $writer.Write([int16]2)
        $writer.Write([int16]16)
        $writer.Write([Text.Encoding]::ASCII.GetBytes("data"))
        $writer.Write([int]$Pcm.Length)
        $writer.Write($Pcm)
    } finally {
        $writer.Dispose()
        $stream.Dispose()
    }
}

$prompts = @(
    [ordered]@{ id = "numbers-names"; text = "At 7:45 p.m., Doctor Nguyen bought three GPUs for 1,299 dollars each." },
    [ordered]@{ id = "prosody"; text = "Wait... did you really say zero point zero five? I thought you said five." },
    [ordered]@{ id = "technical"; text = "FastTalk keeps transcription, language generation, and speech synthesis on this computer while responding one clause at a time." }
)

try {
    $magpieReady = Wait-Ready "http://127.0.0.1:$MagpiePort/ready" $magpie "Magpie"
    $kokoroReady = Wait-Ready "http://127.0.0.1:$KokoroPort/ready" $kokoro "Kokoro"
    $sheet = @()
    $key = @()
    for ($index = 0; $index -lt $prompts.Count; $index++) {
        $prompt = $prompts[$index]
        $magpiePcm = Get-Pcm "http://127.0.0.1:$MagpiePort/v1/audio/speech" $prompt.text "0"
        $kokoroPcm = Get-Pcm "http://127.0.0.1:$KokoroPort/v1/audio/speech" $prompt.text "10"
        $magpieLabel = if ($index % 2 -eq 0) { "A" } else { "B" }
        $kokoroLabel = if ($magpieLabel -eq "A") { "B" } else { "A" }
        Write-PcmWav (Join-Path $output "$($prompt.id)-$magpieLabel.wav") $magpiePcm 22050
        Write-PcmWav (Join-Path $output "$($prompt.id)-$kokoroLabel.wav") $kokoroPcm ([int]$kokoroReady.sample_rate)
        $sheet += [ordered]@{
            id = $prompt.id
            prompt = $prompt.text
            sampleA = "$($prompt.id)-A.wav"
            sampleB = "$($prompt.id)-B.wav"
            preferred = $null
            pronunciationNotes = $null
        }
        $key += [ordered]@{ id = $prompt.id; magpie = $magpieLabel; kokoro = $kokoroLabel }
    }
    [ordered]@{
        schemaVersion = 1
        instructions = "Listen without opening answer-key.json. Record the preferred sample and any pronunciation defects."
        cases = $sheet
    } | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $output "listening-sheet.json") -Encoding utf8
    [ordered]@{
        schemaVersion = 1
        magpieProfile = "magpie-v2602-f16-nanocodec-f16"
        kokoroProfile = "kokoro-82m-int8-cpu"
        assignments = $key
    } | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $output "answer-key.json") -Encoding utf8
    Write-Host "TTS listening set written to $output"
} finally {
    $client.Dispose()
    foreach ($process in @($kokoro, $magpie)) {
        if ($process -and -not $process.HasExited) { $process.Kill($true) }
        if ($process) { $process.WaitForExit() }
    }
}
