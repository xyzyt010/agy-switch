$dest = "$env:LOCALAPPDATA\bin"
$currentPath = [Environment]::GetEnvironmentVariable("PATH", "User")
if ($currentPath -notlike "*$dest*") {
    [Environment]::SetEnvironmentVariable("PATH", "$currentPath;$dest", "User")
    Write-Host "Added $dest to PATH. Restart your terminal." -ForegroundColor Yellow
} else {
    Write-Host "$dest is already in PATH." -ForegroundColor Cyan
}
