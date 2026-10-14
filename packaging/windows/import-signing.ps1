# Used only by the protected release environment. Never print certificate material.
$ErrorActionPreference = 'Stop'
if (-not $env:WINDOWS_CERTIFICATE) { exit 0 }
if (-not $env:WINDOWS_CERTIFICATE_PASSWORD) { throw 'Certificate password is required' }
$certificatePath = Join-Path $env:RUNNER_TEMP 'okytes-signing.pfx'
try {
    [System.IO.File]::WriteAllBytes($certificatePath, [Convert]::FromBase64String($env:WINDOWS_CERTIFICATE))
    $password = ConvertTo-SecureString $env:WINDOWS_CERTIFICATE_PASSWORD -AsPlainText -Force
    $certificates = @(Import-PfxCertificate -FilePath $certificatePath -CertStoreLocation Cert:\CurrentUser\My -Password $password | Where-Object HasPrivateKey)
    if ($certificates.Count -ne 1) { throw 'PFX must contain exactly one signing certificate with a private key' }
    $certificate = $certificates[0]
    "WINDOWS_CERTIFICATE_THUMBPRINT=$($certificate.Thumbprint)" | Out-File -FilePath $env:GITHUB_ENV -Encoding utf8 -Append
} finally {
    Remove-Item -LiteralPath $certificatePath -Force -ErrorAction SilentlyContinue
}
