<#
.SYNOPSIS
Refuse a release whose own metadata disagrees with itself.

.DESCRIPTION
v0.9.0 shipped with no changelog entry. The release commit renamed
`## [Unreleased]` to `## [0.9.0]`, and the next commit renamed that same heading
straight back rather than inserting a fresh one above it, so the string "0.9.0"
appeared nowhere in CHANGELOG.md while four files and the README badge all
declared it. Nothing noticed, because nothing was looking.

This checks the four claims a release makes about itself:

1. Every declared version string agrees — the workspace manifest, the Tauri
   config, the frontend package and the README badge.
2. CHANGELOG.md carries a section for that version, and it is not the
   `[Unreleased]` heading wearing a version number.
3. No version section repeats a `###` subsection, which is what happens when a
   later commit appends to a released section instead of opening a new one.
4. The test counts stated in README.md match what the suites actually report.

Claim 4 runs both suites, so it is the slow half. `-SkipTests` omits it for a
quick metadata check; the release gate must not.

Non-zero exit means do not publish.

.EXAMPLE
pwsh -NoProfile -File scripts/verify-release-metadata.ps1

.EXAMPLE
pwsh -NoProfile -File scripts/verify-release-metadata.ps1 -SkipTests
#>
[CmdletBinding()]
param(
    [switch]$SkipTests
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repoRoot = Split-Path -Parent $PSScriptRoot
$problems = [System.Collections.Generic.List[string]]::new()

function Add-Problem([string]$message) {
    $script:problems.Add($message)
    Write-Host "  FAIL  $message"
}

function Add-Pass([string]$message) {
    Write-Host "  ok    $message"
}

# --- 1. every declared version string agrees ------------------------------

Write-Host 'Version strings'

$cargoToml = Join-Path $repoRoot 'Cargo.toml'
$workspaceVersion = (Select-String -Path $cargoToml -Pattern '^version\s*=\s*"([^"]+)"' |
    Select-Object -First 1).Matches[0].Groups[1].Value
if (-not $workspaceVersion) {
    throw "could not read the workspace version from $cargoToml"
}
Add-Pass "Cargo.toml declares $workspaceVersion"

$declared = @{
    'crates/terminalai-app/tauri.conf.json' = '"version"\s*:\s*"([^"]+)"'
    'web/package.json'                      = '"version"\s*:\s*"([^"]+)"'
    'README.md'                             = 'img\.shields\.io/badge/version-([0-9][^-]*)-'
}

foreach ($relative in $declared.Keys | Sort-Object) {
    $path = Join-Path $repoRoot $relative
    $match = Select-String -Path $path -Pattern $declared[$relative] | Select-Object -First 1
    if (-not $match) {
        Add-Problem "$relative declares no version this script can find"
        continue
    }
    $found = $match.Matches[0].Groups[1].Value
    if ($found -ne $workspaceVersion) {
        Add-Problem "$relative says $found, Cargo.toml says $workspaceVersion"
    }
    else {
        Add-Pass "$relative agrees"
    }
}

# Workspace members pin each other by version as well as by path, and cargo
# reports that mismatch as an unresolvable dependency rather than as a stale
# version string. Caught exactly that on the 0.9.0 -> 0.10.0 bump.
foreach ($manifest in Get-ChildItem -Path (Join-Path $repoRoot 'crates') -Filter 'Cargo.toml' -Recurse) {
    $pins = Select-String -Path $manifest.FullName -Pattern 'path\s*=\s*"[^"]*"\s*,\s*version\s*=\s*"([^"]+)"'
    foreach ($pin in $pins) {
        $found = $pin.Matches[0].Groups[1].Value
        $relative = $manifest.FullName.Substring($repoRoot.Length + 1)
        if ($found -ne $workspaceVersion) {
            Add-Problem "$relative pins a workspace crate at $found, not $workspaceVersion"
        }
        else {
            Add-Pass "$relative pins workspace crates at $workspaceVersion"
        }
    }
}

# --- 2 and 3. the changelog actually records this version -----------------

Write-Host 'Changelog'

$changelogPath = Join-Path $repoRoot 'CHANGELOG.md'
$changelog = Get-Content -Path $changelogPath

$versionHeading = $changelog | Where-Object { $_ -match "^##\s+\[$([regex]::Escape($workspaceVersion))\]" }
if (-not $versionHeading) {
    Add-Problem "CHANGELOG.md has no '## [$workspaceVersion]' section, but every manifest declares that version"
}
else {
    Add-Pass "CHANGELOG.md records $workspaceVersion"
}

# A released section that still carries post-release edits is the failure this
# script exists for, and it shows up as a repeated subsection.
$section = '(none)'
$seen = @{}
foreach ($line in $changelog) {
    if ($line -match '^##\s+\[') {
        $section = $line.Trim()
        $seen = @{}
        continue
    }
    if ($line -match '^###\s+') {
        $subsection = $line.Trim()
        if ($seen.ContainsKey($subsection)) {
            Add-Problem "$section repeats '$subsection' — a later commit appended to it instead of opening a new section"
        }
        $seen[$subsection] = $true
    }
}
if ($problems.Count -eq 0 -or -not ($problems -match 'repeats')) {
    Add-Pass 'no version section repeats a subsection'
}

# --- 4. the README's test counts are the counts the suites report ----------

if ($SkipTests) {
    Write-Host 'Test counts  (skipped)'
}
else {
    Write-Host 'Test counts'

    function Measure-CargoTests([string[]]$cargoArguments) {
        $output = & cargo @cargoArguments 2>&1
        if ($LASTEXITCODE -ne 0) {
            # Say WHICH test failed. Throwing the output away leaves the operator
            # with "the suite failed" and a re-run that usually passes, which is
            # the least actionable thing this gate can report -- the failures
            # seen here have been load-sensitive ones that only appear while the
            # workspace is also rebuilding.
            $failed = $output |
                Select-String -Pattern '^(test .* \.\.\. FAILED|\s+[a-z0-9_]+::[a-z0-9_:]+$|thread .* panicked at .*)$' |
                Select-Object -First 20
            $detail = if ($failed) { "`n" + ($failed -join "`n") } else { "`n(cargo produced no recognisable failure lines; re-run it directly)" }
            throw "cargo $($cargoArguments -join ' ') failed; fix the suite before checking its count.$detail"
        }
        $total = 0
        foreach ($line in $output) {
            if ("$line" -match '^test result: ok\. (\d+) passed') {
                $total += [int]$Matches[1]
            }
        }
        return $total
    }

    $readme = Get-Content -Path (Join-Path $repoRoot 'README.md') -Raw

    $defaultTests = Measure-CargoTests @('test', '--workspace')
    $allFeatureTests = Measure-CargoTests @('test', '--workspace', '--all-features')

    $frontendOutput = & npm --prefix (Join-Path $repoRoot 'web') test 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw 'npm --prefix web test failed; fix the suite before checking its count'
    }
    $frontendTests = 0
    foreach ($line in $frontendOutput) {
        # node --test's summary line is decorated with a leading glyph whose
        # encoding does not survive every console, so match the tail only.
        if ("$line" -match 'pass\s+(\d+)\s*$') {
            $frontendTests += [int]$Matches[1]
        }
    }

    $claims = @(
        @{ Label = 'default Rust tests'; Actual = $defaultTests; Pattern = '(\d+) default Rust tests' }
        @{ Label = 'all-features Rust tests'; Actual = $allFeatureTests; Pattern = '(\d+) with the opt-in' }
        @{ Label = 'frontend tests'; Actual = $frontendTests; Pattern = '(\d+) frontend tests' }
    )

    foreach ($claim in $claims) {
        if ($readme -match $claim.Pattern) {
            $stated = [int]$Matches[1]
            if ($stated -ne $claim.Actual) {
                Add-Problem "README claims $stated $($claim.Label); the suite reports $($claim.Actual)"
            }
            else {
                Add-Pass "$($claim.Label): $stated"
            }
        }
        else {
            Add-Problem "README states no count for $($claim.Label)"
        }
    }
}

# --- verdict ---------------------------------------------------------------

Write-Host ''
if ($problems.Count -gt 0) {
    Write-Host "Release metadata is inconsistent ($($problems.Count) problem(s)). Do not publish."
    exit 1
}

Write-Host "Release metadata for $workspaceVersion is consistent."
exit 0
