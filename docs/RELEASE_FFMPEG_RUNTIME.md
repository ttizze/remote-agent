# Release FFmpeg runtime

Preview recording uses the `libvpx-vp9` encoder. The shared Host/Desktop
runtime also supports H.264 and MJPEG decode, MJPEG encode, pixel conversion,
pipe I/O, and WebM/image muxing for the native preview and device paths. Every
Host and native desktop artifact therefore carries an FFmpeg executable beside
the product executable:

```text
host-daemon
ffmpeg                 # ffmpeg.exe on Windows; a loader wrapper on dynamic Linux
ffmpeg-bin             # dynamic Linux binary, omitted for a static source
lib/                   # copied non-system runtime libraries, when required
FFMPEG-RUNTIME.txt
FFMPEG-LICENSE-*       # deterministic license/copyright inventory
```

The Host recording consumer resolves this sibling executable before falling
back to `PATH`; the pure lookup is tested next to the consumer. The desktop
Preview UI decodes the Host's JPEG frames directly and therefore does not need
to launch FFmpeg itself. When the Desktop launcher starts an installed Host,
it passes that Host's sibling runtime as an explicit development-safe override;
the Host performs the same sibling check itself. The same runtime is present
for native desktop/device transcode consumers. `AGENT_FFMPEG_EXECUTABLE` and
`AGENT_FFMPEG_LICENSE_DIR` remain available for development and isolated tests.
A shipped artifact does not require Nix, Homebrew, or an externally installed
FFmpeg.

Dynamic Linux builds keep the kernel interpreter out of the archive's ELF
`PT_INTERP`: `ffmpeg` invokes the copied loader with an explicit relative
`--library-path` and `ffmpeg-bin` target. This keeps the archive portable
across hosts with a different `/lib64` layout. Shared-library basenames that
would overwrite different files fail packaging instead of silently changing
the closure.

Linux and macOS release jobs resolve the small, lockfile-pinned FFmpeg output
declared in `flake.nix`. The staging helper queries the actual binary and
requires `h264`/`mjpeg` decoders, `mjpeg`/`libvpx-vp9` encoders,
`h264`/`image2pipe`/`matroska,webm` demuxers,
`image2pipe`/`mpjpeg`/`matroska,webm`/`null` muxers, the `pipe` protocol, and the
`scale` filter. `buildSwscale=true` is part of the pinned configuration because
H.264/MJPEG frames can require pixel-format and size conversion. The component
inventory is reviewed against the exact Nixpkgs revision in
`flake.lock`, including the Linux glibc loader and GCC runtime when the
platform's dynamic closure contains them.
staging helper checks that the binary advertises every required capability, copies its
non-system shared-library closure, and rewrites the Linux or macOS loader paths
to the sibling `lib` directory. The macOS app puts the same layout in
`Contents/MacOS`, so both `Bex` and its Host children see the same runtime.
Nested macOS runtime files are signed before the app bundle is signed.

The Windows runner uses the static FFmpeg 9.0.2 essentials archive from
[Gyan's published builds](https://www.gyan.dev/ffmpeg/builds/), pinned by the
SHA-256 value in `scripts/fetch-windows-ffmpeg.ps1` and the upstream
[published checksum](https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip.sha256).
The published build page lists libvpx in the essentials variant and describes
the release binaries as static GPLv3 builds, so the archive has no DLL
prerequisite. The staging script still copies DLLs if a future pinned source
supplies a shared build.

The staging helper collects sorted license and copying files from a runtime
root when they are present. Nix binary and library outputs commonly omit those
files, so `third_party/ffmpeg/components.tsv` is an exact component-root
inventory: each pinned output name maps to its checked-in notices, SPDX
identifiers, and HTTPS upstream sources. A split inventory is accepted only for
a listed component; an unknown root fails packaging even when it happens to
contain an untracked license file. Windows' pinned Gyan artifact supplies the
`ffmpeg-9.0.2-essentials_build` component identity before staging. The manifest
records every component, license file, SPDX identifier, and source URL in
`FFMPEG-RUNTIME.txt`, along with the source executable digest and loader mode.
There is no generic license-directory fallback that could hide a new runtime
dependency.

Packaging fails if FFmpeg is missing, cannot report its version, lacks
`libvpx-vp9`, has a conflicting dependency basename, has an unresolved
dependency, or has no mapped license file. `scripts/ffmpeg-runtime-test.sh`
exercises the Linux loader wrapper, a binary root with split licenses, the
unknown-component fail-closed path, and the conflict/license checks with
fixture tools.
`scripts/release-metadata-test.sh` also checks that Host and desktop Windows
archive layouts contain the runtime manifest, sibling executable, and copied
license inventory.
