[CmdletBinding()]
param(
    [string]$Output = "artifacts/feasibility/kokoro-fallback-transport.json",
    [int]$Port = 18082
)

$ErrorActionPreference = "Stop"
$workspace = Split-Path -Parent $PSScriptRoot
$outputPath = Join-Path $workspace $Output
$artifactDir = Split-Path -Parent $outputPath
New-Item -ItemType Directory -Force -Path $artifactDir | Out-Null
$stderr = Join-Path $artifactDir "kokoro-fallback.stderr.log"
$stdout = Join-Path $artifactDir "kokoro-fallback.stdout.log"
$process = Start-Process -FilePath (Join-Path $workspace "runtime/tts/kokoro-worker.exe") `
    -ArgumentList @(
        "--model-dir", (Join-Path $workspace ".cache/models/kokoro-sherpa"),
        "--host", "127.0.0.1", "--port", $Port.ToString(), "--threads", "4"
    ) -PassThru -WindowStyle Hidden -RedirectStandardError $stderr -RedirectStandardOutput $stdout

$client = [System.Net.Http.HttpClient]::new()
$client.Timeout = [TimeSpan]::FromMinutes(2)
$endpoint = "http://127.0.0.1:$Port/v1/audio/speech"

function Wait-Ready {
    for ($attempt = 0; $attempt -lt 120; $attempt++) {
        if ($process.HasExited) {
            throw "Kokoro worker exited $($process.ExitCode): $(Get-Content -Raw $stderr)"
        }
        try {
            $ready = Invoke-RestMethod "http://127.0.0.1:$Port/ready" -TimeoutSec 2
            if ($ready.ready -eq $true) { return $ready }
        } catch {
            # Model loading makes readiness fail briefly.
        }
        Start-Sleep -Milliseconds 100
    }
    throw "Kokoro worker readiness timed out."
}

function New-SpeechRequest([string]$Text) {
    $payload = @{
        input = $Text
        voice = "10"
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

function Read-Speech([string]$Text) {
    $request = New-SpeechRequest $Text
    $timer = [System.Diagnostics.Stopwatch]::StartNew()
    $response = $client.SendAsync(
        $request,
        [System.Net.Http.HttpCompletionOption]::ResponseHeadersRead
    ).GetAwaiter().GetResult()
    $null = $response.EnsureSuccessStatusCode()
    $headersMs = [math]::Round($timer.Elapsed.TotalMilliseconds, 3)
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
    $pcmBytes = ($reads | ForEach-Object { [int64]$_["bytes"] } | Measure-Object -Sum).Sum
    return [ordered]@{
        responseHeadersMs = $headersMs
        firstPcmMs = $reads[0].atMs
        completedMs = [math]::Round($timer.Elapsed.TotalMilliseconds, 3)
        readCount = $reads.Count
        pcmBytes = $pcmBytes
    }
}

try {
    $ready = Wait-Ready
    $null = Read-Speech "Warm up the CPU speech fallback."
    $measurement = Read-Speech "FastTalk uses this CPU voice when GPU memory is constrained."
    $report = [ordered]@{
        schemaVersion = 1
        backend = "kokoro-82m-int8-cpu"
        transport = "phrase-level-pcm"
        sampleRate = $ready.sample_rate
        speakers = $ready.speakers
        responseHeadersMs = $measurement.responseHeadersMs
        firstPcmMs = $measurement.firstPcmMs
        completedMs = $measurement.completedMs
        readCount = $measurement.readCount
        pcmBytes = $measurement.pcmBytes
    }
    $report | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $outputPath -Encoding utf8
    $report | ConvertTo-Json -Depth 4
} finally {
    $client.Dispose()
    if (-not $process.HasExited) { $process.Kill($true) }
    $process.WaitForExit()
}
