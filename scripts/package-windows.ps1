[CmdletBinding()]
param(
    [ValidateSet("dev", "release")]
    [string]$Profile = "release",
    [switch]$SkipBuild,
    [string]$BinaryDirectory,
    [string]$WslRuntimeDirectory,
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
    $directory = if ($Profile -eq "dev") { "debug" } else { "release" }
    $buildDirectory = Join-Path $metadata.target_directory $directory
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
    if (-not $WslRuntimeDirectory) {
        $WslRuntimeDirectory = Join-Path $metadata.target_directory "wsl-runtime"
    }
    $wslRevisionPath = Join-Path $WslRuntimeDirectory "revision"
    if (-not (Test-Path -LiteralPath $wslRevisionPath) -or
        (Get-Content -Raw -LiteralPath $wslRevisionPath).Trim() -ne $revision) {
        throw "A Linux WSL runtime built from revision $revision is required. Set WslRuntimeDirectory to its directory."
    }
    if (-not $SkipBuild) {
        $previousRevision = $env:AGENT_BUILD_REVISION
        $previousCompiler = $env:GPUI_FXC_PATH
        try {
            $env:AGENT_BUILD_REVISION = $revision
            # GPUI compiles its shaders with the compiler named here.
            $env:GPUI_FXC_PATH = & (Join-Path $PSScriptRoot "../tools/windows-fxc/prepare.ps1")
            & cargo build --locked --profile $Profile --features agent-core/bindings -p agent-desktop -p host-daemon -p agent-process -p agent-cli
            if ($LASTEXITCODE -ne 0) { throw "Windows build failed." }
        } finally {
            $env:AGENT_BUILD_REVISION = $previousRevision
            $env:GPUI_FXC_PATH = $previousCompiler
        }
    }
    # Check the PE Subsystem field in both profiles before distributing the app.
    $desktopReader = [IO.BinaryReader]::new([IO.File]::OpenRead((Join-Path $buildDirectory "agent-desktop.exe")))
    try {
        $desktopReader.BaseStream.Position = 0x3c
        $peHeaderOffset = $desktopReader.ReadInt32()
        $desktopReader.BaseStream.Position = $peHeaderOffset + 24 + 68
        if ($desktopReader.ReadUInt16() -ne 2) {
            throw "The desktop executable must use the Windows GUI subsystem."
        }
    } finally { $desktopReader.Dispose() }
    # A unique staging directory avoids modifying an installed/running Host.
    $packageRoot = Join-Path $metadata.target_directory "windows-packages"
    New-Item -ItemType Directory -Path $packageRoot -Force | Out-Null
    $staging = Join-Path $packageRoot ([guid]::NewGuid().ToString())
    $bundle = Join-Path $staging "bex"
    New-Item -ItemType Directory -Path $bundle -Force | Out-Null
    try {
        foreach ($executable in @("agent-desktop.exe", "host-daemon.exe", "agent-provider-supervisor.exe", "agent-cli.exe")) {
            Copy-Item -LiteralPath (Join-Path $buildDirectory $executable) -Destination $bundle
        }
        Copy-Item -LiteralPath $WslRuntimeDirectory -Destination (Join-Path $bundle "wsl-runtime") -Recurse
        # Claude's bridge and its license must travel with the Host.
        & node scripts/install-claude-sdk.ts $bundle
        if ($LASTEXITCODE -ne 0) { throw "Claude SDK packaging failed." }
        Copy-Item -LiteralPath "docs/WINDOWS.md" -Destination (Join-Path $bundle "README.md")
        @{ revision = $revision; profile = $Profile; target = "x86_64-pc-windows-msvc" } |
            ConvertTo-Json | Set-Content -LiteralPath (Join-Path $bundle "build-info.json") -Encoding UTF8
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
