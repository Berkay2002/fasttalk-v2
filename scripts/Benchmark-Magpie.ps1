[CmdletBinding()]
param(
    [string]$Output = "artifacts/feasibility/magpie-benchmark.json",
    [int]$Port = 18081,
    [int]$Samples = 20
)

$ErrorActionPreference = "Stop"
$workspace = Split-Path -Parent $PSScriptRoot
$outputPath = Join-Path $workspace $Output
$artifactDir = Split-Path -Parent $outputPath
New-Item -ItemType Directory -Force -Path $artifactDir | Out-Null
$env:Path = "$(Join-Path $workspace 'runtime/cuda-13.3');$env:Path"
$stderr = Join-Path $artifactDir "magpie-benchmark.stderr.log"
$process = Start-Process -FilePath (Join-Path $workspace "runtime/asr/nemo-speech.exe") `
    -ArgumentList @(
        "serve",
        "--tts-model", (Join-Path $workspace ".cache/models/magpie-tts/magpie_tts_multilingual_357m.v2602.f16.gguf"),
        "--codec-model", (Join-Path $workspace ".cache/models/nano-codec/nemo_nano_codec_22khz_1.89kbps_21.5fps.decoder.f16.gguf"),
        "--tokenizer-dir", (Join-Path $workspace ".cache/models/magpie-tts/extracted"),
        "--host", "127.0.0.1", "--port", $Port.ToString(), "--no-ui"
    ) -PassThru -WindowStyle Hidden -RedirectStandardError $stderr `
    -RedirectStandardOutput (Join-Path $artifactDir "magpie-benchmark.stdout.log")

$client = [System.Net.Http.HttpClient]::new()
$client.Timeout = [TimeSpan]::FromMinutes(2)
$endpoint = "http://127.0.0.1:$Port/v1/audio/speech"

function Wait-Ready {
    for ($attempt = 0; $attempt -lt 120; $attempt++) {
        if ($process.HasExited) {
            throw "nemo-speech exited $($process.ExitCode): $(Get-Content -Raw $stderr)"
        }
        try {
            $ready = Invoke-RestMethod "http://127.0.0.1:$Port/ready" -TimeoutSec 2
            if ($ready.ready -eq $true) { return }
        } catch {
            # Readiness failures are expected during model loading.
        }
        Start-Sleep -Seconds 1
    }
    throw "nemo-speech readiness timed out."
}

function Invoke-Synthesis([string]$Text, [bool]$CancelAfterFirstChunk) {
    $payload = @{
        input = $Text
        voice = "0"
        language = "en-US"
        response_format = "pcm"
    } | ConvertTo-Json -Compress
    $request = [System.Net.Http.HttpRequestMessage]::new(
        [System.Net.Http.HttpMethod]::Post,
        $endpoint
    )
    $request.Content = [System.Net.Http.StringContent]::new(
        $payload,
        [System.Text.Encoding]::UTF8,
        "application/json"
    )
    $timer = [System.Diagnostics.Stopwatch]::StartNew()
    $response = $client.SendAsync(
        $request,
        [System.Net.Http.HttpCompletionOption]::ResponseHeadersRead
    ).GetAwaiter().GetResult()
    $null = $response.EnsureSuccessStatusCode()
    $stream = $response.Content.ReadAsStream()
    $buffer = [byte[]]::new(8192)
    try {
        $firstRead = $stream.Read($buffer, 0, $buffer.Length)
        $firstAudioMs = [math]::Round($timer.Elapsed.TotalMilliseconds, 3)
        if ($firstRead -le 0) { throw "TTS response ended before audio arrived." }
        if ($CancelAfterFirstChunk) {
            $cancelTimer = [System.Diagnostics.Stopwatch]::StartNew()
            $response.Dispose()
            $cancelTimer.Stop()
            return [ordered]@{
                firstAudioMs = $firstAudioMs
                cancelMs = [math]::Round($cancelTimer.Elapsed.TotalMilliseconds, 3)
            }
        }

        $bytes = [long]$firstRead
        while (($read = $stream.Read($buffer, 0, $buffer.Length)) -gt 0) { $bytes += $read }
        $timer.Stop()
        $audioDurationSeconds = $bytes / (22050.0 * 2.0)
        return [ordered]@{
            firstAudioMs = $firstAudioMs
            realTimeFactor = [math]::Round(($timer.Elapsed.TotalSeconds / $audioDurationSeconds), 4)
        }
    } finally {
        $stream.Dispose()
        $response.Dispose()
        $request.Dispose()
    }
}

try {
    Wait-Ready
    $null = Invoke-Synthesis "Warm up the FastTalk speech pipeline." $false

    $measurements = for ($sample = 0; $sample -lt $Samples; $sample++) {
        Invoke-Synthesis "FastTalk keeps speech local, responsive, and interruptible." $false
    }
    $cancellations = for ($sample = 0; $sample -lt $Samples; $sample++) {
        Invoke-Synthesis "This response is intentionally cancelled after its first audio chunk." $true
    }

    $report = [ordered]@{
        schemaVersion = 1
        profile = "magpie-v2602-f16-nanocodec-f16"
        firstAudioMs = @($measurements | ForEach-Object firstAudioMs)
        realTimeFactor = @($measurements | ForEach-Object realTimeFactor)
        responseCancellationMs = @($cancellations | ForEach-Object cancelMs)
    }
    $report | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $outputPath -Encoding utf8
    $report | ConvertTo-Json -Depth 5
} finally {
    $client.Dispose()
    if (-not $process.HasExited) { $process.Kill($true) }
    $process.WaitForExit()
}
