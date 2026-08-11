[CmdletBinding()]
param(
    [string]$Source = (Join-Path $PSScriptRoot '..\fasttalk-v2-results-summary.source.html'),
    [string]$Output = (Join-Path $PSScriptRoot '..\fasttalk-v2-results-summary.html')
)

$ErrorActionPreference = 'Stop'
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$sourcePath = (Resolve-Path $Source).Path
$outputPath = [System.IO.Path]::GetFullPath($Output)
$utf8 = [System.Text.UTF8Encoding]::new($false)

$assets = @{
    '__FASTTALK_MARK__' = Join-Path $repoRoot 'app\src\assets\fasttalk-mark.png'
    '__FASTTALK_SETUP__' = Join-Path $repoRoot 'benchmarks\2026-08-11-rtx3090-matrix\screenshots\fasttalk-setup.png'
    '__FASTTALK_MODELS__' = Join-Path $repoRoot 'benchmarks\2026-08-11-rtx3090-matrix\screenshots\fasttalk-models.png'
    '__FASTTALK_CONVERSATION__' = Join-Path $repoRoot 'benchmarks\2026-08-11-rtx3090-matrix\screenshots\fasttalk-conversation.png'
}

$html = [System.IO.File]::ReadAllText($sourcePath, $utf8)
foreach ($entry in $assets.GetEnumerator()) {
    $assetPath = (Resolve-Path $entry.Value).Path
    $base64 = [Convert]::ToBase64String([System.IO.File]::ReadAllBytes($assetPath))
    $html = $html.Replace($entry.Key, "data:image/png;base64,$base64")
}

$remaining = [regex]::Matches($html, '__FASTTALK_[A-Z_]+__')
if ($remaining.Count -gt 0) {
    throw "Unresolved asset placeholder: $($remaining[0].Value)"
}

[System.IO.File]::WriteAllText($outputPath, $html, $utf8)
$result = Get-Item -LiteralPath $outputPath
[pscustomobject]@{
    Path = $result.FullName
    Bytes = $result.Length
    Sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $result.FullName).Hash.ToLowerInvariant()
}
