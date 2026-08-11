[CmdletBinding()]
param(
    [string]$Model = ".cache/models/qwen3.6-27b/Qwen3.6-27B-Q4_K_M.gguf",
    [string]$Profile = "qwen3.6-27b-q4-k-m-16k-non-thinking",
    [string]$Output = "artifacts/feasibility/llama-benchmark.json",
    [int]$Port = 18080,
    [int]$ContextSize = 16384,
    [int]$Parallel = 1,
    [int]$WarmSamples = 20,
    [int]$ThroughputSamples = 5
)

$ErrorActionPreference = "Stop"
$workspace = Split-Path -Parent $PSScriptRoot
$server = Join-Path $workspace "runtime/llm/llama-server.exe"
$modelPath = (Resolve-Path (Join-Path $workspace $Model)).Path
$outputPath = Join-Path $workspace $Output
$artifactDir = Split-Path -Parent $outputPath
New-Item -ItemType Directory -Force -Path $artifactDir | Out-Null
$stdout = Join-Path $artifactDir "llama-benchmark.stdout.log"
$stderr = Join-Path $artifactDir "llama-benchmark.stderr.log"
$env:Path = "$(Join-Path $workspace 'runtime/cuda-13.3');$env:Path"

$arguments = @(
    "--model", $modelPath,
    "--ctx-size", $ContextSize.ToString(),
    "--parallel", $Parallel.ToString(),
    "--gpu-layers", "all",
    "--flash-attn", "on",
    "--reasoning", "off",
    "--host", "127.0.0.1",
    "--port", $Port.ToString(),
    "--no-webui",
    "--metrics"
)
$process = Start-Process -FilePath $server -ArgumentList $arguments -PassThru `
    -WindowStyle Hidden -RedirectStandardOutput $stdout -RedirectStandardError $stderr

$client = [System.Net.Http.HttpClient]::new()
$client.Timeout = [TimeSpan]::FromMinutes(2)
$endpoint = "http://127.0.0.1:$Port/v1/chat/completions"

function Wait-Ready {
    for ($attempt = 0; $attempt -lt 120; $attempt++) {
        if ($process.HasExited) {
            throw "llama-server exited $($process.ExitCode): $(Get-Content -Raw $stderr)"
        }
        try {
            $health = Invoke-RestMethod "http://127.0.0.1:$Port/health" -TimeoutSec 2
            if ($health.status -eq "ok") { return }
        } catch {
            # Readiness failures are expected while the model loads.
        }
        Start-Sleep -Seconds 1
    }
    throw "llama-server readiness timed out."
}

function Measure-FirstToken {
    $payload = @{
        model = "fasttalk-local"
        messages = @(@{ role = "user"; content = "Reply with exactly: ready" })
        max_tokens = 8
        temperature = 0
        stream = $true
    } | ConvertTo-Json -Depth 5 -Compress
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
    $reader = [System.IO.StreamReader]::new($stream)
    try {
        while (-not $reader.EndOfStream) {
            $line = $reader.ReadLine()
            if (-not $line.StartsWith("data: ") -or $line -eq "data: [DONE]") { continue }
            $event = $line.Substring(6) | ConvertFrom-Json
            $content = $event.choices[0].delta.content
            if ($content) {
                $timer.Stop()
                return [math]::Round($timer.Elapsed.TotalMilliseconds, 3)
            }
        }
    } finally {
        $reader.Dispose()
        $response.Dispose()
        $request.Dispose()
    }
    throw "Streaming response ended before a content token arrived."
}

function Invoke-Completion([object[]]$Messages, [int]$MaxTokens = 64) {
    $payload = @{
        model = "fasttalk-local"
        messages = $Messages
        max_tokens = $MaxTokens
        temperature = 0
        seed = 42
    } | ConvertTo-Json -Depth 8 -Compress
    $response = Invoke-RestMethod $endpoint -Method Post -ContentType "application/json" `
        -Body $payload -TimeoutSec 120
    return [string]$response.choices[0].message.content
}

try {
    Wait-Ready

    $warmup = @{
        model = "fasttalk-local"
        messages = @(@{ role = "user"; content = "Reply with exactly: ready" })
        max_tokens = 8
        temperature = 0
    } | ConvertTo-Json -Depth 5
    Invoke-RestMethod $endpoint -Method Post -ContentType "application/json" `
        -Body $warmup -TimeoutSec 120 | Out-Null

    $ttft = for ($sample = 0; $sample -lt $WarmSamples; $sample++) {
        Measure-FirstToken
    }

    $throughput = for ($sample = 0; $sample -lt $ThroughputSamples; $sample++) {
        $payload = @{
            model = "fasttalk-local"
            messages = @(@{
                role = "user"
                content = "Write one compact paragraph about local speech software. Use 100 words."
            })
            max_tokens = 160
            temperature = 0.7
            seed = 1000 + $sample
        } | ConvertTo-Json -Depth 5
        $response = Invoke-RestMethod $endpoint -Method Post -ContentType "application/json" `
            -Body $payload -TimeoutSec 120
        [math]::Round($response.timings.predicted_per_second, 3)
    }

    $qualityCases = @(
        [ordered]@{
            id = "exact-instruction"
            response = Invoke-Completion @(@{ role = "user"; content = "Reply with exactly FASTTALK_OK and nothing else." }) 16
            expected = "FASTTALK_OK"
            method = "exact"
        },
        [ordered]@{
            id = "conversation-recall"
            response = Invoke-Completion @(
                @{ role = "user"; content = "Remember that my project codeword is amber-17." },
                @{ role = "assistant"; content = "Understood." },
                @{ role = "user"; content = "What is the project codeword? Reply with only the codeword." }
            ) 16
            expected = "amber-17"
            method = "exact-case-insensitive"
        },
        [ordered]@{
            id = "format-constraint"
            response = Invoke-Completion @(@{ role = "user"; content = "Return exactly three lowercase color names separated by commas, with no spaces or other text." }) 24
            expected = "^[a-z]+,[a-z]+,[a-z]+$"
            method = "regex"
        },
        [ordered]@{
            id = "multi-step-arithmetic"
            response = Invoke-Completion @(@{ role = "user"; content = "A store has 73 units, sells 28, receives 14, then discards 9. Reply with only the final unit count." }) 16
            expected = "50"
            method = "exact"
        },
        [ordered]@{
            id = "latest-conversation-state"
            response = Invoke-Completion @(
                @{ role = "user"; content = "The deployment region is north-3." },
                @{ role = "assistant"; content = "Understood." },
                @{ role = "user"; content = "Correction: the deployment region is west-7." },
                @{ role = "assistant"; content = "Updated." },
                @{ role = "user"; content = "What is the current deployment region? Reply with only the region." }
            ) 16
            expected = "west-7"
            method = "exact-case-insensitive"
        },
        [ordered]@{
            id = "json-contract"
            response = Invoke-Completion @(@{ role = "user"; content = 'Return one JSON object with exactly two fields: string field "status" equal to "ready", and integer field "count" equal to 3. No Markdown.' }) 32
            expected = '{"status":"ready","count":3}'
            method = "json-contract"
        }
    )
    $effectiveContextPerSlot = [math]::Floor($ContextSize / $Parallel)
    if ($effectiveContextPerSlot -ge 16384) {
        $needle = "cobalt-42"
        $longContext = "The recovery code is $needle. " + ("filler " * 12000) +
            "What is the recovery code? Reply with only the code."
        $qualityCases += [ordered]@{
            id = "long-context-recall"
            response = Invoke-Completion @(@{ role = "user"; content = $longContext }) 16
            expected = $needle
            method = "exact-case-insensitive"
        }
    }
    foreach ($case in $qualityCases) {
        $normalized = $case.response.Trim()
        $case.pass = switch ($case.method) {
            "exact" { $normalized -ceq $case.expected }
            "exact-case-insensitive" { $normalized -ieq $case.expected }
            "regex" { $normalized -cmatch $case.expected }
            "json-contract" {
                try {
                    $parsed = $normalized | ConvertFrom-Json
                    $properties = @($parsed.PSObject.Properties.Name)
                    $properties.Count -eq 2 -and
                        $properties -contains "status" -and
                        $properties -contains "count" -and
                        $parsed.status -ceq "ready" -and
                        $parsed.count -eq 3
                } catch { $false }
            }
        }
    }

    $memoryUsed = [double](nvidia-smi --query-gpu=memory.used --format=csv,noheader,nounits | Select-Object -First 1)
    $report = [ordered]@{
        schemaVersion = 1
        profile = $Profile
        model = [IO.Path]::GetRelativePath($workspace, $modelPath).Replace('\', '/')
        contextSize = $ContextSize
        parallel = $Parallel
        effectiveContextPerSlot = $effectiveContextPerSlot
        warmLlmFirstTokenMs = @($ttft)
        generationTokensPerSecond = @($throughput)
        qualityCases = @($qualityCases)
        qualityPassCount = @($qualityCases | Where-Object pass).Count
        qualityCaseCount = $qualityCases.Count
        warmedGpuMemoryMiB = $memoryUsed
    }
    $report | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $outputPath -Encoding utf8
    $report | ConvertTo-Json -Depth 5
} finally {
    $client.Dispose()
    if (-not $process.HasExited) { $process.Kill($true) }
    $process.WaitForExit()
}
