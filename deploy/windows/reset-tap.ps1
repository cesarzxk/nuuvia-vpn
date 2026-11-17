#Requires -RunAsAdministrator
# Removes stale TAP-Windows adapters created by old nuvia attempts.
# WARNING: this also removes TAP adapters used by OpenVPN or another VPN.
# Close those programs first. The driver package itself is kept; run
# install-tap.ps1 afterwards to create a clean adapter.

param(
    [switch]$Force
)

$ErrorActionPreference = "Stop"

$devices = @(
    Get-PnpDevice -Class Net -ErrorAction SilentlyContinue |
        Where-Object { $_.FriendlyName -like "*TAP-Windows*" }
)

if ($devices.Count -eq 0) {
    Write-Host "Nenhum adaptador TAP-Windows encontrado."
    exit 0
}

$devices | Format-Table FriendlyName, Status, InstanceId -AutoSize

if (-not $Force) {
    $answer = Read-Host "Remover $($devices.Count) adaptador(es) TAP-Windows? [s/N]"
    if ($answer -notmatch '^(s|sim|y|yes)$') {
        Write-Host "Cancelado."
        exit 0
    }
}

foreach ($device in $devices) {
    Write-Host "Removendo $($device.FriendlyName) ..."
    & pnputil.exe /remove-device $device.InstanceId
    if ($LASTEXITCODE -ne 0) {
        Write-Warning "Falha ao remover $($device.InstanceId)"
    }
}

Write-Host ""
Write-Host "Pronto. Agora rode:"
Write-Host "  .\\install-tap.ps1 -DriverDir .\\driver -Force"
