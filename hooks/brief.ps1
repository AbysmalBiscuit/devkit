# Emit the devkit project brief when devkit is installed. Silent and exit 0
# otherwise. PowerShell twin of `brief`, for Windows hosts with no bash.
# Stdin is forwarded so --if-changed can read session_id.

$ErrorActionPreference = 'SilentlyContinue'

if (-not (Get-Command devkit -ErrorAction SilentlyContinue)) { exit 0 }

$payload = [Console]::In.ReadToEnd()
$payload | & devkit brief @args
exit 0
