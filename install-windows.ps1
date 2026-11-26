<#
.SYNOPSIS
Builds and installs the GString Captions OBS audio filter plugin on Windows.

.PARAMETER ServerUrl
Optional default overlay server URL compiled into the plugin (e.g. https://overlay.gstringbot.com).
Not required when using a full OBS plugin key copied from the GString dashboard.
#>
param(
    [string]$ServerUrl = ""
)

$ErrorActionPreference = "Stop"

$pluginDir = Split-Path -Parent $MyInvocation.MyCommand.Path
Push-Location $pluginDir
try {
    if ($ServerUrl) {
        $env:GSTRING_OVERLAY_URL = $ServerUrl
    }
    Write-Host "Building gstring-captions-obs (release)..."
    cargo build --release
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build failed"
    }

    $dll = Join-Path $pluginDir "target\release\gstring_captions.dll"
    if (-not (Test-Path $dll)) {
        throw "Expected DLL not found at $dll"
    }

    $destDir = Join-Path $env:ProgramData "obs-studio\plugins\gstring-captions\bin\64bit"
    New-Item -ItemType Directory -Force -Path $destDir | Out-Null
    $dest = Join-Path $destDir "gstring-captions.dll"
    Copy-Item -Force $dll $dest

    Write-Host "Installed OBS plugin to: $dest"
    Write-Host "Restart OBS Studio, then add the 'GString Captions' filter to your Mic or audio source."
} finally {
    Pop-Location
}
