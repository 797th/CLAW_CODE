<#
.SYNOPSIS
Claw Code installer — Windows backend.

.DESCRIPTION
Invoked by install/install.py. Also runnable standalone:

    powershell -ExecutionPolicy Bypass -File install\backends\windows.ps1 -Release
    powershell -ExecutionPolicy Bypass -File install\backends\windows.ps1 -Debug

Builds both `clawcli.exe` and `cliclaw.exe` from rust/, copies them into a
user-level bin directory, adds that directory to the user PATH (idempotently),
and runs a smoke test. The build backend behavior differs from macOS/Linux
(Visual Studio build tools instead of clang/gcc) but the contract is identical.
#>

param(
    [switch]$Release,
    [switch]$Debug,
    [switch]$NoVerify,
    [switch]$NoPathUpdate,
    [string]$InstallDir
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$Profile = if ($Release) { "release" } elseif ($Debug) { "debug" } else { "debug" }

function Resolve-NormalizedPath {
    param([Parameter(Mandatory = $true)][string]$PathValue)
    return [System.IO.Path]::GetFullPath($PathValue).TrimEnd('\')
}

function Test-PathEntryPresent {
    param(
        [Parameter(Mandatory = $true)][string]$PathList,
        [Parameter(Mandatory = $true)][string]$Candidate
    )
    if ([string]::IsNullOrWhiteSpace($PathList)) { return $false }
    $normalizedCandidate = Resolve-NormalizedPath -PathValue $Candidate
    foreach ($entry in ($PathList -split ';')) {
        if ([string]::IsNullOrWhiteSpace($entry)) { continue }
        if ((Resolve-NormalizedPath -PathValue $entry).Equals(
                $normalizedCandidate, [System.StringComparison]::OrdinalIgnoreCase)) {
            return $true
        }
    }
    return $false
}

function Write-Info  { Write-Host "  -> $args" }
function Write-Ok    { Write-Host "  ok $args" }
function Write-Warn2 { Write-Host "  warn $args" }

# ---------------------------------------------------------------------------
# Resolve paths
# ---------------------------------------------------------------------------

$rustDir = $env:CLAW_RUST_DIR
if ([string]::IsNullOrWhiteSpace($rustDir)) {
    $rustDir = Resolve-NormalizedPath (Join-Path $PSScriptRoot "..\..\rust")
}
$cargoToml = Join-Path $rustDir "Cargo.toml"
if (-not (Test-Path -LiteralPath $cargoToml)) {
    throw "Could not find rust/Cargo.toml (CLAW_RUST_DIR=$rustDir)."
}

if ([string]::IsNullOrWhiteSpace($InstallDir)) {
    if (-not [string]::IsNullOrWhiteSpace($env:CARGO_HOME)) {
        $InstallDir = Join-Path $env:CARGO_HOME "bin"
    } elseif (Test-Path -LiteralPath (Join-Path $HOME ".cargo\bin")) {
        $InstallDir = Join-Path $HOME ".cargo\bin"
    } else {
        $InstallDir = Join-Path $HOME ".local\bin"
    }
}

$installDirFull = Resolve-NormalizedPath -PathValue $InstallDir
$targetDir = Join-Path $rustDir ("target\" + $Profile)

Write-Host "Rust dir:    $rustDir"
Write-Host "Profile:     $Profile"
Write-Host "Install dir: $installDirFull"

# ---------------------------------------------------------------------------
# Build
# ---------------------------------------------------------------------------

$cargo = Get-Command cargo -ErrorAction SilentlyContinue
if (-not $cargo) {
    throw "cargo was not found on PATH. Install Rust from https://win.rustup.rs/x86_64 first (includes the MSVC build tools)."
}

$buildArgs = @("build", "--package", "rusty-claude-cli", "--bins")
if ($Profile -eq "release") { $buildArgs += "--release" }

Write-Host ""
Write-Host "Building clawcli + cliclaw..."
Push-Location $rustDir
try {
    & $cargo.Source @buildArgs
} finally {
    Pop-Location
}

# Build both binary names from the single main.rs.
foreach ($binName in @("clawcli", "cliclaw")) {
    $src = Join-Path $targetDir "$binName.exe"
    if (-not (Test-Path -LiteralPath $src)) {
        throw "Expected built binary at '$src'."
    }
    Write-Ok "built $src"
}

# ---------------------------------------------------------------------------
# Install (copy both binaries)
# ---------------------------------------------------------------------------

New-Item -ItemType Directory -Path $installDirFull -Force | Out-Null
foreach ($binName in @("clawcli", "cliclaw")) {
    $src = Join-Path $targetDir "$binName.exe"
    $dst = Join-Path $installDirFull "$binName.exe"
    Copy-Item -LiteralPath $src -Destination $dst -Force
    Write-Ok "installed $dst"
}

# ---------------------------------------------------------------------------
# PATH update (idempotent)
# ---------------------------------------------------------------------------

$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
$pathWasUpdated = $false

if (-not $NoPathUpdate) {
    if (-not (Test-PathEntryPresent -PathList $userPath -Candidate $installDirFull)) {
        $pathEntries = @()
        if (-not [string]::IsNullOrWhiteSpace($userPath)) {
            $pathEntries = $userPath -split ';' | Where-Object { -not [string]::IsNullOrWhiteSpace($_) }
        }
        $newUserPath = @($pathEntries + $installDirFull) -join ';'
        [Environment]::SetEnvironmentVariable("Path", $newUserPath, "User")

        if (-not (Test-PathEntryPresent -PathList $env:Path -Candidate $installDirFull)) {
            $env:Path = if ([string]::IsNullOrWhiteSpace($env:Path)) {
                $installDirFull
            } else {
                $env:Path.TrimEnd(';') + ";" + $installDirFull
            }
        }
        $pathWasUpdated = $true
    }
}

# ---------------------------------------------------------------------------
# Verify
# ---------------------------------------------------------------------------

if ($NoVerify) {
    Write-Warn2 "verification skipped (-NoVerify)"
} else {
    Write-Info "running: clawcli --version"
    $clawcliExe = Join-Path $installDirFull "clawcli.exe"
    $versionOut = & $clawcliExe --version 2>&1
    if ($LASTEXITCODE -eq 0) {
        Write-Ok "clawcli --version -> $versionOut"
    } else {
        Write-Host "clawcli --version failed:" -ForegroundColor Red
        Write-Host $versionOut
        exit 1
    }
}

# ---------------------------------------------------------------------------
# Next steps
# ---------------------------------------------------------------------------

Write-Host ""
Write-Host "Claw Code is built and installed." -ForegroundColor Green
Write-Host ""
Write-Host "  Binaries:  $installDirFull\clawcli.exe  and  $installDirFull\cliclaw.exe"
Write-Host "  Profile:   $Profile"
Write-Host ""
Write-Host "Launch from any folder with:"
Write-Host "  clawcli prompt `"summarize this repository`""
Write-Host "  cliclaw prompt `"review this repository`"   (same binary, relaxes the CWD guard)"
Write-Host ""
Write-Host "Behavior:"
Write-Host "  - clawcli:  the standard CLI (safe working-directory defaults)."
Write-Host "  - cliclaw:  keeps the folder you launched from as the workspace and"
Write-Host "              allows broad working directories such as your home folder."

if ($pathWasUpdated) {
    Write-Host ""
    Write-Host "PATH was updated for your user account and the current PowerShell session."
    Write-Host "Open a new terminal if another shell still cannot find clawcli/cliclaw."
} elseif ($NoPathUpdate) {
    Write-Host ""
    Write-Host "PATH was not modified (-NoPathUpdate)."
} else {
    Write-Host ""
    Write-Host "Install directory was already present on PATH."
}
