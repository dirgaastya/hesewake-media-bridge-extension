# Build the release program, then the Inno Setup installer.
# Use: powershell -ExecutionPolicy Bypass -File scripts\build-installer.ps1
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

$version = (Select-String -Path 'bridge\Cargo.toml' -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
Write-Host "==> version $version; cargo build --release"
Push-Location bridge
cargo build --release --locked
if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }
Pop-Location

$candidates = @(
  (Get-Command iscc -ErrorAction SilentlyContinue).Source,
  "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
  "$env:ProgramFiles\Inno Setup 6\ISCC.exe",
  "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe"
) | Where-Object { $_ -and (Test-Path $_) }
if (-not $candidates) { throw 'ISCC.exe not found. Install Inno Setup 6 (winget install JRSoftware.InnoSetup) and run again.' }
$iscc = $candidates | Select-Object -First 1

Write-Host "==> $iscc"
& $iscc "/DAppVersion=$version" 'installer\hesewake-media-bridge.iss'
if ($LASTEXITCODE -ne 0) { throw 'ISCC failed' }

Get-ChildItem installer_output -Filter '*.exe' | Sort-Object LastWriteTime -Descending | Select-Object -First 1 Name, Length, LastWriteTime
