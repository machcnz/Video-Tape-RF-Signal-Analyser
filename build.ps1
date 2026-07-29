# build.ps1 - Build CLI and GUI with version from VERSION file
$version = (Get-Content "$PSScriptRoot\VERSION").Trim()
Write-Host "Building VHS RF Signal Analyser v$version" -ForegroundColor Green

# Sync version into Cargo.toml (only the [package] version line)
$cargoLines = Get-Content "$PSScriptRoot\Cargo.toml"
$inPackage = $false
for ($i = 0; $i -lt $cargoLines.Count; $i++) {
    if ($cargoLines[$i] -match '^\[package\]') { $inPackage = $true; continue }
    if ($cargoLines[$i] -match '^\[') { $inPackage = $false }
    if ($inPackage -and $cargoLines[$i] -match '^version\s*=') {
        $cargoLines[$i] = "version = `"$version.0`""
        break
    }
}
$cargoLines | Set-Content "$PSScriptRoot\Cargo.toml"
Sleep 2
# Build Rust backend
cargo build --release --bin compare-rf
if ($LASTEXITCODE -ne 0) { Write-Host "Rust build failed" -ForegroundColor Red; exit 1 }

# Build GUI exe
python -m PyInstaller SignalCompareGUI.spec --distpath "dist_v$version" --noconfirm
if ($LASTEXITCODE -ne 0) { Write-Host "PyInstaller build failed" -ForegroundColor Red; exit 1 }

Write-Host "`nBuild complete:" -ForegroundColor Green
Write-Host "  CLI: target\release\compare-rf.exe"
Write-Host "  GUI: dist_v$version\SignalCompareGUI.exe"
