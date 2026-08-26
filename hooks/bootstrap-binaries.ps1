# SessionStart hook: ensure the devkit binaries this plugin drives are present
# and match its version, installing them from the matching GitHub release.
#
# PowerShell twin of `bootstrap-binaries`, for Windows hosts with no bash.
# Both resolve the same state paths, so a machine that later gains Git Bash
# does not reinstall.
#
# Every failure path exits 0: a session must start even with no network.

$ErrorActionPreference = 'Continue'

if ($env:DEVKIT_NO_BOOTSTRAP -eq '1') { exit 0 }

$repoUrl = 'https://github.com/AbysmalBiscuit/devkit'
$pluginRoot = Split-Path -Parent $PSScriptRoot
$manifest = Join-Path $pluginRoot '.claude-plugin/plugin.json'

function Write-Note($message) { [Console]::Error.WriteLine("devkit plugin: $message") }

$version = $null
try {
    $version = (Get-Content -Raw -ErrorAction Stop $manifest | ConvertFrom-Json).version
} catch {}
if ([string]::IsNullOrWhiteSpace($version)) {
    Write-Note "could not read a version from ${manifest}; skipping binary bootstrap"
    exit 0
}

$stateRoot = if ($env:XDG_STATE_HOME) { $env:XDG_STATE_HOME } else { Join-Path $HOME '.local/state' }
$stateDir = Join-Path $stateRoot 'devkit'
$stamp = Join-Path $stateDir 'bootstrap-version'
$failed = Join-Path $stateDir 'bootstrap-failed'

function Read-Marker($path) {
    try { (Get-Content -Raw -ErrorAction Stop $path).Trim() } catch { '' }
}

$missing = @('devkit') | Where-Object { -not (Get-Command $_ -ErrorAction SilentlyContinue) }

if ($missing.Count -eq 0) {
    switch (Read-Marker $stamp) {
        $version { exit 0 }
        # Binaries we did not install (cargo, a source build). Record that and
        # never overwrite them; upgrades stay the user's call.
        { $_ -eq '' -or $_ -eq 'external' } {
            New-Item -ItemType Directory -Force -Path $stateDir -ErrorAction SilentlyContinue | Out-Null
            Set-Content -Path $stamp -Value 'external' -ErrorAction SilentlyContinue
            exit 0
        }
        default { $action = "updating devkit binaries to $version" }
    }
} else {
    $action = "installing devkit binaries $version (missing: $($missing -join ' '))"
}

# A pinned installer keeps the binaries in lockstep with the hooks and MCP
# server this plugin version ships.
if ((Read-Marker $failed) -eq $version) {
    Write-Note "install of $version failed previously; run the installer yourself or remove $failed to retry"
    exit 0
}

Write-Note $action

$status = 0
try {
    $installer = "$repoUrl/releases/download/v$version/devkit-installer.ps1"
    Invoke-RestMethod -Uri $installer -TimeoutSec 300 -ErrorAction Stop | Invoke-Expression
} catch {
    $status = 1
    Write-Note "installer failed: $($_.Exception.Message)"
}

New-Item -ItemType Directory -Force -Path $stateDir -ErrorAction SilentlyContinue | Out-Null
if ($status -ne 0) {
    Set-Content -Path $failed -Value $version -ErrorAction SilentlyContinue
    Write-Note "see $repoUrl#install to install manually"
    exit 0
}

Set-Content -Path $stamp -Value $version -ErrorAction SilentlyContinue
Remove-Item -Path $failed -Force -ErrorAction SilentlyContinue
Write-Note "devkit $version installed; binaries land on PATH via CARGO_HOME (restart the session if they are not found yet)"
exit 0
