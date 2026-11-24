#requires -Version 5.1
[CmdletBinding()]
param(
    [string]$TargetDirectory = (Join-Path $PSScriptRoot 'target'),
    [switch]$Offline
)

$ErrorActionPreference = 'Stop'
if ($env:OS -ne 'Windows_NT') {
    throw 'Build the Windows installer on a Windows host with the x64 MSVC toolchain.'
}

$repository = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).ProviderPath
$TargetDirectory = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($TargetDirectory)
$cargoDirectory = $env:CARGO_HOME
if (!$cargoDirectory) {
    $cargoDirectory = Join-Path $env:USERPROFILE '.cargo'
}
$cargoDirectory = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($cargoDirectory)

$rustFlags = @('-C', 'target-feature=+crt-static')
# Generated bindings can contain source paths under the Cargo target directory.
$paths = [ordered]@{
    '/render-bridge' = $repository
    '/cargo' = $cargoDirectory
    '/target' = $TargetDirectory
}
foreach ($mapping in $paths.GetEnumerator()) {
    foreach ($prefix in @($mapping.Value, $mapping.Value.Replace('\', '/')) | Select-Object -Unique) {
        $rustFlags += @('--remap-path-prefix', "$prefix=$($mapping.Key)")
    }
}

$target = 'x86_64-pc-windows-msvc'
$manifest = Join-Path $PSScriptRoot 'Cargo.toml'
$manifestVersion = Select-String -LiteralPath $manifest -Pattern '^version\s*=\s*"([0-9]+\.[0-9]+\.[0-9]+)"\s*$' |
    Select-Object -First 1
if (!$manifestVersion) {
    throw 'Cargo.toml must contain a three-part release version.'
}
$releaseVersion = $manifestVersion.Matches[0].Groups[1].Value
$buildArguments = @('build', '--release', '--locked', '--target', $target,
    '--manifest-path', $manifest, '--target-dir', $TargetDirectory)
if ($Offline) {
    $buildArguments += '--offline'
}

$previousFlags = $env:CARGO_ENCODED_RUSTFLAGS
try {
    $env:CARGO_ENCODED_RUSTFLAGS = $rustFlags -join [char]31
    & cargo.exe @buildArguments
    if ($LASTEXITCODE -ne 0) {
        throw "Windows installer build failed with exit code $LASTEXITCODE."
    }
}
finally {
    $env:CARGO_ENCODED_RUSTFLAGS = $previousFlags
}

$installer = Get-Item -LiteralPath (Join-Path $TargetDirectory "$target/release/render-bridge-setup.exe")
$metadata = $installer.VersionInfo
if ($metadata.ProductName -cne 'Render Bridge' -or
    $metadata.CompanyName -cne 'BlackRune LLC' -or
    $metadata.FileDescription -cne 'Render Bridge Setup' -or
    $metadata.OriginalFilename -cne 'RenderBridgeSetup.exe' -or
    $metadata.FileVersion -cne $releaseVersion -or
    $metadata.ProductVersion -cne $releaseVersion -or
    ($metadata.FileMajorPart, $metadata.FileMinorPart, $metadata.FileBuildPart, $metadata.FilePrivatePart -join '.') -ne "$releaseVersion.0" -or
    ($metadata.ProductMajorPart, $metadata.ProductMinorPart, $metadata.ProductBuildPart, $metadata.ProductPrivatePart -join '.') -ne "$releaseVersion.0") {
    throw "The Windows installer has missing or stale product metadata. Update the version in installer/assets/render-bridge.rc and regenerate render-bridge.res for $releaseVersion."
}
$installer
