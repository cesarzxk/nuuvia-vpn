#Requires -RunAsAdministrator
# Installs the signed tap-windows6 driver used by nuvia for Layer 2.
# Source: https://github.com/OpenVPN/tap-windows6/releases (GPLv2).

param(
    [string]$DriverDir = "",
    [switch]$Force
)

$ErrorActionPreference = "Stop"

if ($DriverDir) {
    $dir = $DriverDir
} else {
    $version = "9.27.0"
    $url = "https://github.com/OpenVPN/tap-windows6/releases/download/$version/dist.win10.zip"
    $zip = Join-Path $env:TEMP "tap-windows-$version.zip"
    $dir = Join-Path $env:TEMP "nuvia-tap-$version"

    Write-Host "==> downloading tap-windows6 $version"
    if (-not (Test-Path $zip)) {
        Invoke-WebRequest -Uri $url -OutFile $zip
    }

    if (Test-Path $dir) {
        Remove-Item -Recurse -Force $dir
    }
    Expand-Archive -Path $zip -DestinationPath $dir -Force

    $arch = if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "arm64" } else { "amd64" }
    $dir = Join-Path $dir "dist.win10\$arch"
}

# Accept both the flat layout used by the installer/zip and the old
# driver\amd64 layout.
if (-not (Test-Path (Join-Path $dir "OemVista.inf"))) {
    $nested = Join-Path $dir "amd64"
    if (Test-Path (Join-Path $nested "OemVista.inf")) {
        $dir = $nested
    }
}

$inf = Join-Path $dir "OemVista.inf"
$devcon = Join-Path $dir "devcon.exe"
if (-not (Test-Path $inf)) {
    throw "OemVista.inf not found in $dir"
}
if (-not (Test-Path $devcon)) {
    # ARM64 package has no devcon; the x64 binary runs under emulation.
    $devcon = Join-Path (Split-Path $dir -Parent) "amd64\devcon.exe"
}
if (-not (Test-Path $devcon)) {
    throw "devcon.exe not found in $dir"
}

function Test-TapDriver {
    # First check the installed INF files directly. This avoids depending on
    # the localized output of pnputil.
    $inf = Get-ChildItem "$env:WINDIR\INF\oem*.inf" -ErrorAction SilentlyContinue |
        Select-String -Pattern 'tap0901|TAP-Windows Provider' -Quiet
    if ($inf) {
        return $true
    }
    $output = & pnputil.exe /enum-drivers 2>$null | Out-String
    return [bool]($output -match '(?i)(oemvista\.inf|tap-windows|tap0901)')
}

$tapDevices = @(
    Get-PnpDevice -Class Net -ErrorAction SilentlyContinue |
        Where-Object { $_.FriendlyName -like "*TAP-Windows*" }
)

$driverPresent = (Test-TapDriver) -or ($tapDevices.Count -gt 0)
$needInstall = -not $driverPresent
if ($Force) {
    $needInstall = $true
}

if ($needInstall) {
    Write-Host "==> installing driver/device from $inf"
    & $devcon install $inf TAP0901
    $code = $LASTEXITCODE
    if ($code -ne 0) {
        if (Test-TapDriver) {
            Write-Warning "devcon returned $code, but the tap0901 driver is registered"
        } else {
            throw "devcon failed with exit code $code"
        }
    }
    if (-not (Test-TapDriver)) {
        throw "tap0901 driver was not registered"
    }
    Start-Sleep -Seconds 1
} else {
    Write-Host "==> tap-windows6 driver already installed"
}

# Keep one adapter named nuvia0. If the driver created a default TAP, rename the
# first one; the description will still be "TAP-Windows Adapter V9".
$nuvia = Get-NetAdapter -Name "nuvia0" -IncludeHidden -ErrorAction SilentlyContinue
if (-not $nuvia) {
    $tap = Get-NetAdapter -IncludeHidden -ErrorAction SilentlyContinue |
        Where-Object { $_.InterfaceDescription -like "*TAP-Windows*" } |
        Select-Object -First 1
    if ($tap) {
        try {
            Rename-NetAdapter -Name $tap.Name -NewName "nuvia0" -ErrorAction Stop
            Write-Host "==> renamed TAP adapter $($tap.Name) to nuvia0"
            Start-Sleep -Milliseconds 500
        } catch {
            Write-Warning "could not rename $($tap.Name) to nuvia0: $_"
        }
    } else {
        Write-Host "==> no TAP adapter yet; nuvia creates nuvia0 on first connection"
    }
} else {
    Write-Host "==> adapter nuvia0 already exists"
}

Write-Host ""
Write-Host "TAP-Windows6 installed."
Write-Host "nuvia creates the shared nuvia0 adapter automatically on first connection."

exit 0
