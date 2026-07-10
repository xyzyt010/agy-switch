# install.ps1 — AGY-SWITCH installer for Windows
# Usage: Right-click > "Run with PowerShell" or run: .\install.ps1

$ErrorActionPreference = "Stop"

$dest = "$env:LOCALAPPDATA\bin"
New-Item -ItemType Directory -Force -Path $dest | Out-Null

# Copy both binary names
$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$src = Join-Path $scriptDir "agy-switch.exe"

if (-not (Test-Path $src)) {
    Write-Host "Error: agy-switch.exe not found in $scriptDir" -ForegroundColor Red
    Write-Host "Download it from the releases page first." -ForegroundColor Yellow
    exit 1
}

Copy-Item $src "$dest\agy-switch.exe" -Force
Copy-Item $src "$dest\agy.exe" -Force
Write-Host "Copied agy-switch.exe and agy.exe to $dest" -ForegroundColor Green

# Add to PATH if not already there
$currentPath = [Environment]::GetEnvironmentVariable("PATH", "User")
if ($currentPath -notlike "*$dest*") {
    [Environment]::SetEnvironmentVariable("PATH", "$currentPath;$dest", "User")
    Write-Host "Added $dest to PATH. Restart your terminal." -ForegroundColor Yellow
} else {
    Write-Host "$dest is already in PATH." -ForegroundColor Cyan
}

# Create config directory
$configDir = "$env:APPDATA\agy-switch"
New-Item -ItemType Directory -Force -Path $configDir | Out-Null
Write-Host "Created config directory: $configDir" -ForegroundColor Cyan

Write-Host "`nInstallation complete!" -ForegroundColor Green
Write-Host "Run 'agy-switch --version' to verify." -ForegroundColor Cyan
