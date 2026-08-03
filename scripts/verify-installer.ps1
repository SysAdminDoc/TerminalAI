<#
.SYNOPSIS
Release gate: prove the built installer produces an application that starts.

.DESCRIPTION
A bundle that omits terminalai-daemon.exe still installs cleanly and still runs
from the build tree, so nothing short of installing it and launching the installed
binary catches the failure. This gate:

  1. asserts every sidecar declared in tauri.conf.json is present in the bundle,
  2. silently installs the NSIS package into a scratch prefix,
  3. asserts the sidecars landed next to the installed terminalai.exe,
  4. launches the installed binary on the isolated virtual display and asserts a
     visible fleet window plus a listening daemon pipe,
  5. uninstalls and removes the scratch prefix.

Every failure is loud and named. Exit code is non-zero on any failure.

.EXAMPLE
pwsh -NoProfile -File scripts/verify-installer.ps1
#>
[CmdletBinding()]
param(
    [string]$Installer = $null,
    [int]$LaunchTimeoutSeconds = 60,
    [switch]$KeepPrefix
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repoRoot = Split-Path -Parent $PSScriptRoot
$configPath = Join-Path $repoRoot 'crates/terminalai-app/tauri.conf.json'
$isolation = Join-Path $HOME '.claude/scripts/visual-isolation.ps1'
$failures = [System.Collections.Generic.List[string]]::new()

function Fail([string]$message) {
    $failures.Add($message)
    Write-Host "FAIL  $message" -ForegroundColor Red
}

function Pass([string]$message) {
    Write-Host "ok    $message" -ForegroundColor Green
}

<#
Run the visual-isolation helper without ever sharing a pipe with it.

`& pwsh ... | Out-Null` deadlocks here: the launched application inherits the
child's stdout handle, so the pipe stays open for the life of the app and the
pipeline never drains. Redirecting to files gives the same output with no shared
handle to wait on.
#>
function Invoke-Isolation([string[]]$IsolationArgs) {
    $stdout = [System.IO.Path]::GetTempFileName()
    $stderr = [System.IO.Path]::GetTempFileName()
    try {
        $arguments = @('-NoProfile', '-File', $isolation) + $IsolationArgs
        $helper = Start-Process -FilePath 'pwsh' -ArgumentList $arguments -PassThru `
            -RedirectStandardOutput $stdout -RedirectStandardError $stderr -WindowStyle Hidden
        $null = $helper.Handle
        $helper.WaitForExit()
        $output = @()
        foreach ($file in @($stdout, $stderr)) {
            if (Test-Path -LiteralPath $file) {
                $output += @(Get-Content -LiteralPath $file -ErrorAction SilentlyContinue)
            }
        }
        [pscustomobject]@{ ExitCode = $helper.ExitCode; Output = $output }
    } finally {
        Remove-Item -LiteralPath $stdout, $stderr -Force -ErrorAction SilentlyContinue
    }
}

$config = Get-Content -LiteralPath $configPath -Raw | ConvertFrom-Json
$productVersion = $config.version
$declaredSidecars = @()
if ($config.bundle.PSObject.Properties.Name -contains 'externalBin') {
    $declaredSidecars = @($config.bundle.externalBin | ForEach-Object { Split-Path -Leaf $_ })
}
if ($declaredSidecars.Count -eq 0) {
    Fail 'tauri.conf.json declares no externalBin sidecars; the installed app cannot start its daemon'
}

if (-not $Installer) {
    $nsisDir = Join-Path $repoRoot 'target/release/bundle/nsis'
    if (-not (Test-Path -LiteralPath $nsisDir)) {
        throw "no NSIS bundle directory at $nsisDir — run cargo tauri build first"
    }
    $candidate = Get-ChildItem -LiteralPath $nsisDir -Filter '*-setup.exe' |
        Sort-Object LastWriteTime -Descending |
        Select-Object -First 1
    if (-not $candidate) { throw "no *-setup.exe under $nsisDir — run cargo tauri build first" }
    $Installer = $candidate.FullName
}
if (-not (Test-Path -LiteralPath $Installer)) { throw "installer not found: $Installer" }
Write-Host "Installer: $Installer"
Write-Host "Declared sidecars: $($declaredSidecars -join ', ')"

if ($Installer -notlike "*$productVersion*") {
    Fail "installer file name does not carry the configured version $productVersion — stale artifact?"
} else {
    Pass "installer matches configured version $productVersion"
}

$prefix = Join-Path ([System.IO.Path]::GetTempPath()) ("terminalai-installcheck-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
$installed = $false
$process = $null

try {
    Write-Host "Installing into $prefix"
    # NSIS: /S is silent, /D must be last and unquoted.
    $arguments = "/S /D=$prefix"
    $installerProcess = Start-Process -FilePath $Installer -ArgumentList $arguments -PassThru
    # Touching Handle caches it, so ExitCode is still readable after the process
    # exits. Without this, a fast installer leaves an object whose ExitCode throws.
    $null = $installerProcess.Handle
    $installerProcess.WaitForExit()
    $installerExit = $installerProcess.ExitCode
    if ($installerExit -ne 0) {
        Fail "installer exited with code $installerExit"
    } else {
        $installed = $true
        Pass 'installer completed'
    }

    $appExe = Join-Path $prefix 'terminalai.exe'
    if (-not (Test-Path -LiteralPath $appExe)) {
        Fail "installed application binary is missing: $appExe"
    } else {
        Pass 'terminalai.exe installed'
    }

    foreach ($sidecar in $declaredSidecars) {
        $sidecarExe = Join-Path $prefix "$sidecar.exe"
        if (-not (Test-Path -LiteralPath $sidecarExe)) {
            Fail "declared sidecar missing from the installed prefix: $sidecarExe"
        } else {
            Pass "sidecar installed: $sidecar.exe"
        }
    }

    if ($failures.Count -eq 0) {
        # The application must never open on a physical monitor.
        $ensure = Invoke-Isolation @('ensure')
        if ($ensure.ExitCode -ne 0) {
            throw "visual-isolation ensure failed; refusing to launch on a physical display: $($ensure.Output)"
        }

        Write-Host 'Launching the installed binary on the isolated display'
        # launch is the window assertion: it waits for a visible window on the
        # private desktop, proves its placement, and exits non-zero having killed
        # the process if none appeared. A window on a private desktop is not
        # enumerable from this one, so MainWindowHandle would never see it.
        $launch = Invoke-Isolation @('launch', '-FilePath', $appExe)
        if ($launch.ExitCode -ne 0) {
            Fail "the installed application drew no verified window on the isolated display: $($launch.Output)"
        } else {
            Pass 'fleet window appeared and its placement was proven'
        }

        $launchInfo = $launch.Output |
            Where-Object { $_.TrimStart().StartsWith('{') } |
            Select-Object -Last 1
        if ($launchInfo) {
            $appPid = ($launchInfo | ConvertFrom-Json).processId
            $process = Get-Process -Id $appPid -ErrorAction SilentlyContinue
        }

        # The daemon's control pipe is the proof it started, not merely that a
        # process exists: a daemon that cannot bind never serves.
        $deadline = (Get-Date).AddSeconds($LaunchTimeoutSeconds)
        $daemonSeen = $false
        while ((Get-Date) -lt $deadline -and -not $daemonSeen) {
            $daemonSeen = [bool](
                [System.IO.Directory]::GetFiles('\\.\pipe\') |
                    Where-Object { $_ -like '*terminalai*' }
            )
            if (-not $daemonSeen) { Start-Sleep -Milliseconds 500 }
        }
        if (-not $daemonSeen) {
            Fail "no TerminalAI daemon pipe appeared within ${LaunchTimeoutSeconds}s"
        } else {
            Pass 'daemon control pipe is listening'
        }

        if ($process -and -not $process.HasExited) {
            Pass 'the application is still running after the daemon handshake'
        } else {
            Fail 'the installed application exited instead of staying up'
        }
    } else {
        Write-Host 'Skipping launch: the installed prefix is already incomplete.' -ForegroundColor Yellow
    }
} finally {
    if ($process -and -not $process.HasExited) {
        Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
    }
    # Close everything this gate started from the scratch prefix, or the silent
    # uninstall blocks on files still in use and leaves the prefix behind.
    Get-Process -ErrorAction SilentlyContinue |
        Where-Object {
            $_.ProcessName -like 'terminalai*' -and
            $(try { $_.Path } catch { $null }) -like "$prefix*"
        } |
        Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep -Seconds 1
    if ($installed -and -not $KeepPrefix) {
        $uninstaller = Join-Path $prefix 'uninstall.exe'
        if (Test-Path -LiteralPath $uninstaller) {
            Start-Process -FilePath $uninstaller -ArgumentList '/S' -Wait -ErrorAction SilentlyContinue
            Start-Sleep -Seconds 2
        }
        if (Test-Path -LiteralPath $prefix) {
            Remove-Item -LiteralPath $prefix -Recurse -Force -ErrorAction SilentlyContinue
        }
        if (Test-Path -LiteralPath $prefix) {
            Write-Host "note: scratch prefix still present at $prefix" -ForegroundColor Yellow
        } else {
            Pass 'uninstalled and scratch prefix removed'
        }
    }
}

if ($failures.Count -gt 0) {
    Write-Host ''
    Write-Host "Installer gate FAILED with $($failures.Count) problem(s):" -ForegroundColor Red
    $failures | ForEach-Object { Write-Host "  - $_" -ForegroundColor Red }
    exit 1
}

Write-Host ''
Write-Host 'Installer gate passed.' -ForegroundColor Green
