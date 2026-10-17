[CmdletBinding()]
param(
    [ValidateSet("dev", "release")]
    [string]$Profile = "release",
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest
if ($env:OS -ne "Windows_NT") { throw "Windows packages require Windows." }
$repository = Split-Path $PSScriptRoot -Parent
Push-Location $repository
try {
    $compilerInfo = & rustc -vV
    if ($LASTEXITCODE -ne 0 -or $compilerInfo -notcontains "host: x86_64-pc-windows-msvc") {
        throw "This package requires the x86-64 MSVC Rust toolchain."
    }
    if ($env:CARGO_BUILD_TARGET) { throw "Run without CARGO_BUILD_TARGET; this script packages the native x86-64 build." }
    $metadataJson = & cargo metadata --locked --no-deps --format-version 1
    if ($LASTEXITCODE -ne 0) { throw "cargo metadata failed." }
    $metadata = $metadataJson | ConvertFrom-Json
    $directory = if ($Profile -eq "dev") { "debug" } else { "release" }
    $buildDirectory = Join-Path $metadata.target_directory $directory
    $revision = & git rev-parse HEAD
    if ($LASTEXITCODE -ne 0) { throw "Cannot determine the source revision." }
    $sourceStatus = & git status --porcelain --untracked-files=normal
    if ($sourceStatus) { $revision = "$revision-dirty" }
    if (-not $SkipBuild) {
        $previousRevision = $env:BEX_BUILD_REVISION
        try {
            $env:BEX_BUILD_REVISION = $revision
            & cargo build --locked --profile $Profile --features agent-core/bindings -p bex-desktop -p host-daemon -p bex-process -p agent-cli
            if ($LASTEXITCODE -ne 0) { throw "Windows build failed." }
        } finally { $env:BEX_BUILD_REVISION = $previousRevision }
    }
    # A unique staging directory avoids modifying an installed/running Host.
    $packageRoot = Join-Path $metadata.target_directory "windows-packages"
    New-Item -ItemType Directory -Path $packageRoot -Force | Out-Null
    $staging = Join-Path $packageRoot ([guid]::NewGuid().ToString())
    $bundle = Join-Path $staging "bex"
    New-Item -ItemType Directory -Path $bundle -Force | Out-Null
    try {
        foreach ($executable in @("bex-desktop.exe", "host-daemon.exe", "bex-provider-supervisor.exe", "agent-cli.exe")) {
            Copy-Item -LiteralPath (Join-Path $buildDirectory $executable) -Destination $bundle
        }
        # Claude's bridge and its license must travel with the Host.
        & node scripts/install-claude-sdk.mjs $bundle
        if ($LASTEXITCODE -ne 0) { throw "Claude SDK packaging failed." }
        Copy-Item -LiteralPath "docs/WINDOWS.md" -Destination (Join-Path $bundle "README.md")
        @{ revision = $revision; profile = $Profile; target = "x86_64-pc-windows-msvc" } |
            ConvertTo-Json | Set-Content -LiteralPath (Join-Path $bundle "build-info.json") -Encoding UTF8
        $archive = Join-Path $packageRoot "bex-windows-x86_64-$Profile.zip"
        Compress-Archive -LiteralPath $bundle -DestinationPath $archive -Force
        $hash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
        "$hash  $(Split-Path $archive -Leaf)" | Set-Content -LiteralPath "$archive.sha256" -Encoding ASCII
        Write-Output $archive
    } finally {
        # Verify the absolute target is the unique child created above.
        $resolvedStaging = [IO.Path]::GetFullPath($staging)
        $resolvedRoot = [IO.Path]::GetFullPath($packageRoot) + [IO.Path]::DirectorySeparatorChar
        if (-not $resolvedStaging.StartsWith($resolvedRoot, [StringComparison]::OrdinalIgnoreCase)) {
            throw "Refusing to clean staging outside the package directory."
        }
        Remove-Item -LiteralPath $resolvedStaging -Recurse -Force
    }
} finally { Pop-Location }
