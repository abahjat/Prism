# Update-Version.ps1
# Automates updating the version in Cargo.toml and .csproj files
param (
    [Parameter(Mandatory = $true)]
    [string]$Version
)

$ErrorActionPreference = "Stop"
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$RepoRoot = Split-Path -Parent $ScriptDir
$CargoToml = Join-Path $RepoRoot "Cargo.toml"
$Csproj = Join-Path $RepoRoot "bindings\dotnet\Prism.Native\Prism.Native.csproj"

# 1. Validate SemVer (Simple Regex)
if ($Version -notmatch '^\d+\.\d+\.\d+(-\w+)?$') {
    Write-Error "Invalid version format '$Version'. Expected SemVer (e.g., 0.1.0 or 0.1.0-beta)"
    exit 1
}

Write-Host "Updating version to $Version..." -ForegroundColor Cyan

# 2. Update Cargo.toml (Workspace version)
$CargoContent = Get-Content $CargoToml -Raw
# Regex to find [workspace.package] followed by version = "..."
# We use a pattern that looks for the specific section and then the version key
if ($CargoContent -match '(\[workspace\.package\][\s\S]*?version = ")([^"]+)(")') {
    # Use named groups to avoid ambiguity
    $NewCargoContent = $CargoContent -replace '(?<prefix>\[workspace\.package\][\s\S]*?version = ")(?<ver>[^"]+)(?<suffix>")', ('${prefix}' + $Version + '${suffix}')
    Set-Content -Path $CargoToml -Value $NewCargoContent -Encoding UTF8
    Write-Host "Updated Cargo.toml" -ForegroundColor Green
}
else {
    Write-Warning "Could not find [workspace.package] version in Cargo.toml"
}

# 3. Update Prism.Native.csproj
$CsprojContent = Get-Content $Csproj -Raw
# Simple replacement for unique tag
if ($CsprojContent -match '<Version>[^<]+</Version>') {
    $NewCsprojContent = $CsprojContent -replace '<Version>[^<]+</Version>', ("<Version>$Version</Version>")
    Set-Content -Path $Csproj -Value $NewCsprojContent -Encoding UTF8
    Write-Host "Updated Prism.Native.csproj" -ForegroundColor Green
}
else {
    Write-Warning "Could not find <Version> tag in Prism.Native.csproj"
}


# 4. Update Cargo.lock (by running cargo check)
Write-Host "Updating Cargo.lock..." -ForegroundColor Cyan
Push-Location $RepoRoot
cargo check --quiet
Pop-Location

Write-Host "Version updated successfully to $Version" -ForegroundColor Green
Write-Warning "Don't forget to update CHANGELOG.md!"
