<#
.SYNOPSIS
Collect the verified Windows installers and generate release provenance.

.DESCRIPTION
This is the final, artifact-facing step of a tagged Windows release. It does not
build or sign anything. It refuses to guess which installer belongs to the
release: the exact versioned NSIS and MSI files must already exist in the Tauri
bundle directories.

The output contains:

  * both unsigned installers,
  * SHA256SUMS for those installers,
  * release-manifest.json with the commit, hashes, sizes, and MSI identity,
  * a machine-readable unsigned-policy note, and
  * a three-file Winget manifest ready for submission after the release exists.

The MSI product and upgrade codes are read from the MSI database itself. The
upgrade code is not copied from a source template, because a manifest that names
the wrong installed identity is worse than one that names none.

.PARAMETER OutputDirectory
Where the release assets are written. Defaults to dist/release.

.PARAMETER Repository
GitHub owner/repository used to construct the installer URLs.

.PARAMETER Tag
Release tag used in installer URLs. Defaults to v<version> and must match the
version declared by the workspace.

.PARAMETER SkipWingetValidation
Do not invoke the local Winget manifest validator. The generated files are still
written. This is intended only for non-Windows or minimal build images; the
release workflow leaves validation enabled.
#>
[CmdletBinding()]
param(
    [string]$OutputDirectory = $null,
    [string]$Repository = 'SysAdminDoc/TerminalAI',
    [string]$Tag = $null,
    [switch]$SkipWingetValidation
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repoRoot = [System.IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
if (-not $OutputDirectory) { $OutputDirectory = Join-Path $repoRoot 'dist/release' }
$outputPath = [System.IO.Path]::GetFullPath($OutputDirectory)
$rootPrefix = $repoRoot.TrimEnd('\') + '\'
if ($outputPath -eq $repoRoot -or -not $outputPath.StartsWith($rootPrefix, [StringComparison]::OrdinalIgnoreCase)) {
    throw "OutputDirectory must be inside the repository and must not be the repository root: $outputPath"
}

$cargoPath = Join-Path $repoRoot 'Cargo.toml'
$configPath = Join-Path $repoRoot 'crates/terminalai-app/tauri.conf.json'
$cargoVersionMatch = Select-String -Path $cargoPath -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1
if (-not $cargoVersionMatch) { throw "could not read the workspace version from $cargoPath" }
$version = $cargoVersionMatch.Matches[0].Groups[1].Value
$config = Get-Content -LiteralPath $configPath -Raw | ConvertFrom-Json
if ($config.version -ne $version) {
    throw "tauri.conf.json declares $($config.version), while Cargo.toml declares $version"
}
if (-not $Tag) { $Tag = "v$version" }
if ($Tag -ne "v$version") { throw "release tag $Tag does not match workspace version $version" }
if ($Repository -notmatch '^[^/]+/[^/]+$') { throw "Repository must be owner/name: $Repository" }

$nsisSource = Join-Path $repoRoot "target/release/bundle/nsis/TerminalAI_${version}_x64-setup.exe"
$msiSource = Join-Path $repoRoot "target/release/bundle/msi/TerminalAI_${version}_x64_en-US.msi"
foreach ($source in @($nsisSource, $msiSource)) {
    if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
        throw "expected current-version installer is missing: $source"
    }
}

function Get-MsiProperty([object]$database, [string]$property) {
    $query = "SELECT ``Value`` FROM Property WHERE Property='$property'"
    $view = $database.OpenView($query)
    try {
        $view.Execute() | Out-Null
        $row = $view.Fetch()
        if (-not $row) { throw "MSI has no Property table value for $property" }
        return $row.StringData(1).Trim()
    } finally {
        if ($view) { $view.Close() | Out-Null }
    }
}

function Get-MsiIdentity([string]$path) {
    $windowsInstaller = New-Object -ComObject WindowsInstaller.Installer
    $database = $windowsInstaller.OpenDatabase($path, 0)
    try {
        [ordered]@{
            productCode = (Get-MsiProperty $database 'ProductCode')
            upgradeCode = (Get-MsiProperty $database 'UpgradeCode')
            productName = (Get-MsiProperty $database 'ProductName')
            manufacturer = (Get-MsiProperty $database 'Manufacturer')
            productVersion = (Get-MsiProperty $database 'ProductVersion')
        }
    } finally {
        if ($database) { $database.Commit() | Out-Null }
    }
}

if (Test-Path -LiteralPath $outputPath) {
    Remove-Item -LiteralPath $outputPath -Recurse -Force
}
New-Item -ItemType Directory -Force -Path $outputPath | Out-Null
$wingetPath = Join-Path $outputPath 'winget'
New-Item -ItemType Directory -Force -Path $wingetPath | Out-Null

$nsisName = Split-Path -Leaf $nsisSource
$msiName = Split-Path -Leaf $msiSource
Copy-Item -LiteralPath $nsisSource -Destination (Join-Path $outputPath $nsisName)
Copy-Item -LiteralPath $msiSource -Destination (Join-Path $outputPath $msiName)

$msiIdentity = Get-MsiIdentity (Join-Path $outputPath $msiName)
if ($msiIdentity.productName -ne 'TerminalAI') { throw "MSI ProductName is $($msiIdentity.productName), not TerminalAI" }
if ($msiIdentity.productVersion -ne $version) {
    throw "MSI ProductVersion is $($msiIdentity.productVersion), not $version"
}

$releaseUrl = "https://github.com/$Repository/releases/download/$Tag"
$artifactRows = @(
    [ordered]@{
        name = $nsisName
        kind = 'installer'
        installerType = 'nsis'
        architecture = 'x64'
        sha256 = (Get-FileHash -LiteralPath (Join-Path $outputPath $nsisName) -Algorithm SHA256).Hash
        bytes = (Get-Item -LiteralPath (Join-Path $outputPath $nsisName)).Length
        url = "$releaseUrl/$nsisName"
    }
    [ordered]@{
        name = $msiName
        kind = 'installer'
        installerType = 'msi'
        architecture = 'x64'
        sha256 = (Get-FileHash -LiteralPath (Join-Path $outputPath $msiName) -Algorithm SHA256).Hash
        bytes = (Get-Item -LiteralPath (Join-Path $outputPath $msiName)).Length
        url = "$releaseUrl/$msiName"
    }
)

$hashLines = foreach ($artifact in $artifactRows | Sort-Object name) {
    "{0} *{1}" -f $artifact.sha256, $artifact.name
}
Set-Content -LiteralPath (Join-Path $outputPath 'SHA256SUMS') -Value $hashLines -Encoding utf8NoBOM

$commit = (& git -C $repoRoot rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0 -or $commit -notmatch '^[0-9a-f]{40}$') { throw 'could not read the release commit from git' }

$manifest = [ordered]@{
    schemaVersion = 1
    product = 'TerminalAI'
    version = $version
    tag = $Tag
    repository = $Repository
    commit = $commit
    unsigned = $true
    provenance = 'SLSA Build L1: hosted build from this tagged commit; no signing is performed.'
    installers = @($artifactRows)
    msi = [ordered]@{
        productCode = $msiIdentity.productCode
        upgradeCode = $msiIdentity.upgradeCode
        productName = $msiIdentity.productName
        manufacturer = $msiIdentity.manufacturer
        productVersion = $msiIdentity.productVersion
    }
    verification = [ordered]@{
        hashFile = 'SHA256SUMS'
        installerGate = 'scripts/verify-installer.ps1'
        executableReproducibilityGate = 'scripts/verify-reproducible.ps1'
        crossTargetGate = 'scripts/check-cross-targets.ps1'
    }
}
$manifest | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $outputPath 'release-manifest.json') -Encoding utf8NoBOM

@"
TerminalAI $version is intentionally unsigned.

The release workflow uses Tauri's --no-sign flag. Verify downloaded installers
against SHA256SUMS; no code-signing certificate or detached signature is implied.
The installer gate ran against the exact artifact before publication.
"@ | Set-Content -LiteralPath (Join-Path $outputPath 'UNSIGNED.txt') -Encoding utf8NoBOM

$nsisHash = ($artifactRows | Where-Object installerType -eq 'nsis').sha256
$msiHash = ($artifactRows | Where-Object installerType -eq 'msi').sha256
$productCode = $msiIdentity.productCode
$upgradeCode = $msiIdentity.upgradeCode

@"
# yaml-language-server: `$schema=https://aka.ms/winget-manifest.defaultLocale.1.12.0.schema.json
PackageIdentifier: SysAdminDoc.TerminalAI
PackageVersion: $version
PackageLocale: en-US
Publisher: SysAdminDoc
PublisherUrl: https://github.com/$Repository
PublisherSupportUrl: https://github.com/$Repository/issues
Author: SysAdminDoc
PackageName: TerminalAI
PackageUrl: https://github.com/$Repository
License: MIT
LicenseUrl: https://github.com/$Repository/blob/main/LICENSE
ShortDescription: A local fleet dashboard for supervised Claude Code and Codex sessions.
Description: TerminalAI supervises local agent sessions, presents their live state, and keeps review and launch controls in one Windows-native workspace.
Tags:
  - agents
  - claude
  - codex
  - terminal
ManifestType: defaultLocale
ManifestVersion: 1.12.0
"@ | Set-Content -LiteralPath (Join-Path $wingetPath 'SysAdminDoc.TerminalAI.locale.en-US.yaml') -Encoding utf8NoBOM

@"
# yaml-language-server: `$schema=https://aka.ms/winget-manifest.installer.1.12.0.schema.json
PackageIdentifier: SysAdminDoc.TerminalAI
PackageVersion: $version
Installers:
  - Architecture: x64
    InstallerType: nullsoft
    InstallerUrl: $releaseUrl/$nsisName
    InstallerSha256: $nsisHash
    InstallerSwitches:
      Silent: /S
    UpgradeBehavior: install
  - Architecture: x64
    InstallerType: msi
    InstallerUrl: $releaseUrl/$msiName
    InstallerSha256: $msiHash
    InstallerSwitches:
      Silent: /quiet
      SilentWithProgress: /passive
    ProductCode: '$productCode'
    UpgradeBehavior: install
    AppsAndFeaturesEntries:
      - DisplayName: TerminalAI
        Publisher: sysadmindoc
        ProductCode: '$productCode'
        UpgradeCode: '$upgradeCode'
        InstallerType: msi
ManifestType: installer
ManifestVersion: 1.12.0
"@ | Set-Content -LiteralPath (Join-Path $wingetPath 'SysAdminDoc.TerminalAI.installer.yaml') -Encoding utf8NoBOM

@"
# yaml-language-server: `$schema=https://aka.ms/winget-manifest.version.1.12.0.schema.json
PackageIdentifier: SysAdminDoc.TerminalAI
PackageVersion: $version
DefaultLocale: en-US
ManifestType: version
ManifestVersion: 1.12.0
"@ | Set-Content -LiteralPath (Join-Path $wingetPath 'SysAdminDoc.TerminalAI.yaml') -Encoding utf8NoBOM

if (-not $SkipWingetValidation) {
    $winget = Get-Command winget -ErrorAction SilentlyContinue
    if (-not $winget) { throw 'winget is not installed; pass -SkipWingetValidation only for a non-Windows/minimal build image' }
    & $winget.Source validate --manifest $wingetPath --disable-interactivity
    if ($LASTEXITCODE -ne 0) { throw "winget manifest validation failed with exit code $LASTEXITCODE" }
}

Write-Host "Prepared release assets for $Tag ($commit)"
Write-Host "  installers: $nsisName, $msiName"
Write-Host "  hashes:     $(Join-Path $outputPath 'SHA256SUMS')"
Write-Host "  manifests:  $wingetPath"
Write-Host '  policy:     unsigned; verify SHA256SUMS; no signing performed'
