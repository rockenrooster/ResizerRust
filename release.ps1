param(
    [ValidatePattern('^\d+\.\d+\.\d+$')]
    [string]$Version,

    [string]$Message
)

$ErrorActionPreference = "Stop"

$packageName = "resizerrust"
$cargoToml = Join-Path $PSScriptRoot "Cargo.toml"

function RunGit {
    & git @args
    if ($LASTEXITCODE -ne 0) {
        throw "git $($args -join ' ') failed"
    }
}

function Get-VersionFromContent {
    param([string]$Content, [string]$Source)

    if ($Content -notmatch '(?ms)^\[package\]\s+.*?^version\s*=\s*"(\d+\.\d+\.\d+)"') {
        throw "package version not found in $Source"
    }

    [version]$matches[1]
}

function Get-CurrentVersion {
    Get-VersionFromContent (Get-Content $cargoToml -Raw) $cargoToml
}

function Get-DefaultReleaseVersion {
    $current = Get-CurrentVersion
    git rev-parse --verify HEAD 2>$null | Out-Null
    if ($LASTEXITCODE -ne 0) {
        return $current.ToString(3)
    }

    $headContent = git show HEAD:Cargo.toml 2>$null
    if ($LASTEXITCODE -eq 0 -and $headContent) {
        $headVersion = Get-VersionFromContent ($headContent -join "`n") "HEAD:Cargo.toml"
        if ($current -gt $headVersion) {
            return $current.ToString(3)
        }
    }

    "$($current.Major).$($current.Minor).$($current.Build + 1)"
}

function Get-GeneratedCommitBody {
    $lines = git diff --cached --name-status
    if ($LASTEXITCODE -ne 0) {
        throw "Could not inspect staged changes."
    }

    if (!$lines) {
        return "Automated release."
    }

    $items = foreach ($line in $lines) {
        $parts = $line -split "`t"
        $status = $parts[0]
        $path = $parts[-1]
        $verb = switch -Regex ($status) {
            '^A' { "Added"; break }
            '^D' { "Removed"; break }
            '^R' { "Renamed"; break }
            default { "Updated" }
        }
        "- $verb $path"
    }

    "Changes:`n" + ($items -join "`n")
}

if ([string]::IsNullOrWhiteSpace($Version)) {
    $Version = Get-DefaultReleaseVersion
}

$tag = "v$Version"
Write-Host "Releasing $tag" -ForegroundColor Cyan

$branch = (git branch --show-current).Trim()
if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($branch)) {
    throw "Could not determine the current branch."
}

$origin = git remote get-url origin 2>$null
if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($origin)) {
    throw "No git remote named origin is configured."
}
$origin = $origin.Trim()

$null = git rev-parse -q --verify "refs/tags/$tag" 2>$null
if ($LASTEXITCODE -eq 0) {
    throw "Local tag $tag already exists."
}

$remoteTag = git ls-remote --tags origin "refs/tags/$tag"
if ($LASTEXITCODE -ne 0) {
    throw "Could not check remote tags."
}
if (![string]::IsNullOrWhiteSpace($remoteTag)) {
    throw "Remote tag $tag already exists."
}

& (Join-Path $PSScriptRoot "build.ps1") -Version $Version -NoIncrement

$artifactExe = Join-Path $PSScriptRoot "artifacts\$packageName.exe"
$artifactSha = Join-Path $PSScriptRoot "artifacts\$packageName.exe.sha256"
if (!(Test-Path $artifactExe) -or !(Test-Path $artifactSha)) {
    throw "Build did not create the release artifacts."
}

$fileVersion = (Get-Item $artifactExe).VersionInfo.FileVersion
if ($fileVersion -and $fileVersion -match '^(\d+\.\d+\.\d+)' -and $matches[1] -ne $Version) {
    throw "FileVersion $fileVersion does not match $Version."
}
Write-Host "Verified local artifact FileVersion $fileVersion for $tag" -ForegroundColor Cyan

RunGit add -A

git diff --cached --quiet
if ($LASTEXITCODE -eq 1) {
    if ([string]::IsNullOrWhiteSpace($Message)) {
        RunGit commit -m "Release $tag" -m (Get-GeneratedCommitBody)
    }
    else {
        RunGit commit -m $Message
    }
}
elseif ($LASTEXITCODE -ne 0) {
    throw "Could not inspect staged changes."
}

RunGit push -u origin $branch
RunGit tag $tag
RunGit push origin $tag

Write-Host "Pushed $branch and $tag. GitHub Actions will create the release." -ForegroundColor Green
