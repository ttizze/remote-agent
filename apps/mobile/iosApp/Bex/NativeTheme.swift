import AgentCore
import SwiftUI
import UIKit

extension Color {
    init(paletteRGB value: UInt32) {
        self.init(.sRGB, red: Double((value >> 16) & 255) / 255,
                  green: Double((value >> 8) & 255) / 255,
                  blue: Double(value & 255) / 255, opacity: 1)
    }
}

extension ColorScheme {
    var nativePalette: NativePalette {
        AgentCore.nativePalette(platform: .ios, dark: self == .dark)
    }
}

extension UIFont {
    static func conversationFont(size: CGFloat, weight: UIFont.Weight = .regular) -> UIFont {
        let name = weight >= .semibold ? "DMSans-Bold" : weight >= .medium ? "DMSans-Medium" : "DMSans-Regular"
        return UIFont(name: name, size: size) ?? .systemFont(ofSize: size, weight: weight)
    }
}
