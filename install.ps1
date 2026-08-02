# Papered one-liner installer for Windows.
#
# Usage (PowerShell):
#   irm https://raw.githubusercontent.com/WuChenh/papered/main/install.ps1 | iex
#   $env:PAPERED_VERSION="v0.2.1"; irm https://raw.githubusercontent.com/WuChenh/papered/main/install.ps1 | iex
#
# Installs `papered.exe` and `papered-daemon.exe` from the latest GitHub Release
# into %LOCALAPPDATA%\papered (user-scope) and adds it to the user PATH.

$ErrorActionPreference = "Stop"

$Repo = if ($env:PAPERED_REPO) { $env:PAPERED_REPO } else { "WuChenh/papered" }
$Version = $env:PAPERED_VERSION
$InstallDir = Join-Path $env:LOCALAPPDATA "papered"

function Write-Step([string]$msg) { Write-Host "[papered] $msg" -ForegroundColor Cyan }
function Write-Err([string]$msg)   { Write-Host "[papered] $msg" -ForegroundColor Red; exit 1 }

# --- detect architecture ---
$Arch = switch ($env:PROCESSOR_ARCHITECTURE) {
  "AMD64" { "x86_64" }
  "ARM64" { "aarch64" }
  default { Write-Err "unsupported architecture: $env:PROCESSOR_ARCHITECTURE" }
}
$Target = "${Arch}-pc-windows-msvc"
Write-Step "detected target: $Target"

# --- resolve version (latest release or pinned) ---
if (-not $Version) {
  Write-Step "resolving latest release..."
  try {
    $release = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/latest" `
      -Headers @{ "User-Agent" = "papered-installer" }
  } catch {
    Write-Err "failed to query GitHub API for $Repo releases: $_"
  }
  $Version = $release.tag_name
}
if (-not $Version) { Write-Err "no release tag found" }
if ($Version -notlike "v*") { $Version = "v$Version" }
Write-Step "installing release: $Version"

# --- fetch SHA256SUMS.txt ---
$BaseUrl = "https://github.com/$Repo/releases/download/$Version"
$Archive = "papered-${Version}-${Target}.zip"
$SumsUrl = "$BaseUrl/SHA256SUMS.txt"
Write-Step "fetching SHA256SUMS.txt..."
try {
  $sums = Invoke-WebRequest -Uri $SumsUrl -UseBasicParsing | Select-Object -ExpandProperty Content
} catch {
  Write-Err "could not download $SumsUrl : $_"
}
$expected = ($sums -split "`n" | ForEach-Object {
  $parts = $_ -split "\s+"
  if ($parts.Count -ge 2 -and $parts[1] -eq $Archive) { $parts[0] }
} | Where-Object { $_ }) | Select-Object -First 1
if (-not $expected) { Write-Err "no checksum entry for $Archive in SHA256SUMS.txt" }

# --- download archive ---
$tmpDir = Join-Path $env:TEMP ("papered-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $tmpDir | Out-Null
try {
  $archivePath = Join-Path $tmpDir $Archive
  Write-Step "downloading $Archive..."
  $ProgressPreference = "SilentlyContinue"
  try {
    Invoke-WebRequest -Uri "$BaseUrl/$Archive" -OutFile $archivePath -UseBasicParsing
  } catch {
    Write-Err "download failed: $BaseUrl/$Archive (check that $Target exists in this release): $_"
  }

  # --- verify checksum ---
  Write-Step "verifying checksum..."
  $hash = (Get-FileHash -Algorithm SHA256 -Path $archivePath).Hash.ToLower()
  if ($hash -ne $expected.ToLower()) {
    Write-Err "checksum mismatch for $Archive`n  expected: $expected`n  got:      $hash"
  }
  Write-Step "checksum OK"

  # --- extract ---
  Write-Step "extracting..."
  Expand-Archive -Path $archivePath -DestinationPath $tmpDir -Force
  $extracted = Join-Path $tmpDir "papered-${Version}-${Target}"
  if (-not (Test-Path $extracted)) {
    Write-Err "archive did not contain expected directory: papered-${Version}-${Target}"
  }

  # --- install ---
  Write-Step "installing to $InstallDir..."
  if (-not (Test-Path $InstallDir)) { New-Item -ItemType Directory -Path $InstallDir | Out-Null }
  foreach ($bin in @("papered.exe", "papered-daemon.exe")) {
    $src = Join-Path $extracted $bin
    if (-not (Test-Path $src)) { Write-Err "binary missing from archive: $bin" }
    Copy-Item -Force -Path $src -Destination (Join-Path $InstallDir $bin)
  }

  # --- ensure $InstallDir is on the user PATH ---
  $userPath = [Environment]::GetEnvironmentVariable("PATH", "User")
  if (-not ($userPath -split ";" | Where-Object { $_ -eq $InstallDir })) {
    Write-Step "adding $InstallDir to user PATH..."
    $newPath = ($userPath -split ";" | Where-Object { $_ } | ForEach-Object { $_ }) + $InstallDir
    [Environment]::SetEnvironmentVariable("PATH", ($newPath -join ";"), "User")
    Write-Step "NOTE: restart your terminal for the PATH change to take effect."
  }
} finally {
  Remove-Item -Recurse -Force $tmpDir -ErrorAction SilentlyContinue
}

Write-Step "installed papered and papered-daemon to $InstallDir"
Write-Step "run 'papered ui' to start the daemon and open the web UI."
