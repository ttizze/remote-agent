#!/usr/bin/env bash
# Transparent header logo is the source for app icons. Requires macOS Apple tools.
set -euo pipefail
cd "$(dirname "$0")/.."
source=apps/desktop/assets/logo.png
temporary=$(mktemp -d)
trap 'rm -rf "$temporary"' EXIT
# All launcher icons use a white backdrop; the header keeps source alpha.
cat > "$temporary/app-icon.swift" <<'SWIFT'
import Foundation
import CoreGraphics
import ImageIO
let source = CGImageSourceCreateWithURL(URL(fileURLWithPath: CommandLine.arguments[1]) as CFURL, nil)!
let image = CGImageSourceCreateImageAtIndex(source, 0, nil)!
let context = CGContext(data: nil, width: 1024, height: 1024, bitsPerComponent: 8,
    bytesPerRow: 0, space: CGColorSpaceCreateDeviceRGB(),
    bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue)!
let bounds = CGRect(x: 0, y: 0, width: 1024, height: 1024)
context.setFillColor(CGColor(gray: 1, alpha: 1))
context.fill(bounds)
context.draw(image, in: bounds)
let destination = CGImageDestinationCreateWithURL(
    URL(fileURLWithPath: CommandLine.arguments[2]) as CFURL, "public.png" as CFString, 1, nil)!
CGImageDestinationAddImage(destination, context.makeImage()!, nil)
precondition(CGImageDestinationFinalize(destination))
SWIFT
xcrun swiftc "$temporary/app-icon.swift" -o "$temporary/app-icon"
icon=apps/desktop/assets/icon.png
"$temporary/app-icon" "$source" "$icon"
cp "$icon" apps/mobile/iosApp/Bex/Assets.xcassets/AppIcon.appiconset/AppIcon.png
sips -z 192 192 "$icon" --out apps/mobile/src/main/res/drawable/bex_icon.png >/dev/null
iconset="$temporary/Bex.iconset"
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$icon" --out "$iconset/icon_${size}x${size}.png" >/dev/null
    doubled=$((size * 2))
    sips -z "$doubled" "$doubled" "$icon" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o apps/desktop/assets/icon.icns
