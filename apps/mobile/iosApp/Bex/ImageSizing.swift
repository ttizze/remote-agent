import CoreGraphics

/// How a picked image is prepared before it is attached: formats the Host
/// accepts as they are pass through; anything else, or anything too large,
/// is redrawn as a JPEG whose longest edge is at most `maxEdge` pixels.
enum ImageSizing {
    static let maxEdge: CGFloat = 2048
    static let jpegQuality: CGFloat = 0.85
    static let passThroughTypes: Set<String> = ["image/gif", "image/jpeg", "image/png", "image/webp"]

    static func needsRendering(mimeType: String, needsCompression: Bool) -> Bool {
        needsCompression || !passThroughTypes.contains(mimeType.lowercased())
    }

    static func scaled(_ size: CGSize, maxEdge: CGFloat = maxEdge) -> CGSize {
        let longest = max(size.width, size.height)
        guard longest > maxEdge, longest > 0 else { return size }
        let scale = maxEdge / longest
        return CGSize(width: (size.width * scale).rounded(), height: (size.height * scale).rounded())
    }

    /// `photo.heic` becomes `photo.jpg`.
    static func jpegName(_ name: String) -> String {
        let base = name.split(separator: ".", omittingEmptySubsequences: false).dropLast().joined(separator: ".")
        return (base.isEmpty ? name : base) + ".jpg"
    }
}
