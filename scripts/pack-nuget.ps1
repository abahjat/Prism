# Pack-Nuget.ps1
# Builds the Rust bindings and packs the NuGet package
param (
    [string]$Configuration = "Release"
)

$ErrorActionPreference = "Stop"

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$RepoRoot = Split-Path -Parent $ScriptDir
$BindingsDir = Join-Path $RepoRoot "crates\prism-bindings"
$DotNetDir = Join-Path $RepoRoot "bindings\dotnet\Prism.Native"
$RuntimesDir = Join-Path $DotNetDir "runtimes"

Write-Host "Building Rust bindings..." -ForegroundColor Cyan
Push-Location $BindingsDir
cargo build --release
if ($LASTEXITCODE -ne 0) { throw "Rust build failed" }
Pop-Location

# Prepare runtimes folder structure
# We are currently only simulating the other platforms or packaging what we have locally (Windows x64)
# In a CI environment, you would gather artifacts from other jobs here.

$WinX64Dir = Join-Path $RuntimesDir "win-x64\native"
if (-not (Test-Path $WinX64Dir)) {
    New-Item -ItemType Directory -Force -Path $WinX64Dir | Out-Null
}

$SourceDll = Join-Path $RepoRoot "target\release\prism_bindings.dll"
if (-not (Test-Path $SourceDll)) {
    throw "Build artifact not found: $SourceDll"
}

Write-Host "Copying native artifacts..." -ForegroundColor Cyan
Copy-Item -Path $SourceDll -Destination (Join-Path $WinX64Dir "prism_bindings.dll") -Force

# Note: For strict NuGet compliance regarding native deps, usually you create a runtimes structure:
# runtimes/win-x64/native/prism_bindings.dll
# runtimes/linux-x64/native/libprism_bindings.so
# runtimes/osx-x64/native/libprism_bindings.dylib
# etc.
# For now, we only populate win-x64 since that's where we are running.
# CAUTION: This package will crash on other platforms unless those files are added!

Write-Host "Packing NuGet package..." -ForegroundColor Cyan
Push-Location $DotNetDir
dotnet pack -c $Configuration
Pop-Location

Write-Host "Done." -ForegroundColor Green
