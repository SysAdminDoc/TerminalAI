<#
.SYNOPSIS
Build the daemon and probe and stage them as Tauri sidecars.

.DESCRIPTION
The app spawns a sibling terminalai-daemon.exe from its own directory. Without a
staged sidecar the installer ships only terminalai.exe, so an installed copy exits
before drawing a window while a copy run from the build tree works — the defect is
invisible to anyone who builds rather than installs.

Tauri resolves externalBin entries by appending the target triple, and installs
them next to the main binary with the triple stripped, which is exactly the name
the app looks for.

Run automatically by tauri.conf.json's beforeBuildCommand; safe to run by hand.
#>
[CmdletBinding()]
param(
    [string]$Target = $null,
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repoRoot = Split-Path -Parent $PSScriptRoot
$sidecars = @('terminalai-daemon', 'terminalai-probe')

if (-not $Target) {
    $hostLine = (& rustc -vV) | Where-Object { $_ -like 'host:*' }
    if (-not $hostLine) { throw 'could not read the host target triple from rustc -vV' }
    $Target = ($hostLine -split ':', 2)[1].Trim()
}

$exeSuffix = if ($Target -like '*windows*') { '.exe' } else { '' }
$binaries = Join-Path $repoRoot 'crates/terminalai-app/binaries'
New-Item -ItemType Directory -Force -Path $binaries | Out-Null

if (-not $SkipBuild) {
    Write-Host "Building sidecars for $Target"
    # Only the daemon and probe: building the app crate here would overwrite a
    # cargo tauri build product with a dev-URL shell. Built for the host without
    # an explicit --target so the artifacts share cargo tauri build's target dir
    # instead of compiling every dependency a second time.
    & cargo build --release -p terminalai-daemon -p terminalai-probe
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed with exit code $LASTEXITCODE" }
}

$builtDir = Join-Path $repoRoot 'target/release'
foreach ($name in $sidecars) {
    $source = Join-Path $builtDir "$name$exeSuffix"
    if (-not (Test-Path -LiteralPath $source)) {
        throw "sidecar was not built: $source"
    }
    $destination = Join-Path $binaries "$name-$Target$exeSuffix"
    Copy-Item -LiteralPath $source -Destination $destination -Force
    Write-Host "Staged $destination"
}
