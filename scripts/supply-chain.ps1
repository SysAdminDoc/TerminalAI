<#
.SYNOPSIS
Produce the release's supply-chain artifacts and prove they describe the binaries.

.DESCRIPTION
Two things a downloader of an unsigned binary has no other way to get:

  1. An embedded dependency manifest. `cargo auditable` writes a compressed list
     of crate names and versions into a `.dep-v0` linker section, so `cargo audit
     bin` can answer "which crate versions are in this exe" from the exe. A
     lockfile in a repository answers a different question — what the source tree
     said at some point — and requires trusting that the artifact was built from
     it.

  2. A CycloneDX SBOM, as a release asset, for the tooling that expects one.

The verification half is the point. An SBOM generated beside a binary describes
the *source tree*, not the binary, so publishing one proves nothing on its own:
if the exe were built from a different tree, or the auditable step were silently
skipped, the SBOM would look exactly the same. So each shipped executable is
checked for its embedded manifest, and the SBOM is only written once they all
carry one.

Provenance claim: a locally built artifact is SLSA Build L1 by definition — L2
requires a hosted build platform. Nothing here should be worded to imply more.

.PARAMETER OutputDirectory
Where to write the SBOM. Defaults to `dist/sbom` under the repository root.

.PARAMETER SkipBuild
Check and describe whatever is already in target/release rather than building.
#>
[CmdletBinding()]
param(
    [string]$OutputDirectory = $null,
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repoRoot = Split-Path -Parent $PSScriptRoot
if (-not $OutputDirectory) { $OutputDirectory = Join-Path $repoRoot 'dist/sbom' }

# The executables that actually ship. terminalai.exe is produced by
# `cargo tauri build` rather than by this script, so it is checked when present
# and reported as absent otherwise -- silently skipping it would let a release
# ship one unaudited binary out of three while this script says everything is
# fine.
$shipped = @('terminalai-daemon.exe', 'terminalai-probe.exe')
$appExe = 'terminalai.exe'

function Assert-Tool {
    param([string]$Name, [string]$Install)
    if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) {
        throw "$Name is not installed. Install it with: $Install"
    }
}

Assert-Tool -Name 'cargo-auditable' -Install 'cargo install cargo-auditable --locked'
Assert-Tool -Name 'cargo-audit' -Install 'cargo install cargo-audit --locked'
Assert-Tool -Name 'cargo-cyclonedx' -Install 'cargo install cargo-cyclonedx --locked'

Push-Location $repoRoot
try {
    if (-not $SkipBuild) {
        Write-Host 'Building the sidecars with an embedded dependency manifest'
        # --locked so the tree described is the tree that was pinned; a build
        # that silently updated a dependency would produce an SBOM that is
        # accurate and a release that is not what was reviewed.
        & cargo auditable build --release --locked -p terminalai-daemon -p terminalai-probe
        if ($LASTEXITCODE -ne 0) { throw "cargo auditable build failed with exit code $LASTEXITCODE" }
    }

    $releaseDir = Join-Path $repoRoot 'target/release'
    $checked = @()
    foreach ($name in $shipped) {
        $path = Join-Path $releaseDir $name
        if (-not (Test-Path -LiteralPath $path)) {
            throw "shipped binary is missing: $path (run without -SkipBuild)"
        }
        $output = & cargo audit bin $path 2>&1
        $text = $output -join "`n"
        if ($LASTEXITCODE -ne 0) {
            throw "cargo audit bin reported a problem for ${name}:`n$text"
        }
        # The message naming the data is the assertion. `cargo audit bin` exits
        # zero for a binary with NO embedded manifest too -- it simply has
        # nothing to report -- so trusting the exit code alone would certify
        # exactly the binaries this script exists to catch.
        if ($text -notmatch "Found 'cargo auditable' data") {
            throw "$name carries no embedded dependency manifest; it was not built with cargo auditable.`n$text"
        }
        $dependencies = if ($text -match '\((\d+) dependencies\)') { [int]$Matches[1] } else { 0 }
        if ($dependencies -le 0) {
            throw "$name reports an embedded manifest with no dependencies in it"
        }
        Write-Host "  $name — $dependencies dependencies resolved from the artifact"
        $checked += $name
    }

    $appPath = Join-Path $releaseDir $appExe
    if (Test-Path -LiteralPath $appPath) {
        $appText = (& cargo audit bin $appPath 2>&1) -join "`n"
        if ($appText -match "Found 'cargo auditable' data") {
            Write-Host "  $appExe — embedded manifest present"
            $checked += $appExe
        } else {
            # Not fatal: the Tauri CLI drives that build and does not route it
            # through cargo-auditable. Stated rather than hidden, because a
            # reader of this output would otherwise assume all three are covered.
            Write-Warning "$appExe has no embedded manifest (built by the Tauri CLI, which does not use cargo-auditable). The SBOM below still covers its dependencies."
        }
    } else {
        Write-Warning "$appExe is not in target/release; run cargo tauri build to include it in this check."
    }

    New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
    Get-ChildItem -Path $OutputDirectory -Filter '*.cdx.json' -File -ErrorAction SilentlyContinue |
        Remove-Item -Force
    Write-Host 'Generating the CycloneDX SBOM'
    # `--describe binaries` gives one SBOM per shipped executable rather than one
    # for the workspace, so an SBOM can be matched to the artifact it describes.
    # cargo-cyclonedx has no output-directory option -- it writes beside each
    # crate's Cargo.toml -- so the files are collected afterwards.
    & cargo cyclonedx --format json --all-features --describe binaries
    if ($LASTEXITCODE -ne 0) { throw "cargo cyclonedx failed with exit code $LASTEXITCODE" }

    $generated = @(Get-ChildItem -Path (Join-Path $repoRoot 'crates') -Filter '*.cdx.json' -File -Recurse)
    if (-not $generated) { throw 'cargo cyclonedx wrote no SBOM beside any crate manifest' }
    foreach ($file in $generated) {
        Move-Item -LiteralPath $file.FullName -Destination (Join-Path $OutputDirectory $file.Name) -Force
    }

    $boms = @(Get-ChildItem -Path $OutputDirectory -Filter '*.cdx.json' -File)
    if (-not $boms) { throw "cargo cyclonedx wrote no SBOM into $OutputDirectory" }
    foreach ($bom in $boms) {
        $parsed = Get-Content -LiteralPath $bom.FullName -Raw | ConvertFrom-Json
        $components = @($parsed.components).Count
        if ($components -le 0) { throw "$($bom.Name) lists no components" }
        Write-Host "  $($bom.Name) — $components components"
    }

    Write-Host ''
    Write-Host "Embedded manifests verified in: $($checked -join ', ')"
    Write-Host "SBOM written: $(($boms | ForEach-Object { $_.Name }) -join ', ')"
    Write-Host 'Provenance level: SLSA Build L1 (locally built; L2 requires a hosted build platform).'
} finally {
    Pop-Location
}
