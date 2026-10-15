$ErrorActionPreference = "Stop"

$workspace = $PSScriptRoot
$profile = $env:USERPROFILE

$env:RUSTFLAGS = "--remap-path-prefix=$workspace=dlssg-patcher --remap-path-prefix=$profile=home"

Write-Host "Building release with anonymous paths:"
Write-Host "  <workspace>   -> dlssg-patcher"
Write-Host "  <userprofile> -> home"
Write-Host ""

cargo build --release --locked
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$exe = Join-Path $workspace "target\release\dlssg-patcher.exe"
$bytes = [System.IO.File]::ReadAllBytes($exe)
$ascii = [System.Text.Encoding]::ASCII.GetString($bytes)
$utf16 = [System.Text.Encoding]::Unicode.GetString($bytes)
$profileHits = ([regex]::Matches($ascii, [regex]::Escape($profile))).Count
$asciiHits = ([regex]::Matches($ascii, [regex]::Escape($env:USERNAME))).Count
$utf16Hits = ([regex]::Matches($utf16, [regex]::Escape($env:USERNAME))).Count

Write-Host ""
Write-Host "Privacy self-check:"
Write-Host "  references to user profile : $profileHits"
Write-Host "  references to username     : $($asciiHits + $utf16Hits)"
if ($profileHits -gt 0 -or $asciiHits -gt 0 -or $utf16Hits -gt 0) {
    Write-Host "WARNING: binary still references your user profile. Do not distribute."
    exit 1
}
Write-Host "OK: binary contains no reference to your user profile."