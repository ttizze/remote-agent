import AgentCore
import SwiftUI
import UIKit

/// The stock palette from core, resolved per appearance, plus the mobile type scale.
enum AppTheme {
    private static let light = theme(dark: false).colors
    private static let dark = theme(dark: true).colors

    static func color(_ role: String) -> Color {
        Color(uiColor: uiColor(role))
    }

    static func uiColor(_ role: String) -> UIColor {
        adaptive(light[role, default: "#27272a"], dark[role, default: "#f5f5f5"])
    }

    static func adaptive(_ lightHex: String, _ darkHex: String) -> UIColor {
        let lightColor = hex(lightHex)
        let darkColor = hex(darkHex)
        return UIColor { $0.userInterfaceStyle == .dark ? darkColor : lightColor }
    }

    static func adaptiveColor(_ lightHex: String, _ darkHex: String) -> Color {
        Color(uiColor: adaptive(lightHex, darkHex))
    }

    private static func hex(_ value: String) -> UIColor {
        let digits = UInt64(value.dropFirst(), radix: 16) ?? 0
        let hasAlpha = value.count == 9
        let rgb = hasAlpha ? digits >> 8 : digits
        return UIColor(
            red: CGFloat((rgb >> 16) & 255) / 255,
            green: CGFloat((rgb >> 8) & 255) / 255,
            blue: CGFloat(rgb & 255) / 255,
            alpha: hasAlpha ? CGFloat(digits & 255) / 255 : 1
        )
    }

    static func font(_ size: CGFloat = 16, weight: Font.Weight = .regular) -> Font {
        let name = switch weight {
        case .bold, .heavy, .black, .semibold: "DMSans-Bold"
        case .medium: "DMSans-Medium"
        default: "DMSans-Regular"
        }
        return .custom(name, size: size)
    }

    static func mono(_ size: CGFloat, weight: Font.Weight = .regular) -> Font {
        .custom("Menlo", size: size).weight(weight)
    }

    // Tailwind hues the mobile palette uses for status, in light / dark pairs.
    static let sky = adaptiveColor("#0284c7", "#38bdf8")
    static let indigo = adaptiveColor("#4f46e5", "#a5b4fc")
    static let emerald = adaptiveColor("#047857", "#6ee7b7")
    static let emeraldIcon = adaptiveColor("#059669", "#34d399")
    static let rose = adaptiveColor("#e11d48", "#fb7185")
    static let roseText = adaptiveColor("#be123c", "#fda4af")
    static let amber = adaptiveColor("#b45309", "#fcd34d")
    static let violet = adaptiveColor("#7c3aed", "#a78bfa")

    static var text: Color {
        color("text")
    }

    static var muted: Color {
        color("textMuted")
    }

    static var tertiary: Color {
        color("textMuted").opacity(0.72)
    }

    static var border: Color {
        color("border")
    }

    static var borderSubtle: Color {
        color("border").opacity(0.6)
    }

    static var screen: Color {
        color("canvas")
    }

    static var primary: Color {
        color("accent")
    }

    static var danger: Color {
        color("errorSurface")
    }

    static var dangerForeground: Color {
        color("errorForeground")
    }

    static var subtle: Color {
        color("accentSurface")
    }

    static var subtleStrong: Color {
        color("input")
    }

    static var card: Color {
        color("surface")
    }

    static var groupedCard: Color {
        color("mobileGroupedCard")
    }
}

/// The mobile type scale: size and line height.
enum TypeScale {
    case micro, caption, label, footnote, body, headline, title, largeTitle

    var size: CGFloat {
        switch self {
        case .micro: 11
        case .caption: 12
        case .label: 13
        case .footnote: 14
        case .body: 16
        case .headline: 18
        case .title: 21
        case .largeTitle: 26
        }
    }

    var lineHeight: CGFloat {
        switch self {
        case .micro: 14
        case .caption: 16
        case .label: 17
        case .footnote: 19
        case .body, .headline: 23
        case .title: 28
        case .largeTitle: 32
        }
    }
}

extension View {
    func typeScale(_ scale: TypeScale, weight: Font.Weight = .regular) -> some View {
        font(AppTheme.font(scale.size, weight: weight))
            .lineSpacing(max(0, scale.lineHeight - scale.size * 1.2))
    }
}

extension StatusTone {
    var color: Color {
        switch self {
        case .working: AppTheme.sky
        case .completed: AppTheme.emeraldIcon
        case .failed: AppTheme.rose
        case .inactive: AppTheme.muted
        }
    }
}

struct NoticeText: View {
    let text: String

    var body: some View {
        Text(text)
            .foregroundColor(AppTheme.dangerForeground)
            .accessibilityIdentifier("notice")
    }
}
