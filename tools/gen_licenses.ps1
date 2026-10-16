# Generates THIRD-PARTY-LICENSES.txt for the release bundle.
#
# Reads the locked dependency graph (`cargo metadata --locked`) and writes one
# section per dependency: name, version, declared SPDX license, repository, and
# the text of any LICENSE/COPYING/NOTICE file shipped beside the crate source.
$ErrorActionPreference = "Stop"

$workspace = Split-Path $PSScriptRoot -Parent
$out = Join-Path $workspace "THIRD-PARTY-LICENSES.txt"
$rootName = "dlssg-patcher"

Push-Location $workspace
try {
    $meta = cargo metadata --format-version 1 --locked | ConvertFrom-Json
} finally {
    Pop-Location
}

$sb = [System.Text.StringBuilder]::new()
$null = $sb.AppendLine("THIRD-PARTY LICENSES")
$null = $sb.AppendLine()
$null = $sb.AppendLine("This build statically links the Rust crates listed below. Each is used")
$null = $sb.AppendLine("under its own license, reproduced from the crate source where included.")
$null = $sb.AppendLine()

$pkgs = $meta.packages | Where-Object { $_.name -ne $rootName } | Sort-Object name, version
foreach ($p in $pkgs) {
    $null = $sb.AppendLine("=" * 80)
    $null = $sb.AppendLine("$($p.name) $($p.version)")
    $license = if ($p.license) { $p.license } else { "see bundled license text" }
    $null = $sb.AppendLine("License: $license")
    $repo = if ($p.repository) { $p.repository } elseif ($p.homepage) { $p.homepage } else { "n/a" }
    $null = $sb.AppendLine("Repository: $repo")

    $dir = Split-Path $p.manifest_path -Parent
    $files = Get-ChildItem -Path $dir -File -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -match '^(LICENSE|COPYING|NOTICE)' } |
        Sort-Object Name
    if ($files) {
        foreach ($f in $files) {
            $null = $sb.AppendLine("-" * 80)
            $null = $sb.AppendLine($f.Name)
            $null = $sb.AppendLine((Get-Content -Raw -LiteralPath $f.FullName))
        }
    } else {
        $null = $sb.AppendLine("(No license file bundled; see the declared license above.)")
    }
    $null = $sb.AppendLine()
}

Set-Content -Path $out -Value $sb.ToString() -Encoding utf8
Write-Host "wrote $out ($($pkgs.Count) packages)"
