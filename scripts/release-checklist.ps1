# Polled Release Packaging & Verification Checklist
# Usage: .\scripts\release-checklist.ps1 [-Version "0.12.0"] [-SkipBuild]
param(
    [string]$Version = "",
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"
$RepoRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
Push-Location $RepoRoot

try {
    if ([string]::IsNullOrWhiteSpace($Version)) {
        $CargoToml = Get-Content (Join-Path $RepoRoot "Cargo.toml") -Raw
        if ($CargoToml -match '(?m)^version\s*=\s*"([^"]+)"') {
            $Version = $Matches[1]
        } else {
            throw "Could not detect workspace version from Cargo.toml"
        }
    }

    Write-Host "=== Polled v$Version Release Verification ===" -ForegroundColor Cyan

    Write-Host "[1/6] Checking formatting..."
    cargo fmt --all -- --check
    if ($LASTEXITCODE -ne 0) { throw "cargo fmt check failed" }

    Write-Host "[2/6] Running clippy (-D warnings)..."
    cargo clippy --workspace --all-targets -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw "cargo clippy failed" }

    Write-Host "[3/6] Running full workspace test suite..."
    cargo test --workspace
    if ($LASTEXITCODE -ne 0) { throw "cargo test failed" }

    if (-not $SkipBuild) {
        Write-Host "[4/6] Building release binaries (polled-cli + polled-gui)..."
        cargo build --release --workspace
        if ($LASTEXITCODE -ne 0) { throw "cargo build --release failed" }
    } else {
        Write-Host "[4/6] Skipping release build (-SkipBuild)."
    }

    $CliExe = Join-Path $RepoRoot "target\release\polled.exe"
    $GuiExe = Join-Path $RepoRoot "target\release\polled-gui.exe"
    if (-not (Test-Path $CliExe) -or -not (Test-Path $GuiExe)) {
        throw "Release binaries missing in target\release (ensure polled-gui.exe is closed before building)."
    }

    Write-Host "[5/6] Staging portable release ZIP..."
    $DistDir = Join-Path $RepoRoot "dist"
    $StageDir = Join-Path $DistDir "polled-v$Version-windows-x64"
    $ZipName = "polled-v$Version-windows-x64.zip"
    $ZipPath = Join-Path $DistDir $ZipName
    if (Test-Path $StageDir) { Remove-Item $StageDir -Recurse -Force }
    if (Test-Path $ZipPath) { Remove-Item $ZipPath -Force }
    New-Item -ItemType Directory -Path $StageDir -Force | Out-Null

    Copy-Item $GuiExe $StageDir
    Copy-Item $CliExe $StageDir
    Copy-Item (Join-Path $RepoRoot "README.md") $StageDir
    Copy-Item (Join-Path $RepoRoot "LICENSE") $StageDir
    Copy-Item (Join-Path $RepoRoot "CHANGELOG.md") $StageDir
    $DocsStage = Join-Path $StageDir "docs"
    New-Item -ItemType Directory -Path $DocsStage -Force | Out-Null
    Copy-Item (Join-Path $RepoRoot "docs\COMPATIBILITY.md") $DocsStage
    Copy-Item (Join-Path $RepoRoot "docs\SYSTEM_MODIFICATIONS.md") $DocsStage

    Compress-Archive -Path (Join-Path $StageDir "*") -DestinationPath $ZipPath -CompressionLevel Optimal

    $ZipHash = (Get-FileHash $ZipPath -Algorithm SHA256).Hash.ToLowerInvariant()
    $ZipHashUpper = $ZipHash.ToUpperInvariant()
    $CliHash = (Get-FileHash $CliExe -Algorithm SHA256).Hash.ToLowerInvariant()
    $GuiHash = (Get-FileHash $GuiExe -Algorithm SHA256).Hash.ToLowerInvariant()

    $ChecksumsPath = Join-Path $DistDir "SHA256SUMS.txt"
    "$ZipHash  $ZipName" | Set-Content -Path $ChecksumsPath -Encoding ASCII

    Write-Host "[6/6] Release packaging summary"
    Write-Host ""
    Write-Host "Release archive ready:" -ForegroundColor Green
    Write-Host "  ZIP:       $ZipPath"
    Write-Host "  SHA-256:   $ZipHash"
    Write-Host "  polled-gui: $GuiHash"
    Write-Host "  polled:     $CliHash"
    Write-Host "  Checksums: $ChecksumsPath"
    Write-Host ""
    Write-Host "Next steps to publish:" -ForegroundColor Yellow
    Write-Host "  1. git push origin main; git tag v$Version; git push origin v$Version"
    Write-Host "  2. Wait for GitHub Actions release.yml to publish polled-v$Version-windows-x64.zip + SHA256SUMS.txt"
    Write-Host "  3. For WinGet first-time PR, copy the published release ZIP SHA-256 (uppercase) into:"
    Write-Host "     packaging/winget/k1jaa.Polled.installer.yaml (example local hash: $ZipHashUpper)"
    Write-Host "  4. Validate with: winget validate --manifest packaging/winget"
} finally {
    Pop-Location
}
