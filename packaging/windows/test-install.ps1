param(
    [Parameter(Mandatory = $true)][string]$Installer,
    [Parameter(Mandatory = $true)][string]$Destination
)
$ErrorActionPreference = 'Stop'
$workspace = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
$targetDirectory = [System.IO.Path]::GetFullPath($Destination)
if (-not $targetDirectory.StartsWith($workspace + [System.IO.Path]::DirectorySeparatorChar, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw 'Installer test destination must be inside this workspace'
}
if (Test-Path -LiteralPath $targetDirectory) { throw 'Use a fresh installer test directory' }
if (Test-Path 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\Okytes') {
    throw 'Existing Okytes registration found; use a clean VM to avoid changing it'
}
$installerPath = (Resolve-Path -LiteralPath $Installer).Path
$process = Start-Process -FilePath $installerPath -ArgumentList @('/S', "/D=$targetDirectory") -WindowStyle Hidden -PassThru -Wait
if ($process.ExitCode -ne 0) { throw "NSIS install failed: $($process.ExitCode)" }
$executable = Join-Path $targetDirectory 'okytes-desktop.exe'
if (-not (Test-Path -LiteralPath $executable)) { throw 'Installer did not install the desktop executable' }
try {
    $env:OKYTES_INSTALLED_DESKTOP = $executable
    & node (Join-Path $workspace 'apps/desktop/scripts/native-e2e.mjs')
    if ($LASTEXITCODE -ne 0) { throw 'Installed WebView/recovery checks failed' }
} finally {
    Remove-Item Env:\OKYTES_INSTALLED_DESKTOP -ErrorAction SilentlyContinue
    $uninstaller = Join-Path $targetDirectory 'uninstall.exe'
    if (Test-Path -LiteralPath $uninstaller) {
        $process = Start-Process -FilePath $uninstaller -ArgumentList '/S' -WindowStyle Hidden -PassThru -Wait
        if ($process.ExitCode -ne 0) { throw "NSIS uninstall failed: $($process.ExitCode)" }
    }
}
for ($attempt = 0; $attempt -lt 100 -and (Test-Path -LiteralPath $executable); $attempt++) {
    Start-Sleep -Milliseconds 100
}
if (Test-Path -LiteralPath $executable) { throw 'Uninstaller left the application executable behind' }
if (Test-Path 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\Okytes') {
    throw 'Uninstaller left the Okytes registration behind'
}
Write-Output 'NSIS installation, installed desktop recovery, executable removal and registration removal passed. Cross-version upgrade remains unverified.'
