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
  5. installs again over that install with the daemon still running, which is the
     path every existing user takes and the one a clean install never exercises,
  6. uninstalls and removes the scratch prefix.

Step 5 exists because the daemon is deliberately designed to outlive its window,
so at upgrade time it is still running and still holds an open image section on
its own executable. Whether the two builds differ is irrelevant to that lock, so
the default is to install the same package over itself; pass -PreviousInstaller
to run a genuine previous-release-to-current upgrade instead.

Every failure is loud and named. Exit code is non-zero on any failure.

.EXAMPLE
pwsh -NoProfile -File scripts/verify-installer.ps1
#>
[CmdletBinding()]
param(
    [string]$Installer = $null,
    [string]$PreviousInstaller = $null,
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

function Get-PeSubsystem([string]$path) {
    $stream = [System.IO.File]::OpenRead($path)
    try {
        $reader = [System.IO.BinaryReader]::new($stream)
        $stream.Seek(0x3c, [System.IO.SeekOrigin]::Begin) | Out-Null
        $peOffset = $reader.ReadInt32()
        if ($peOffset -lt 0) { throw "invalid PE header offset in $path" }

        $stream.Seek($peOffset, [System.IO.SeekOrigin]::Begin) | Out-Null
        if ($reader.ReadUInt32() -ne 0x00004550) { throw "$path is not a PE image" }

        # Signature (4) plus IMAGE_FILE_HEADER (20) leads to the optional header.
        $optionalHeader = $peOffset + 24
        $stream.Seek($optionalHeader, [System.IO.SeekOrigin]::Begin) | Out-Null
        $magic = $reader.ReadUInt16()
        if ($magic -ne 0x010b -and $magic -ne 0x020b) {
            throw "$path has an unsupported PE optional-header magic 0x$('{0:x4}' -f $magic)"
        }

        # IMAGE_OPTIONAL_HEADER32 and IMAGE_OPTIONAL_HEADER64 both place
        # Subsystem at offset 0x44 from the optional-header start.
        $stream.Seek($optionalHeader + 0x44, [System.IO.SeekOrigin]::Begin) | Out-Null
        return [int]$reader.ReadUInt16()
    } finally {
        $stream.Dispose()
    }
}

function Get-TerminalAiPipeNames() {
    [System.IO.Directory]::GetFiles('\\.\pipe\') |
        Where-Object { $_ -like '*terminalai*' }
}

function Get-InstalledDaemonProcesses([string]$prefix, [int[]]$baselinePids) {
    $expectedPath = [System.IO.Path]::GetFullPath((Join-Path $prefix 'terminalai-daemon.exe'))
    @(Get-Process -Name 'terminalai-daemon' -ErrorAction SilentlyContinue | ForEach-Object {
        if ($baselinePids -contains $_.Id) { return }

        $path = $null
        try { $path = $_.Path } catch { return }
        if (-not $path) { return }

        try { $path = [System.IO.Path]::GetFullPath($path) } catch { return }
        if ([StringComparer]::OrdinalIgnoreCase.Equals($path, $expectedPath)) {
            $_
        }
    })
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

# The artifact can be perfect while the release describing it is not: v0.9.0
# shipped with no changelog section at all. Run the metadata gate here so one
# command still answers "may this be published". Tests are skipped because this
# gate already assumes a build the suites passed.
$metadataScript = Join-Path $PSScriptRoot 'verify-release-metadata.ps1'
& pwsh -NoProfile -File $metadataScript -SkipTests
if ($LASTEXITCODE -ne 0) {
    Fail 'release metadata is inconsistent — see the report above'
} else {
    Pass 'release metadata agrees with itself'
}

$prefix = Join-Path ([System.IO.Path]::GetTempPath()) ("terminalai-installcheck-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
$installed = $false
$process = $null
$daemonProcess = $null
$seedDaemon = $null

$storeDirectory = $null
if ($env:LOCALAPPDATA) { $storeDirectory = Join-Path $env:LOCALAPPDATA 'TerminalAI' }

function Get-QuarantinedStoreFiles() {
    if (-not $storeDirectory -or -not (Test-Path -LiteralPath $storeDirectory)) { return @() }
    @(Get-ChildItem -LiteralPath $storeDirectory -Filter 'sessions.corrupt-*.json' -ErrorAction SilentlyContinue)
}

function Install-Package([string]$package, [string]$target) {
    # NSIS: /S is silent, /D must be last and unquoted.
    $run = Start-Process -FilePath $package -ArgumentList "/S /D=$target" -PassThru
    # Touching Handle caches it, so ExitCode is still readable after the process
    # exits. Without this, a fast installer leaves an object whose ExitCode throws.
    $null = $run.Handle
    $run.WaitForExit()
    $run.ExitCode
}

try {
    if ($PreviousInstaller) {
        if (-not (Test-Path -LiteralPath $PreviousInstaller)) {
            throw "previous installer not found: $PreviousInstaller"
        }
        Write-Host "Seeding $prefix with the previous release: $PreviousInstaller"
        $seedExit = Install-Package $PreviousInstaller $prefix
        if ($seedExit -ne 0) {
            throw "the previous release's installer exited with code $seedExit; cannot stage an upgrade"
        }
        $installed = $true
        $seedDaemonExe = Join-Path $prefix 'terminalai-daemon.exe'
        if (-not (Test-Path -LiteralPath $seedDaemonExe)) {
            throw "the previous release installed no terminalai-daemon.exe; cannot stage an upgrade"
        }
        # The GUI is not what holds the lock. Starting the sidecar alone puts the
        # prefix in exactly the state an upgrade meets: a running daemon with an
        # open image section on the file the installer is about to overwrite.
        $seedDaemon = Start-Process -FilePath $seedDaemonExe -PassThru -WindowStyle Hidden
        $null = $seedDaemon.Handle
        Start-Sleep -Seconds 2
        if ($seedDaemon.HasExited) {
            Write-Host 'note: the previous release daemon exited immediately; the upgrade lock may not be staged' -ForegroundColor Yellow
        } else {
            Pass 'previous release installed and its daemon is running'
        }
    }

    Write-Host "Installing into $prefix"
    $installerExit = Install-Package $Installer $prefix
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
        $subsystem = Get-PeSubsystem $appExe
        if ($subsystem -ne 2) {
            Fail "installed terminalai.exe uses PE subsystem $subsystem; expected GUI subsystem 2"
        } else {
            Pass 'terminalai.exe uses the Windows GUI subsystem'
        }
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

        $baselineDaemonPids = @(
            Get-Process -Name 'terminalai-daemon' -ErrorAction SilentlyContinue |
                Select-Object -ExpandProperty Id
        )
        $baselineDaemonPipes = @(Get-TerminalAiPipeNames)
        if ($baselineDaemonPipes.Count -gt 0) {
            Write-Host (
                "note: TerminalAI pipe(s) already existed before launch ({0}); " +
                'the gate will require a new daemon process from the installed prefix'
            ) -f ($baselineDaemonPipes -join ', ') -ForegroundColor Yellow
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

        # A pre-existing daemon can make the app connect successfully without
        # starting the sidecar from this prefix. Require both a new installed
        # daemon process and its pipe; the pipe alone is not proof of the
        # installed sidecar.
        $deadline = (Get-Date).AddSeconds($LaunchTimeoutSeconds)
        $daemonPipeSeen = $false
        while ((Get-Date) -lt $deadline -and (-not $daemonProcess -or -not $daemonPipeSeen)) {
            $daemonCandidates = @(Get-InstalledDaemonProcesses $prefix $baselineDaemonPids)
            if ($daemonCandidates.Count -gt 0) {
                $daemonProcess = $daemonCandidates[0]
            }
            $daemonPipeSeen = @(Get-TerminalAiPipeNames).Count -gt 0
            if (-not $daemonProcess -or -not $daemonPipeSeen) {
                Start-Sleep -Milliseconds 500
            }
        }
        if (-not $daemonProcess) {
            Fail "no new terminalai-daemon.exe from the installed prefix appeared within ${LaunchTimeoutSeconds}s"
        } else {
            Pass "installed daemon process is running from $($daemonProcess.Path)"
        }
        if (-not $daemonPipeSeen) {
            Fail "no TerminalAI daemon pipe appeared within ${LaunchTimeoutSeconds}s"
        } else {
            Pass 'installed daemon control pipe is listening'
        }

        if ($process -and -not $process.HasExited) {
            Pass 'the application is still running after the daemon handshake'
        } else {
            Fail 'the installed application exited instead of staying up'
        }

        # The upgrade path. Every existing user installs over a prefix whose daemon is
        # still running by design, holding its named pipe and an open image section on
        # its own executable; a clean install into a scratch prefix never touches that.
        # Close the window and leave the daemon up, which is precisely the documented
        # steady state, then install over it.
        if ($failures.Count -eq 0 -and $daemonProcess) {
            Write-Host 'Upgrading over the running install'
            if ($process -and -not $process.HasExited) {
                Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
                $process.WaitForExit(10000) | Out-Null
            }
            $process = $null

            $daemonProcess.Refresh()
            if ($daemonProcess.HasExited) {
                Fail 'the daemon did not outlive its window, so the upgrade lock could not be staged'
            } else {
                Pass 'the daemon outlived its window, staging the upgrade lock'
            }

            $quarantineBefore = @(Get-QuarantinedStoreFiles | Select-Object -ExpandProperty FullName)
            $upgradeExit = Install-Package $Installer $prefix
            if ($upgradeExit -ne 0) {
                Fail "installing over the running daemon exited with code $upgradeExit"
            } else {
                Pass 'installer completed over a running daemon'
            }

            $daemonProcess.Refresh()
            if (-not $daemonProcess.HasExited) {
                Fail 'the previous daemon survived the upgrade; its executable was overwritten underneath it or not at all'
            } else {
                Pass 'the upgrade stopped the previous daemon before writing over it'
            }
            $daemonProcess = $null

            foreach ($sidecar in $declaredSidecars) {
                $sidecarExe = Join-Path $prefix "$sidecar.exe"
                if (-not (Test-Path -LiteralPath $sidecarExe)) {
                    Fail "sidecar missing after the upgrade: $sidecarExe"
                } else {
                    Pass "sidecar survived the upgrade: $sidecar.exe"
                }
            }

            $relaunch = Invoke-Isolation @('launch', '-FilePath', $appExe)
            if ($relaunch.ExitCode -ne 0) {
                Fail "the upgraded application drew no verified window on the isolated display: $($relaunch.Output)"
            } else {
                Pass 'the upgraded application starts'
            }

            $relaunchInfo = $relaunch.Output |
                Where-Object { $_.TrimStart().StartsWith('{') } |
                Select-Object -Last 1
            if ($relaunchInfo) {
                $appPid = ($relaunchInfo | ConvertFrom-Json).processId
                $process = Get-Process -Id $appPid -ErrorAction SilentlyContinue
            }

            $deadline = (Get-Date).AddSeconds($LaunchTimeoutSeconds)
            $upgradedDaemon = $null
            while ((Get-Date) -lt $deadline -and -not $upgradedDaemon) {
                $candidates = @(Get-InstalledDaemonProcesses $prefix $baselineDaemonPids)
                if ($candidates.Count -gt 0) {
                    $upgradedDaemon = $candidates[0]
                } else {
                    Start-Sleep -Milliseconds 500
                }
            }
            if (-not $upgradedDaemon) {
                Fail "no terminalai-daemon.exe from the upgraded prefix appeared within ${LaunchTimeoutSeconds}s"
            } else {
                $daemonProcess = $upgradedDaemon
                Pass 'the upgraded daemon is running'
            }

            # A store the upgraded daemon cannot parse is renamed aside rather than
            # reported, so silence here is not evidence: look for the rename.
            $newlyQuarantined = @(
                Get-QuarantinedStoreFiles |
                    Where-Object { $quarantineBefore -notcontains $_.FullName }
            )
            if ($newlyQuarantined.Count -gt 0) {
                Fail "the upgraded daemon quarantined the session store: $(($newlyQuarantined | Select-Object -ExpandProperty Name) -join ', ')"
            } else {
                Pass 'the session store survived the upgrade unquarantined'
            }
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
