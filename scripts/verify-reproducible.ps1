<#
.SYNOPSIS
Build the sidecars twice from the same commit and prove the bytes match.

.DESCRIPTION
Reproducibility is the only claim that lets someone else confirm a released
UNSIGNED binary came from this source. There is no signature to check, so the
alternative is that they build it themselves and get the same file.

Two ways this check lies if written naively, both of them learned the hard way
on another project and both live here too:

  1. **A rebuild that is not a rebuild.** If the second build reuses the first
     one's artifacts, the comparison confirms the cache is consistent, not that
     the build is reproducible -- it would pass on a build that is wildly
     non-deterministic. So the target directory is cleaned between builds, and
     the executables are removed explicitly afterwards: `cargo clean -p` leaves
     the uplifted exe in `target/release` even after removing everything behind
     it, which is exactly a stale artifact surviving into the second build.

  2. **A clean that eats the evidence.** The comparison copies must never live
     under `target/`, because the clean between builds deletes them -- and then
     a "not reproducible" result has to be diagnosed against artifacts that no
     longer exist. They are written outside it, and the first build's copy is
     kept on failure.

`--locked` on both builds, so the tree being compared is the tree that was
pinned rather than whatever resolved today.

.PARAMETER WorkDirectory
Where to keep the two builds' copies. Defaults to `dist/reproducible`, which is
outside `target/` on purpose.
#>
[CmdletBinding()]
param(
    [string]$WorkDirectory = $null
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repoRoot = Split-Path -Parent $PSScriptRoot
if (-not $WorkDirectory) { $WorkDirectory = Join-Path $repoRoot 'dist/reproducible' }

$artifacts = @('terminalai-daemon.exe', 'terminalai-probe.exe')

function Invoke-Build {
    param([string]$Label)

    Write-Host "Build $Label"
    & cargo build --release --locked -p terminalai-daemon -p terminalai-probe
    if ($LASTEXITCODE -ne 0) { throw "build $Label failed with exit code $LASTEXITCODE" }

    $destination = Join-Path $WorkDirectory $Label
    New-Item -ItemType Directory -Force -Path $destination | Out-Null
    $hashes = [ordered]@{}
    foreach ($name in $artifacts) {
        $source = Join-Path $repoRoot "target/release/$name"
        if (-not (Test-Path -LiteralPath $source)) { throw "build $Label produced no $name" }
        # Copied out of target/ before anything cleans it. This is the whole
        # point of the work directory living elsewhere.
        Copy-Item -LiteralPath $source -Destination (Join-Path $destination $name) -Force
        $hashes[$name] = (Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash
        Write-Host "  $name  $($hashes[$name])"
    }
    return $hashes
}

function Clear-Build {
    # Always a full clean. `cargo clean -p terminalai-core -p terminalai-daemon
    # -p terminalai-probe` was measured on 2026-08-07 removing 57,865 files and
    # 38.1 GiB from this workspace -- effectively the whole target directory --
    # so a "just the workspace crates" mode would have been a knob that did not
    # do what its name said.
    Write-Host 'Cleaning the target directory'
    & cargo clean
    if ($LASTEXITCODE -ne 0) { throw "cargo clean failed with exit code $LASTEXITCODE" }

    # `cargo clean -p` leaves the uplifted executable in target/release even
    # after removing the fingerprints behind it -- observed here, and it is why
    # this is checked rather than assumed. A stale exe surviving into the second
    # build is failure mode 1, and it reports as a pass.
    foreach ($name in $artifacts) {
        $path = Join-Path $repoRoot "target/release/$name"
        if (Test-Path -LiteralPath $path) { Remove-Item -LiteralPath $path -Force }
        if (Test-Path -LiteralPath $path) {
            throw "could not remove $path, so the second build would not be independent"
        }
    }
}

Push-Location $repoRoot
try {
    if (Test-Path -LiteralPath $WorkDirectory) { Remove-Item -LiteralPath $WorkDirectory -Recurse -Force }
    New-Item -ItemType Directory -Force -Path $WorkDirectory | Out-Null

    Clear-Build
    $first = Invoke-Build -Label 'first'
    Clear-Build
    $second = Invoke-Build -Label 'second'

    $differences = @()
    foreach ($name in $artifacts) {
        if ($first[$name] -ne $second[$name]) {
            $differences += "$name differs: $($first[$name]) then $($second[$name])"
        }
    }

    if ($differences) {
        Write-Host ''
        Write-Warning "Both builds are kept under $WorkDirectory for diagnosis."
        throw "the build is not reproducible:`n  " + ($differences -join "`n  ")
    }

    Write-Host ''
    Write-Host "Reproducible: $($artifacts -join ', ') are byte-identical across two clean builds."
    Write-Host 'Scope: the whole dependency tree was rebuilt from scratch for each build.'
    Remove-Item -LiteralPath $WorkDirectory -Recurse -Force
} finally {
    Pop-Location
}
