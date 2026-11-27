# Submitting Polled to WinGet (`winget install k1jaa.Polled`)

The manifests in this folder follow WinGet manifest schema `1.9.0` for portable ZIP installs (`polled-gui.exe` + `polled.exe`).

## 1. One-Time Initial Submission

1. Publish a GitHub Release (for example `v0.14.3` or later) and copy the 64-character SHA-256 hash from that release's `SHA256SUMS.txt` (convert to UPPERCASE).
2. Replace `REPLACE_WITH_SHA256_FROM_RELEASE_ZIP` in `k1jaa.Polled.installer.yaml` with that hash and verify `PackageVersion` / `InstallerUrl` / `ReleaseDate` match the published release.
3. Validate locally on Windows:
   ```powershell
   winget validate --manifest packaging\winget
   ```
4. Submit to [`microsoft/winget-pkgs`](https://github.com/microsoft/winget-pkgs) using [`wingetcreate`](https://github.com/microsoft/winget-create) (or a manual fork PR copying these 3 YAML files into `manifests/k/k1jaa/Polled/<version>/`):
   ```powershell
   winget install Microsoft.WingetCreate
   wingetcreate submit packaging\winget
   ```

## 2. Automatic Updates on Every Future Release

Once Microsoft merges the initial `k1jaa.Polled` PR, `.github/workflows/release.yml` already includes an automated `publish-winget` job using `vedantmgoyal9/winget-releaser@v2`:

1. Create a GitHub Personal Access Token (`public_repo` scope on your account with a fork of `microsoft/winget-pkgs`).
2. Add it as a repository secret named **`WINGET_GITHUB_TOKEN`** in `k1jaa/polled` -> Settings -> Secrets and variables -> Actions -> Secrets.
3. Add a repository variable named **`WINGET_AUTO_PUBLISH`** = **`true`** under Variables.

From that point on, pushing any `v*` tag automatically builds the Windows ZIP, publishes the GitHub Release with notes extracted from `CHANGELOG.md`, and opens a WinGet update PR with the exact SHA-256 hash.
