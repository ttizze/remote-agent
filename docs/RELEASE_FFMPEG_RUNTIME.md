# Release FFmpeg runtime

Preview recording uses the `libvpx-vp9` encoder. Every Host and native desktop
artifact therefore carries an FFmpeg executable beside the product executable:

```text
host-daemon
ffmpeg                 # ffmpeg.exe on Windows; a loader wrapper on dynamic Linux
ffmpeg-bin             # dynamic Linux binary, omitted for a static source
lib/                   # copied non-system runtime libraries, when required
FFMPEG-RUNTIME.txt
FFMPEG-LICENSE-*       # deterministic license/copyright inventory
```

The Host and desktop launchers find this sibling executable before falling back
to `PATH`. `AGENT_FFMPEG_EXECUTABLE` and `AGENT_FFMPEG_LICENSE_DIR` remain
available for development and isolated tests. A shipped artifact does not
require Nix, Homebrew, or an externally installed FFmpeg.

Dynamic Linux builds keep the kernel interpreter out of the archive's ELF
`PT_INTERP`: `ffmpeg` invokes the copied loader with an explicit relative
`--library-path` and `ffmpeg-bin` target. This keeps the archive portable
across hosts with a different `/lib64` layout. Shared-library basenames that
would overwrite different files fail packaging instead of silently changing
the closure.

Linux and macOS release jobs resolve FFmpeg from the lockfile-pinned Nixpkgs
input. `scripts/stage-ffmpeg-runtime.sh` checks that the binary advertises
`libvpx-vp9`, copies its non-system shared-library closure, and rewrites the
Linux or macOS loader paths to the sibling `lib` directory. The macOS app puts
the same layout in `Contents/MacOS`, so both `Bex` and its Host children see the
same runtime. Nested macOS runtime files are signed before the app bundle is
signed.

The Windows runner uses the static FFmpeg 9.0.2 essentials archive from
[Gyan's published builds](https://www.gyan.dev/ffmpeg/builds/), pinned by the
SHA-256 value in `scripts/fetch-windows-ffmpeg.ps1` and the upstream
[published checksum](https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip.sha256).
The published build page lists libvpx in the essentials variant and describes
the release binaries as static GPLv3 builds, so the archive has no DLL
prerequisite. The staging script still copies DLLs if a future pinned source
supplies a shared build.

The staging helper collects sorted license and copying files from the FFmpeg
package root, every non-system Nix dependency root, and the checked-in
`third_party/ffmpeg` notices needed when Nix splits license data out of its
binary output. It fails when no such file is available and records the copied
inventory in `FFMPEG-RUNTIME.txt`, so an artifact cannot silently omit the
runtime's license material. The manifest also records the source executable
digest and loader mode.

Packaging fails if FFmpeg is missing, cannot report its version, lacks
`libvpx-vp9`, has a conflicting dependency basename, or has no license file.
`scripts/ffmpeg-runtime-test.sh` exercises the Linux loader wrapper,
non-system license inventory, and fail-closed license check with fixture tools.
`scripts/release-metadata-test.sh` also checks that Host and desktop Windows
archive layouts contain the runtime manifest, sibling executable, and copied
license inventory.
