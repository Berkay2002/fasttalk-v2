[CmdletBinding()]
param(
    [string]$Output = "artifacts/feasibility/magpie-streaming-transport.json",
    [int]$Port = 18081
)

$ErrorActionPreference = "Stop"
$workspace = Split-Path -Parent $PSScriptRoot
$outputPath = Join-Path $workspace $Output
$artifactDir = Split-Path -Parent $outputPath
New-Item -ItemType Directory -Force -Path $artifactDir | Out-Null
$env:Path = "$(Join-Path $workspace 'runtime/cuda-13.3');$env:Path"
$stderr = Join-Path $artifactDir "magpie-streaming-transport.stderr.log"
$process = Start-Process -FilePath (Join-Path $workspace "runtime/asr/nemo-speech.exe") `
    -ArgumentList @(
        "serve",
        "--tts-model", (Join-Path $workspace ".cache/models/magpie-tts/magpie_tts_multilingual_357m.v2602.f16.gguf"),
        "--codec-model", (Join-Path $workspace ".cache/models/nano-codec/nemo_nano_codec_22khz_1.89kbps_21.5fps.decoder.f16.gguf"),
        "--tokenizer-dir", (Join-Path $workspace ".cache/models/magpie-tts/extracted"),
        "--host", "127.0.0.1", "--port", $Port.ToString(), "--no-ui"
    ) -PassThru -WindowStyle Hidden -RedirectStandardError $stderr `
    -RedirectStandardOutput (Join-Path $artifactDir "magpie-streaming-transport.stdout.log")

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
            # Model loading is expected to make readiness fail temporarily.
        }
        Start-Sleep -Milliseconds 250
    }
    throw "nemo-speech readiness timed out."
}

function New-SpeechRequest([string]$Text) {
    $payload = @{
        input = $Text
        voice = "0"
        language = "en-US"
        sample_rate = 22050
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
    return $request
}

try {
    Wait-Ready
    $warmup = New-SpeechRequest "Warm up the streaming speech transport."
    $warmupResponse = $client.SendAsync($warmup).GetAwaiter().GetResult()
    $null = $warmupResponse.EnsureSuccessStatusCode()
    $warmupResponse.Dispose()
    $warmup.Dispose()

    $request = New-SpeechRequest "FastTalk begins playing this clause while the remaining speech is still being synthesized. This second sentence makes the streaming interval directly measurable."
    $timer = [System.Diagnostics.Stopwatch]::StartNew()
    $response = $client.SendAsync(
        $request,
        [System.Net.Http.HttpCompletionOption]::ResponseHeadersRead
    ).GetAwaiter().GetResult()
    $null = $response.EnsureSuccessStatusCode()
    $headersMs = [math]::Round($timer.Elapsed.TotalMilliseconds, 3)
    $chunked = $response.Headers.TransferEncodingChunked -eq $true
    $stream = $response.Content.ReadAsStream()
    $buffer = [byte[]]::new(4096)
    $reads = [System.Collections.Generic.List[object]]::new()
    try {
        while (($read = $stream.Read($buffer, 0, $buffer.Length)) -gt 0) {
            $reads.Add([ordered]@{
                atMs = [math]::Round($timer.Elapsed.TotalMilliseconds, 3)
                bytes = $read
            })
        }
    } finally {
        $stream.Dispose()
        $response.Dispose()
        $request.Dispose()
    }
    $timer.Stop()
    $completeMs = [math]::Round($timer.Elapsed.TotalMilliseconds, 3)

    $cancelRequest = New-SpeechRequest "This deliberately long response proves that dropping the PCM stream cancels native synthesis before the sentence can finish speaking."
    $cancelResponse = $client.SendAsync(
        $cancelRequest,
        [System.Net.Http.HttpCompletionOption]::ResponseHeadersRead
    ).GetAwaiter().GetResult()
    $null = $cancelResponse.EnsureSuccessStatusCode()
    $cancelStream = $cancelResponse.Content.ReadAsStream()
    $null = $cancelStream.Read($buffer, 0, $buffer.Length)
    $cancelTimer = [System.Diagnostics.Stopwatch]::StartNew()
    $cancelStream.Dispose()
    $cancelResponse.Dispose()
    $cancelRequest.Dispose()
    $cancelTimer.Stop()

    if (-not $chunked) { throw "PCM response did not use HTTP chunked transfer." }
    if ($reads.Count -lt 2) { throw "PCM response did not produce multiple client reads." }
    if ($reads[0].atMs -ge $completeMs) { throw "First PCM arrived only after synthesis completed." }

    $report = [ordered]@{
        schemaVersion = 1
        transferEncodingChunked = $chunked
        responseHeadersMs = $headersMs
        firstPcmMs = $reads[0].atMs
        completedMs = $completeMs
        readCount = $reads.Count
        reads = $reads
        responseDisposeMs = [math]::Round($cancelTimer.Elapsed.TotalMilliseconds, 3)
    }
    $report | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $outputPath -Encoding utf8
    $report | ConvertTo-Json -Depth 5
} finally {
    $client.Dispose()
    if (-not $process.HasExited) { $process.Kill($true) }
    $process.WaitForExit()
}
