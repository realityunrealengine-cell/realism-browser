# Publishes a GitHub release with the setup and SHA256SUMS.txt.
# Usage (after building the browser and the installer in release mode):  powershell -File tools/release.ps1
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
$ver = (Select-String -Path "$root\Cargo.toml" -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
$tag = "v$ver"
$out = "$root\dist"
New-Item -ItemType Directory -Force $out | Out-Null
$setup = "$out\Realism-Browser-Setup.exe"
Copy-Item "$root\installer\target\release\realism-setup.exe" $setup -Force
$hash = (Get-FileHash $setup -Algorithm SHA256).Hash.ToLower()
"$hash  Realism-Browser-Setup.exe" | Set-Content "$out\SHA256SUMS.txt" -Encoding ascii
$notes = if (Test-Path "$root\tools\release-notes.md") { "$root\tools\release-notes.md" } else { $null }
if ($notes) { gh release create $tag $setup "$out\SHA256SUMS.txt" --title "Realism Browser $ver" --notes-file $notes }
else { gh release create $tag $setup "$out\SHA256SUMS.txt" --title "Realism Browser $ver" --generate-notes }
Write-Output "Released $tag  sha256=$hash"
