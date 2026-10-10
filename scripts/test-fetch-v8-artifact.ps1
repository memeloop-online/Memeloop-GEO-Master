$ErrorActionPreference = "Stop"
$restoreScript = Join-Path $PSScriptRoot "fetch-v8-artifact.ps1"
$tempRoot = Join-Path ([System.IO.Path]::GetTempPath()) "geo-v8-artifact-test-$([guid]::NewGuid())"
$archiveName = "rusty_v8_simdutf_release_x86_64-pc-windows-msvc.lib.gz"
$expectedCacheName = "https___github_com_denoland_rusty_v8_releases_download_v150_4_0_$($archiveName -replace '[^A-Za-z0-9]', '_')"

function New-Fixture(
    [string]$Root,
    [string]$Target = "x86_64-pc-windows-msvc",
    [string]$Content = "fixture archive"
) {
    New-Item -ItemType Directory -Force -Path $Root | Out-Null
    $archivePath = Join-Path $Root $archiveName
    Set-Content -LiteralPath $archivePath -Value $Content -NoNewline
    $hash = (Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash.ToLowerInvariant()
    Set-Content -LiteralPath (Join-Path $Root "rusty_v8.sha256") -Value "$hash  $archiveName"
    Set-Content -LiteralPath (Join-Path $Root "target.txt") -Value $Target -NoNewline
    Set-Content -LiteralPath (Join-Path $Root "v8-version.txt") -Value "150.4.0" -NoNewline
    Set-Content -LiteralPath (Join-Path $Root "rustc.txt") -Value "rustc 1.96.0 (fixture)" -NoNewline
    return $archivePath
}

function Assert-Throws([scriptblock]$Action, [string]$Name) {
    try {
        & $Action
    } catch {
        return
    }
    throw "Expected rejection: $Name"
}

try {
    $validArtifact = Join-Path $tempRoot "valid"
    $cargoHome = Join-Path $tempRoot "cargo"
    $archivePath = New-Fixture -Root $validArtifact
    & $restoreScript -ArtifactDirectory $validArtifact -CargoHome $cargoHome

    $cachePath = Join-Path $cargoHome ".rusty_v8"
    $cachedArchive = Join-Path $cachePath $expectedCacheName
    if (-not (Test-Path -LiteralPath $cachedArchive -PathType Leaf)) {
        throw "The expected URL-escaped Cargo cache file was not created."
    }
    if ((Get-Content -LiteralPath $cachedArchive -Raw) -ne "fixture archive") {
        throw "Restored archive contents do not match the verified fixture."
    }

    $linuxArtifact = Join-Path $tempRoot "linux"
    New-Fixture -Root $linuxArtifact -Target "x86_64-unknown-linux-gnu" | Out-Null
    $linuxCargoHome = Join-Path $tempRoot "linux-cargo"
    Assert-Throws { & $restoreScript -ArtifactDirectory $linuxArtifact -CargoHome $linuxCargoHome } "Linux target"
    if (Test-Path -LiteralPath (Join-Path $linuxCargoHome ".rusty_v8")) {
        throw "Rejected Linux artifact mutated the Cargo cache."
    }

    $tamperedArtifact = Join-Path $tempRoot "tampered"
    $tamperedArchive = New-Fixture -Root $tamperedArtifact
    Set-Content -LiteralPath $tamperedArchive -Value "modified archive" -NoNewline
    $tamperedCargoHome = Join-Path $tempRoot "tampered-cargo"
    Assert-Throws { & $restoreScript -ArtifactDirectory $tamperedArtifact -CargoHome $tamperedCargoHome } "SHA-256 mismatch"
    if (Test-Path -LiteralPath (Join-Path $tamperedCargoHome ".rusty_v8")) {
        throw "Rejected checksum fixture mutated the Cargo cache."
    }

    $traversalArtifact = Join-Path $tempRoot "traversal"
    New-Fixture -Root $traversalArtifact | Out-Null
    $traversalChecksum = Join-Path $traversalArtifact "rusty_v8.sha256"
    $hash = (Get-FileHash -LiteralPath (Join-Path $traversalArtifact $archiveName) -Algorithm SHA256).Hash.ToLowerInvariant()
    Set-Content -LiteralPath $traversalChecksum -Value "$hash  rusty_v8_../x86_64-pc-windows-msvc.lib.gz"
    Assert-Throws { & $restoreScript -ArtifactDirectory $traversalArtifact -CargoHome (Join-Path $tempRoot "traversal-cargo") } "path traversal"

    Write-Output "V8 artifact restore tests passed: URL cache name, content, Linux rejection, checksum rejection."
} finally {
    if (Test-Path -LiteralPath $tempRoot) {
        Remove-Item -LiteralPath $tempRoot -Recurse -Force
    }
}
