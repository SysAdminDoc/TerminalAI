<#
.SYNOPSIS
Run the deterministic 30-session fleet reliability and resource gate.

.DESCRIPTION
This is a headless release check. It builds the probe, runs the injected-domain
fleet profile, and fails if the bounded registry gates or Windows process
resource budgets fail. No real agent, credential, browser or window is used.

The profile budgets are intentionally documented in the JSON report:
60 seconds for synthetic startup, 100 ms hook p95, 500 ms snapshot p95 and
256 MiB working-set growth. The registry also proves the subscriber cap,
scrollback cap, store round trip and malformed-store rejection.

.EXAMPLE
pwsh -NoProfile -File scripts/verify-fleet-stress.ps1
#>
[CmdletBinding()]
param(
    [int]$Sessions = 30,
    [int]$EventsPerSession = 64
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repoRoot = Split-Path -Parent $PSScriptRoot
$probe = Join-Path $repoRoot 'target/release/terminalai-probe.exe'
$reportFile = New-TemporaryFile

Push-Location $repoRoot
try {
    Write-Host "building terminalai-probe for a $Sessions-session stress profile ..." -ForegroundColor Cyan
    & cargo build --release -p terminalai-probe
    if ($LASTEXITCODE -ne 0) { throw "terminalai-probe release build failed (exit $LASTEXITCODE)" }

    & $probe fleet-stress --sessions $Sessions --events-per-session $EventsPerSession --json --output $reportFile.FullName
    if ($LASTEXITCODE -ne 0) { throw "fleet-stress failed (exit $LASTEXITCODE)" }

    $report = Get-Content -LiteralPath $reportFile.FullName -Raw | ConvertFrom-Json
    if (-not $report.profile.gates.all_pass) {
        throw "logical fleet stress gates failed: $($report.profile.gates | ConvertTo-Json -Compress)"
    }
    if (-not $report.resources.enforced) {
        throw 'resource budgets were not enforced on this Windows host'
    }
    if (-not $report.resources.all_pass) {
        throw "resource budgets failed: $($report.resources | ConvertTo-Json -Compress)"
    }
    if ([int]$report.profile.sessions -ne $Sessions) {
        throw "stress report covered $($report.profile.sessions) sessions, expected $Sessions"
    }

    Write-Host "PASS  $($report.profile.sessions) sessions / $($report.profile.events) events" -ForegroundColor Green
    Write-Host "      startup=$([math]::Round($report.profile.startup_ms, 2)) ms; hook p95=$([math]::Round($report.profile.hooks.p95_ms, 3)) ms; snapshot p95=$([math]::Round($report.profile.snapshots.p95_ms, 3)) ms"
    Write-Host "      rss delta=$([math]::Round($report.resources.working_set_delta_bytes / 1MB, 2)) MiB; dropped bounded events=$($report.profile.dropped_events); restored=$($report.profile.recovery.restored_sessions)"
} finally {
    Pop-Location
    Remove-Item -LiteralPath $reportFile.FullName -Force -ErrorAction SilentlyContinue
}

exit 0
