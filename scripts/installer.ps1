# Build a Windows installer with crabnebula-dev/cargo-packager.
#
#   powershell -ExecutionPolicy Bypass -File scripts\installer.ps1
#   powershell -ExecutionPolicy Bypass -File scripts\installer.ps1 -Msi
#
# Produces dist\macast-gpui_<version>_x64-setup.exe (NSIS) or the MSI equivalent.
# The configuration lives in crates/macast-app/Cargo.toml under
# [package.metadata.packager] - that is what the CLI auto-discovers.
#
# Requires the `cargo-packager` CLI v0.11 on PATH:
#   cargo install cargo-packager --locked
#   (or take a prebuilt binary from
#    https://github.com/crabnebula-dev/cargo-packager/releases)
# NSIS itself is downloaded by cargo-packager on first run; the MSI format
# additionally needs the WiX Toolset installed.

param(
    [switch]$Msi
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

if (-not (Get-Command cargo-packager -ErrorAction SilentlyContinue)) {
    throw "cargo-packager CLI not found on PATH; run: cargo install cargo-packager --locked"
}

# gpui compiles its HLSL with fxc.exe from the Windows SDK. It only probes PATH
# and one hard-coded SDK version, so point it at whichever SDK is installed.
if (-not $env:GPUI_FXC_PATH) {
    $fxc = Get-ChildItem "C:\Program Files (x86)\Windows Kits\10\bin" -Recurse -Filter fxc.exe -ErrorAction SilentlyContinue |
        Where-Object { $_.FullName -like "*\x64\fxc.exe" } |
        Sort-Object FullName -Descending |
        Select-Object -First 1
    if ($fxc) {
        $env:GPUI_FXC_PATH = $fxc.FullName
        Write-Host "==> using fxc: $($fxc.FullName)"
    }
}

# cargo-packager packages an existing binary (it does not build one itself when
# the config comes from Cargo.toml metadata), so build the release profile first.
Write-Host "==> cargo build --release -p macast-app -j 2"
cargo build --release -p macast-app -j 2
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

$format = if ($Msi) { "wix" } else { "nsis" }
Write-Host "==> cargo packager -p macast-app -f $format -r"
cargo packager -p macast-app -f $format -r
if ($LASTEXITCODE -ne 0) { throw "cargo packager failed" }

Get-ChildItem "$root\dist" -Filter "*-setup.*" | Select-Object -ExpandProperty FullName
