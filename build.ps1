param(
    [ValidatePattern('^\d+\.\d+\.\d+$')]
    [string]$Version,

    [switch]$NoIncrement,
    [switch]$NoUpx
)

$ErrorActionPreference = "Stop"

$packageName = "resizerrust"
$cargoToml = Join-Path $PSScriptRoot "Cargo.toml"
$cargoLock = Join-Path $PSScriptRoot "Cargo.lock"

function Get-CargoVersion {
    $content = Get-Content $cargoToml -Raw
    if ($content -notmatch '(?ms)^\[package\]\s+.*?^version\s*=\s*"([^"]+)"') {
        throw "version not found in $cargoToml"
    }
    $matches[1]
}

function Set-CargoLockVersion {
    param([string]$NewVersion)

    if (!(Test-Path $cargoLock)) {
        return
    }

    $lines = Get-Content $cargoLock
    $inPackage = $false
    $isAppPackage = $false
    $updated = $false

    for ($i = 0; $i -lt $lines.Count; $i++) {
        if ($lines[$i] -eq "[[package]]") {
            $inPackage = $true
            $isAppPackage = $false
            continue
        }

        if ($inPackage -and $lines[$i] -eq "name = `"$packageName`"") {
            $isAppPackage = $true
            continue
        }

        if ($isAppPackage -and $lines[$i] -match '^version = ') {
            $lines[$i] = "version = `"$NewVersion`""
            $updated = $true
            break
        }
    }

    if (!$updated) {
        throw "$packageName version not found in $cargoLock"
    }

    Set-Content $cargoLock $lines
}

function Set-CargoVersion {
    param([string]$NewVersion)

    $content = Get-Content $cargoToml -Raw
    $updated = [regex]::Replace(
        $content,
        '(?ms)^(\[package\]\s+.*?^version\s*=\s*)"[^"]+"',
        ('${1}"' + $NewVersion + '"'),
        1
    )

    if ($updated -eq $content -and (Get-CargoVersion) -ne $NewVersion) {
        throw "could not update version in $cargoToml"
    }

    Set-Content $cargoToml $updated -NoNewline
    Set-CargoLockVersion $NewVersion
}

function Run {
    param([string]$Command, [string[]]$Arguments)

    & $Command @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$Command $($Arguments -join ' ') failed"
    }
}

$nativeCmake = "$env:ProgramFiles\CMake\bin\cmake.exe"
if (Test-Path $nativeCmake) {
    $env:CMAKE = $nativeCmake
}

if (![string]::IsNullOrWhiteSpace($Version)) {
    Set-CargoVersion $Version
}

Run "cargo" @("build", "--release", "--locked")

$artifactDir = Join-Path $PSScriptRoot "artifacts"
$targetExe = Join-Path $PSScriptRoot "target\release\$packageName.exe"
$artifactExe = Join-Path $artifactDir "$packageName.exe"
$artifactSha = Join-Path $artifactDir "$packageName.exe.sha256"
$rootExe = Join-Path $PSScriptRoot "$packageName.exe"

if (!(Test-Path $targetExe)) {
    throw "Build did not create $targetExe"
}

New-Item -ItemType Directory -Path $artifactDir -Force | Out-Null
Copy-Item $targetExe $artifactExe -Force

if (!$NoUpx -and (Test-Path (Join-Path $PSScriptRoot "upx.exe"))) {
    Run (Join-Path $PSScriptRoot "upx.exe") @("-3", $artifactExe)
}

Copy-Item $artifactExe $rootExe -Force
$hash = (Get-FileHash $artifactExe -Algorithm SHA256).Hash.ToLowerInvariant()
"$hash  $packageName.exe" | Set-Content $artifactSha

Write-Host "Built $artifactExe ($(Get-CargoVersion))" -ForegroundColor Green
