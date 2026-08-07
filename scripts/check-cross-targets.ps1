<#
.SYNOPSIS
Type-check the non-Windows code paths, which nothing else compiles.

.DESCRIPTION
The workspace carries `cfg(unix)` / `cfg(not(windows))` branches across pty,
atomic_file, environment, external, review, the daemon and the probe. On a
Windows-only machine those branches are not merely untested — they are never
type-checked, so a signature error in them survives a full green suite. The first
run of this script found exactly that: the daemon's peer-PID check compared
`interprocess`'s Unix `pid_t` (`i32`) against the client-declared `u32`.

Running those branches is a separate job (WSL2). Compiling them is the cheap half
and closes the larger hole.

aarch64-pc-windows-msvc is checked for a different reason: this project's whole
differentiator is Windows, and it ships x86_64 only, so every Snapdragon-class
Windows machine runs the entire supervised fleet -- daemon, probe and both
agents -- under emulation. The Windows-specific code here is `windows-sys` calls
(job objects, EcoQoS, ConPTY, named pipes, taskbar) rather than intrinsics, so
the port should be a target and bundle question rather than a code one. Type-
checking it is what turns that "should be" into something the suite knows.

Type-checking is NOT support. An ARM64 bundle would have to go through
scripts/verify-installer.ps1 on real hardware before a release claimed it, and
an untested second architecture is worse than an honest single one.

terminalai-app is deliberately excluded: on Linux its Tauri dependency tree pulls
`libdbus-sys`, whose build script needs a Linux pkg-config and dbus headers that
cross-checking from Windows cannot supply. Its own `cfg(unix)` code is small and
is listed in the failure message rather than silently counted as covered.

Two pins are load-bearing. The toolchain must be the managed one — the linked
`terminalai` toolchain is a standalone MSI install and rustup refuses to add
targets or components to it. And RUSTC must be set explicitly: the standalone
install sits earlier on PATH, so cargo would otherwise drive a rustc whose
sysroot has no Linux std and report the target as missing when it is present.

.EXAMPLE
pwsh -NoProfile -File scripts/check-cross-targets.ps1
#>
[CmdletBinding()]
param(
    [string]$Toolchain = 'stable-x86_64-pc-windows-msvc',
    [string[]]$Targets = @('x86_64-unknown-linux-gnu', 'aarch64-pc-windows-msvc')
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repoRoot = Split-Path -Parent $PSScriptRoot
$packages = @('terminalai-core', 'terminalai-daemon', 'terminalai-probe')
$failures = [System.Collections.Generic.List[string]]::new()

$toolchainRoot = Join-Path $HOME ".rustup/toolchains/$Toolchain/bin"
$cargo = Join-Path $toolchainRoot 'cargo.exe'
$rustc = Join-Path $toolchainRoot 'rustc.exe'

if (-not (Test-Path $cargo) -or -not (Test-Path $rustc)) {
    Write-Host "FAIL  the managed toolchain '$Toolchain' is not installed." -ForegroundColor Red
    Write-Host "      rustup toolchain install $Toolchain --profile minimal"
    Write-Host "      Do NOT make it the default: the standalone install must stay the default"
    Write-Host "      toolchain or every rustup-shim-resolved tool switches with it."
    exit 1
}

Push-Location $repoRoot
try {
    foreach ($target in $Targets) {
        $installed = & $env:USERPROFILE\.cargo\bin\rustup.exe target list --toolchain $Toolchain --installed
        if ($installed -notcontains $target) {
            $failures.Add("$target is not installed: rustup target add --toolchain $Toolchain $target")
            continue
        }

        Write-Host "checking $target ..." -ForegroundColor Cyan
        $arguments = @('check', '--target', $target, '--all-targets')
        foreach ($package in $packages) { $arguments += @('-p', $package) }

        $previousRustc = $env:RUSTC
        $env:RUSTC = $rustc
        try {
            & $cargo @arguments
            $code = $LASTEXITCODE
        } finally {
            if ($null -eq $previousRustc) { Remove-Item Env:\RUSTC -ErrorAction SilentlyContinue }
            else { $env:RUSTC = $previousRustc }
        }

        if ($code -ne 0) { $failures.Add("cargo check failed for $target (exit $code)") }
        else { Write-Host "PASS  $target compiles" -ForegroundColor Green }
    }
} finally {
    Pop-Location
}

if ($failures.Count -gt 0) {
    Write-Host ''
    foreach ($failure in $failures) { Write-Host "FAIL  $failure" -ForegroundColor Red }
    exit 1
}

Write-Host ''
Write-Host "PASS  every cross target compiles ($($packages -join ', '))." -ForegroundColor Green
Write-Host "NOTE  terminalai-app is not cross-checked: libdbus-sys needs a Linux pkg-config." -ForegroundColor Yellow
exit 0
