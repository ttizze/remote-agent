#!/usr/bin/env bash
# Desktop PNG is the source for platform icon assets. Requires macOS sips/iconutil.
set -euo pipefail
cd "$(dirname "$0")/.."
source=apps/desktop/assets/icon.png
temporary=$(mktemp -d)
trap 'rm -rf "$temporary"' EXIT
iconset="$temporary/Bex.iconset"
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$source" --out "$iconset/icon_${size}x${size}.png" >/dev/null
    doubled=$((size * 2))
    sips -z "$doubled" "$doubled" "$source" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o apps/desktop/assets/icon.icns
# Mobile launcher icons use a white backdrop; Desktop preserves source alpha.
cat > "$temporary/mobile-icon.swift" <<'SWIFT'
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
xcrun swiftc "$temporary/mobile-icon.swift" -o "$temporary/mobile-icon"
"$temporary/mobile-icon" "$source" apps/mobile/iosApp/Bex/Assets.xcassets/AppIcon.appiconset/AppIcon.png
sips -z 192 192 apps/mobile/iosApp/Bex/Assets.xcassets/AppIcon.appiconset/AppIcon.png --out apps/mobile/src/main/res/drawable/bex_icon.png >/dev/null
