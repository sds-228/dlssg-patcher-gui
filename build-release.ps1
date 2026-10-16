$ErrorActionPreference = "Stop"

$workspace = $PSScriptRoot
$userProfile = $env:USERPROFILE

$env:RUSTFLAGS = "--remap-path-prefix=$workspace=dlssg-patcher --remap-path-prefix=$userProfile=home"

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

# The full profile path is the reliable signal, so it is always checked.
$profileHits = ([regex]::Matches($ascii, [regex]::Escape($userProfile))).Count +
               ([regex]::Matches($utf16, [regex]::Escape($userProfile))).Count

# The bare username only means something when it is long enough to be specific;
# a short name ("sam", "dev") matches unrelated bytes and false-positives.
$nameHits = 0
if ($env:USERNAME.Length -ge 4) {
    $nameHits = ([regex]::Matches($ascii, [regex]::Escape($env:USERNAME))).Count +
                ([regex]::Matches($utf16, [regex]::Escape($env:USERNAME))).Count
}

Write-Host ""
Write-Host "Privacy self-check:"
Write-Host "  references to user profile : $profileHits"
Write-Host "  references to username     : $nameHits"
if ($profileHits -gt 0 -or $nameHits -gt 0) {
    Write-Host "WARNING: binary still references your user profile. Do not distribute."
    exit 1
}
Write-Host "OK: binary contains no reference to your user profile."
