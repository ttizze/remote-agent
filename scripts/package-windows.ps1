[CmdletBinding()]
param(
    [ValidateSet("release")]
    [string]$Profile = "release",
    [ValidateSet("main", "dev")]
    [string]$Channel = "main",
    [switch]$SkipBuild,
    [string]$BinaryDirectory,
    [string]$InnoCompiler
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
    $buildDirectory = Join-Path $metadata.target_directory "release"
    if ($BinaryDirectory) {
        if (-not $SkipBuild) { throw "BinaryDirectory requires SkipBuild." }
        $buildDirectory = (Resolve-Path -LiteralPath $BinaryDirectory).Path
    }
    if (-not $InnoCompiler) {
        foreach ($candidate in @("${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe")) {
            if (Test-Path -LiteralPath $candidate) { $InnoCompiler = $candidate; break }
        }
    }
    if (-not $InnoCompiler) {
        $toolRoot = Join-Path $metadata.target_directory "tool-cache/inno-6.6.1"
        $InnoCompiler = Join-Path $toolRoot "compiler/ISCC.exe"
        if (-not (Test-Path -LiteralPath $InnoCompiler)) {
            New-Item -ItemType Directory -Force -Path $toolRoot | Out-Null
            $toolInstaller = Join-Path $toolRoot "innosetup-6.6.1.exe"
            Invoke-WebRequest -Uri "https://github.com/jrsoftware/issrc/releases/download/is-6_6_1/innosetup-6.6.1.exe" -OutFile $toolInstaller
            $toolHash = (Get-FileHash -LiteralPath $toolInstaller -Algorithm SHA256).Hash.ToLowerInvariant()
            if ($toolHash -ne "d243ce440c02705530699554fb9612b9b2bd7a2a90629cdb7f41e66f5faeb91f") { throw "Inno Setup checksum mismatch." }
            $compilerDirectory = Join-Path $toolRoot "compiler"
            $install = Start-Process -FilePath $toolInstaller -ArgumentList @("/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/CURRENTUSER", ('/DIR="' + $compilerDirectory + '"')) -WindowStyle Hidden -Wait -PassThru
            if ($install.ExitCode -ne 0) { throw "Inno Setup installation failed: $($install.ExitCode)" }
        }
    }
    $revision = & git rev-parse HEAD
    if ($LASTEXITCODE -ne 0) { throw "Cannot determine the source revision." }
    $sourceStatus = & git -c core.longpaths=true status --porcelain --untracked-files=normal
    if ($LASTEXITCODE -ne 0) { throw "Cannot verify the source checkout." }
    if ($sourceStatus) { $revision = "$revision-dirty" }
    if (-not $SkipBuild) {
        $previousRevision = $env:BEX_BUILD_REVISION
        $previousChannel = $env:BEX_BUILD_CHANNEL
        try {
            $env:BEX_BUILD_REVISION = $revision
            $env:BEX_BUILD_CHANNEL = $Channel
            & cargo build --locked --profile $Profile --features agent-core/bindings -p bex-desktop -p host-daemon -p bex-process -p agent-cli
            if ($LASTEXITCODE -ne 0) { throw "Windows build failed." }
        } finally {
            $env:BEX_BUILD_REVISION = $previousRevision
            $env:BEX_BUILD_CHANNEL = $previousChannel
        }
    }
    # Read facts from the executable; directory names and caller labels are not proof.
    $desktopInfo = & (Join-Path $buildDirectory "bex-desktop.exe") --build-info | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0 -or $desktopInfo.profile -ne "release" -or $desktopInfo.debugAssertions) {
        throw "Distribution requires a release executable with debug assertions disabled."
    }
    if ($desktopInfo.revision -ne $revision) { throw "Desktop binary does not match source revision $revision." }
    $expectedAppName = if ($Channel -eq "dev") { "Bex dev" } else { "bex" }
    if ($desktopInfo.appName -ne $expectedAppName) { throw "Desktop binary does not match channel $Channel." }
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
        @{ revision = $revision; profile = $Profile; target = "x86_64-pc-windows-msvc"; appName = $desktopInfo.appName } |
            ConvertTo-Json | Set-Content -LiteralPath (Join-Path $bundle "build-info.json") -Encoding UTF8
        Copy-Item -LiteralPath (Join-Path $bundle "build-info.json") -Destination (Join-Path $bundle "desktop-build-info.json")
        $archive = Join-Path $packageRoot "bex-windows-x86_64-$Profile.zip"
        Compress-Archive -LiteralPath $bundle -DestinationPath $archive -Force
        $hash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
        "$hash  $(Split-Path $archive -Leaf)" | Set-Content -LiteralPath "$archive.sha256" -Encoding ASCII
        Write-Output $archive
        & $InnoCompiler "/DBundleDirectory=$bundle" "/DPackageDirectory=$packageRoot" "/DBuildRevision=$($revision.Substring(0, 8))" scripts/windows-installer.iss
        if ($LASTEXITCODE -ne 0) { throw "Windows EXE installer compilation failed." }
        $installer = Join-Path $packageRoot "bex-windows-x86_64-setup.exe"
        $installerHash = (Get-FileHash -LiteralPath $installer -Algorithm SHA256).Hash.ToLowerInvariant()
        "$installerHash  $(Split-Path $installer -Leaf)" | Set-Content -LiteralPath "$installer.sha256" -Encoding ASCII
        Write-Output $installer
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
