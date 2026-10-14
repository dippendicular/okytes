# Remove only the exact private signing certificate imported on this CI runner.
$ErrorActionPreference = 'Stop'
if (-not $env:WINDOWS_CERTIFICATE_THUMBPRINT) { exit 0 }
if (-not $env:RUNNER_TEMP) { throw 'Signing cleanup is restricted to an ephemeral CI runner' }
if ($env:WINDOWS_CERTIFICATE_THUMBPRINT -notmatch '^[A-Fa-f0-9]{40}$') { throw 'Invalid certificate thumbprint' }
$certificatePath = "Cert:\CurrentUser\My\$env:WINDOWS_CERTIFICATE_THUMBPRINT"
if (Test-Path -LiteralPath $certificatePath) { Remove-Item -LiteralPath $certificatePath -DeleteKey -Force }
