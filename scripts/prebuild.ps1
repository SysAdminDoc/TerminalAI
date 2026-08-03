<#
.SYNOPSIS
Everything cargo tauri build needs before it compiles the app crate.

.DESCRIPTION
Stages the daemon and probe sidecars, then builds the Vite frontend. Wired to
tauri.conf.json's beforeBuildCommand so a bundle can never be produced without
the sibling executables the app spawns.
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repoRoot = Split-Path -Parent $PSScriptRoot

& (Join-Path $PSScriptRoot 'stage-sidecars.ps1')

Push-Location (Join-Path $repoRoot 'web')
try {
    & npm run build
    if ($LASTEXITCODE -ne 0) { throw "npm run build failed with exit code $LASTEXITCODE" }
} finally {
    Pop-Location
}
