# Build a release binary and bundle it into a zip for distribution.
#
#   powershell -ExecutionPolicy Bypass -File scripts\package.ps1
#
# Produces dist\macast-gpui-<version>-windows-x64.zip containing the exe, the
# docs and a note about mpv. The release profile caps codegen parallelism
# (-j 2) because gpui can crash rustc when many CGUs compile at once.

param(
    [string]$OutDir = "dist"
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

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

Write-Host "==> cargo build --release -p macast-app -j 2"
cargo build --release -p macast-app -j 2
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

$exe = Join-Path $root "target\release\macast-app.exe"
if (-not (Test-Path $exe)) { throw "missing $exe" }

$version = (Select-String -Path "Cargo.toml" -Pattern '^version = "(.*)"').Matches[0].Groups[1].Value
$stage = Join-Path $root "$OutDir\macast-gpui-$version"
# Do not delete the staging directory: overwrite in place so the script also
# works when a policy/tool blocks removals (and avoids touching unrelated files).
New-Item -ItemType Directory -Path $stage -Force | Out-Null

Copy-Item $exe $stage -Force
Copy-Item "README.md", "GUIDE.md", "LICENSE" $stage -Force
@"
macast-gpui $version
====================

1. Install mpv and make sure mpv.exe is on PATH (or set mpv_path in
   %APPDATA%\macast-gpui\config.json).
   Without mpv the app still runs, using the "null" player for protocol tests.

2. Run macast-app.exe. On first launch Windows Firewall asks for network
   access -- allow it on private networks, otherwise phones cannot discover
   this device.

3. Keep %APPDATA%\macast-gpui\config.json: it holds the device UDN. Deleting
   it makes phones treat this PC as a new device.

4. This build has no console window. Logs go to
   %APPDATA%\macast-gpui\logs\macast-gpui.log. Closing the window keeps the app
   in the system tray; right-click the tray icon and choose 退出 to quit.
"@ | Set-Content -Encoding UTF8 (Join-Path $stage "README-FIRST.txt")

$zip = Join-Path $root "$OutDir\macast-gpui-$version-windows-x64.zip"
if (Test-Path $zip) {
    # -Update replaces the entries we just rebuilt instead of deleting the zip.
    Compress-Archive -Path (Join-Path $stage "*") -DestinationPath $zip -Update
} else {
    Compress-Archive -Path (Join-Path $stage "*") -DestinationPath $zip
}
Write-Host "==> $zip"
