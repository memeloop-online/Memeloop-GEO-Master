<#
.SYNOPSIS
Restores a downloaded Windows/MSVC rusty_v8 CI artifact into Cargo's V8 cache.

.DESCRIPTION
Download the `rusty-v8-msvc-<sha>` artifact from GitHub Actions and pass the
extracted directory to this script. The script verifies every archive listed
in rusty_v8.sha256 before copying the exact set into CARGO_HOME\.rusty_v8. It
intentionally rejects Linux artifacts and never handles credentials.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateScript({ Test-Path -LiteralPath $_ -PathType Container })]
    [string]$ArtifactDirectory,

    [string]$CargoHome = $(if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE ".cargo" })
)

$ErrorActionPreference = "Stop"
$artifactPath = (Resolve-Path -LiteralPath $ArtifactDirectory).Path
$checksumPath = Join-Path $artifactPath "rusty_v8.sha256"
$targetPath = Join-Path $artifactPath "target.txt"
$versionPath = Join-Path $artifactPath "v8-version.txt"
if (-not (Test-Path -LiteralPath $checksumPath -PathType Leaf)) {
    throw "Missing rusty_v8.sha256. Download the rusty-v8-msvc artifact, not the Linux bundle."
}
if ((Get-Content -LiteralPath $targetPath -Raw).Trim() -ne "x86_64-pc-windows-msvc") {
    throw "The artifact is not the supported Windows/MSVC rusty_v8 cache."
}
$v8Version = (Get-Content -LiteralPath $versionPath -Raw).Trim()
if ($v8Version -notmatch '^\d+\.\d+\.\d+$') {
    throw "Missing or invalid v8-version.txt metadata."
}

$archives = [System.Collections.Generic.List[string]]::new()
foreach ($line in Get-Content -LiteralPath $checksumPath) {
    if ($line -notmatch '^(?<hash>[0-9a-fA-F]{64})\s+\*?(?<name>.+)$') {
        throw "Invalid rusty_v8.sha256 entry: $line"
    }
    $expectedHash = $Matches.hash
    $archiveName = $Matches.name.Trim()
    $archivePath = Join-Path $artifactPath $archiveName
    if (-not (Test-Path -LiteralPath $archivePath -PathType Leaf)) {
        throw "The archive named by rusty_v8.sha256 was not found: $archiveName"
    }
    $actualHash = (Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash
    if ($actualHash -ine $expectedHash) {
        throw "SHA-256 mismatch for ${archiveName}: expected $expectedHash, got $actualHash."
    }
    $archives.Add($archivePath)
}

$cachePath = Join-Path $CargoHome ".rusty_v8"
New-Item -ItemType Directory -Force -Path $cachePath | Out-Null
foreach ($archivePath in $archives) {
    $archiveName = Split-Path $archivePath -Leaf
    if ($archiveName -notmatch '^rusty_v8_.+_x86_64-pc-windows-msvc\.lib\.gz$') {
        throw "Unexpected Windows/MSVC rusty_v8 archive name: $archiveName"
    }
    $releaseUrl = "https://github.com/denoland/rusty_v8/releases/download/v$v8Version/$archiveName"
    $cargoCacheName = $releaseUrl -replace '[^A-Za-z0-9]', '_'
    Copy-Item -LiteralPath $archivePath -Destination (Join-Path $cachePath $cargoCacheName) -Force
}
Write-Output "Restored $($archives.Count) verified archive(s) to $cachePath"
