$ErrorActionPreference = 'Stop'

# The archive is a static x86_64 build.  Its published checksum is pinned so
# a nightly runner cannot silently select a moving "latest" encoder binary.
$version = '9.0.2'
$url = "https://www.gyan.dev/ffmpeg/builds/packages/ffmpeg-$version-essentials_build.zip"
$sha256 = '60f467265b1e312373dbcd92200c2618a74850f98d3d078e94296bb3fa2047ba'
$root = Join-Path $env:RUNNER_TEMP "ffmpeg-$version"
$archive = Join-Path $env:RUNNER_TEMP "ffmpeg-$version.zip"

New-Item -ItemType Directory -Force -Path $root | Out-Null
Invoke-WebRequest -Uri $url -OutFile $archive
$actual = (Get-FileHash -Algorithm SHA256 -Path $archive).Hash.ToLowerInvariant()
if ($actual -ne $sha256) {
    throw "FFmpeg archive checksum mismatch: expected $sha256, got $actual"
}
Expand-Archive -LiteralPath $archive -DestinationPath $root -Force
$executable = Get-ChildItem -LiteralPath $root -Filter ffmpeg.exe -File -Recurse | Select-Object -First 1
if ($null -eq $executable) {
    throw 'The pinned FFmpeg archive did not contain ffmpeg.exe'
}
$requiredCapabilities = @{
    decoders = @('h264', 'mjpeg')
    encoders = @('mjpeg', 'libvpx-vp9')
    demuxers = @('h264', 'image2pipe', 'matroska,webm')
    muxers = @('image2pipe', 'mpjpeg', 'matroska,webm', 'null')
    protocols = @('pipe')
    filters = @('scale')
}
foreach ($table in $requiredCapabilities.Keys) {
    $output = & $executable.FullName -hide_banner -loglevel error "-$table" 2>&1 | Out-String
    if ($LASTEXITCODE -ne 0) {
        throw "The pinned FFmpeg archive could not report its $table table"
    }
    foreach ($name in $requiredCapabilities[$table]) {
        $escaped = [regex]::Escape($name)
        $pattern = if ($table -eq 'protocols') {
            "(?m)^\s*$escaped(\s|$)"
        } else {
            "(?m)^\s+\S+\s+$escaped(\s|$)"
        }
        if ($output -notmatch $pattern) {
            throw "The pinned FFmpeg archive lacks required $table entry: $name"
        }
    }
}

# Point at the extracted package root so the staging helper can copy both DLLs
# and the upstream license/readme files beside the shipped executable.
"AGENT_FFMPEG_EXECUTABLE=$($executable.FullName)" | Out-File -FilePath $env:GITHUB_ENV -Encoding utf8 -Append
"AGENT_FFMPEG_RUNTIME_DIR=$($executable.Directory.Parent.FullName)" | Out-File -FilePath $env:GITHUB_ENV -Encoding utf8 -Append
"AGENT_FFMPEG_COMPONENT=ffmpeg-$version-essentials_build" | Out-File -FilePath $env:GITHUB_ENV -Encoding utf8 -Append
"FFMPEG_WINDOWS_VERSION=$version" | Out-File -FilePath $env:GITHUB_ENV -Encoding utf8 -Append
