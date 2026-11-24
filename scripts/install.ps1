# Tinkr per-user installer for Windows. Verifies SHA-256 before installing.
param(
    [string]$Version = "0.1.0",
    [string]$Repo = "thunderkex/tinkr",
    [string]$InstallDir = "$env:LOCALAPPDATA\tinkr\bin"
)
$ErrorActionPreference = "Stop"

$Arch = if ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture -eq "Arm64") { "arm64" } else { "x64" }
$Asset = "tinkr-$Version-windows-$Arch.zip"
$Base = "https://github.com/$Repo/releases/download/v$Version"
$Tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("tinkr-install-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Force $Tmp | Out-Null

try {
    Invoke-WebRequest -Uri "$Base/$Asset" -OutFile "$Tmp/$Asset"
    Invoke-WebRequest -Uri "$Base/SHA256SUMS" -OutFile "$Tmp/SHA256SUMS"
    $Line = (Get-Content "$Tmp/SHA256SUMS") | Where-Object { $_ -match "\s+$Asset$" } | Select-Object -First 1
    if (-not $Line) { throw "Missing checksum for $Asset" }
    $Expected = ($Line -split "\s+")[0].ToLower()
    $Actual = (Get-FileHash "$Tmp/$Asset" -Algorithm SHA256).Hash.ToLower()
    if ($Expected -ne $Actual) { throw "SHA-256 mismatch: expected $Expected, got $Actual" }

    New-Item -ItemType Directory -Force $InstallDir | Out-Null
    Expand-Archive -Path "$Tmp/$Asset" -DestinationPath $InstallDir -Force
    $UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
    if (-not ($UserPath -split ";" | Where-Object { $_ -eq $InstallDir })) {
        $NewPath = if ([string]::IsNullOrWhiteSpace($UserPath)) { $InstallDir } else { "$UserPath;$InstallDir" }
        [Environment]::SetEnvironmentVariable("Path", $NewPath, "User")
    }
    Write-Host "Installed tinkr $Version to $InstallDir. Open a new terminal and run 'tinkr'."
} finally {
    Remove-Item -Recurse -Force $Tmp -ErrorAction SilentlyContinue
}
