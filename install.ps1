# Engram installer for native Windows — fetches engram-alpha.exe, then hands repo
# wiring to the binary itself:
#
#   powershell -ExecutionPolicy Bypass -c "irm https://raw.githubusercontent.com/techtheist/engram/main/install.ps1 | iex"
#
# Run it from your project's root. Downloads the Windows binary from GitHub
# Releases (checksum-verified) into %LOCALAPPDATA%\Engram\bin, adds it to
# your user PATH, and runs `engram-alpha setup` (auto-detects installed AI
# assistants; wiring assets are embedded in the binary).
#
# With parameters (download once, then run):
#   .\install.ps1 -Cli codex,gemini -Skill normal
#   .\install.ps1 -BinOnly
# Environment: ENGRAM_VERSION pins a release tag; ENGRAM_BIN_DIR overrides
# the install directory.
#
# Note: if your AI assistants run inside WSL, install there with install.sh
# instead — the daemon and the agents must share one filesystem.

param(
    [string]$Cli,
    [ValidateSet("relaxed", "normal", "aggressive")]
    [string]$Skill = "relaxed",
    [switch]$BinOnly,
    [string]$Version = $env:ENGRAM_VERSION
)

$ErrorActionPreference = "Stop"
$repo = "techtheist/engram"

function Say([string]$msg) { Write-Host "==> $msg" }

if (-not $Version) {
    $Version = (Invoke-RestMethod "https://api.github.com/repos/$repo/releases/latest").tag_name
    if (-not $Version) { throw "could not resolve the latest release tag" }
}

$asset = "engram-alpha-$Version-x86_64-pc-windows-msvc.exe"
$url = "https://github.com/$repo/releases/download/$Version/$asset"
$binDir = if ($env:ENGRAM_BIN_DIR) { $env:ENGRAM_BIN_DIR } else { Join-Path $env:LOCALAPPDATA "Engram\bin" }
$tmp = Join-Path $env:TEMP "engram-install-$PID"
New-Item -ItemType Directory -Force -Path $binDir, $tmp | Out-Null

try {
    Say "downloading $asset ($Version)"
    Invoke-WebRequest $url -OutFile (Join-Path $tmp $asset)
    Invoke-WebRequest "$url.sha256" -OutFile (Join-Path $tmp "$asset.sha256")

    Say "verifying checksum"
    $expected = ((Get-Content (Join-Path $tmp "$asset.sha256") -Raw) -split '\s+')[0].ToLower()
    $actual = (Get-FileHash (Join-Path $tmp $asset) -Algorithm SHA256).Hash.ToLower()
    if ($actual -ne $expected) { throw "checksum mismatch - refusing to install" }

    $exe = Join-Path $binDir "engram-alpha.exe"
    Say "installing engram-alpha.exe to $binDir"
    Move-Item -Force (Join-Path $tmp $asset) $exe
}
finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}

$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($userPath -notlike "*$binDir*") {
    [Environment]::SetEnvironmentVariable("Path", "$userPath;$binDir", "User")
    Say "added $binDir to your user PATH (new terminals pick it up)"
}

# v0.3.0 -> v0.4.0 cleanup: drop a stale pre-rename binary, but only if it is
# actually OUR engram ("engram" is a contested name). Setup re-points wiring.
$old = Join-Path $binDir "engram.exe"
if (Test-Path $old) {
    $help = (& $old --help 2>$null) | Out-String
    if ($help -match "Durable graph memory") {
        Remove-Item -Force $old
        Say "removed the pre-rename engram.exe (the product binary is engram-alpha since v0.4.0)"
    }
}

if ($BinOnly) { Say "done (binary only)"; exit 0 }

$setupArgs = @("setup", "--skill", $Skill)
if ($Cli) { $setupArgs += @("--cli", $Cli) }
& $exe @setupArgs
if ($LASTEXITCODE -ne 0) {
    Say "no assistants detected - wire one explicitly: engram-alpha setup --cli claude"
}

Write-Host ""
Write-Host "Next steps:"
Write-Host "  1. start the daemon in this repo:   engram-alpha serve"
Write-Host "     (first run downloads the local embedding model, ~30 MB)"
Write-Host "  2. open the pane:                   http://127.0.0.1:8787"
Write-Host "  3. restart your assistant's session. Later: engram-alpha update"
