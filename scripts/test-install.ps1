# Cross-platform install + end-to-end verification script (Windows).
param([string]$TinkrExe = ".\target\release\tinkr.exe")
$ErrorActionPreference = "Stop"

$Tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("tinkr-e2e-" + [guid]::NewGuid().ToString("N"))
$env:TINKR_HOME = Join-Path $Tmp "tinkr-home"
$FakeHome = Join-Path $Tmp "fake-home"
New-Item -ItemType Directory -Force "$FakeHome\.claude", "$FakeHome\.codex", "$FakeHome\.cursor" | Out-Null

try {
    Write-Host "1. Version & doctor:"
    & $TinkrExe --version
    & $TinkrExe doctor

    Write-Host "`n2. Real token-cutting test via 'tinkr run':"
    & $TinkrExe run --shell pwsh -- "Get-ChildItem -Recurse crates" | Select-Object -Last 8

    Write-Host "`n3. Ledger savings ('tinkr gain'):"
    & $TinkrExe gain

    Write-Host "`n4. Undo --purge:"
    & $TinkrExe undo --purge
    Write-Host "E2E OK"
} finally {
    Remove-Item -Recurse -Force $Tmp -ErrorAction SilentlyContinue
}
