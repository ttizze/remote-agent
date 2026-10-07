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
        let lightColor = hex(light[role, default: "#27272a"])
        let darkColor = hex(dark[role, default: "#f5f5f5"])
        return UIColor { $0.userInterfaceStyle == .dark ? darkColor : lightColor }
    }

    /// `#rrggbb` or `#rrggbbaa`.
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

    static var sky: Color {
        color("statusSky")
    }

    static var indigo: Color {
        color("statusIndigo")
    }

    static var emerald: Color {
        color("statusEmerald")
    }

    static var emeraldIcon: Color {
        color("statusEmeraldIcon")
    }

    static var rose: Color {
        color("statusRose")
    }

    static var roseText: Color {
        color("statusRoseText")
    }

    static var amber: Color {
        color("statusAmber")
    }

    static var violet: Color {
        color("statusViolet")
    }

    static var text: Color {
        color("mobileForeground")
    }

    static var muted: Color {
        color("mobileForegroundMuted")
    }

    static var tertiary: Color {
        color("mobileForegroundTertiary")
    }

    static var border: Color {
        color("mobileBorder")
    }

    static var borderSubtle: Color {
        color("mobileBorderSubtle")
    }

    static var screen: Color {
        color("mobileScreen")
    }

    static var sheet: Color {
        color("mobileSheet")
    }

    static var primary: Color {
        color("mobilePrimary")
    }

    static var danger: Color {
        color("mobileDanger")
    }

    static var dangerForeground: Color {
        color("mobileDangerForeground")
    }

    static var warningForeground: Color {
        color("mobileWarningForeground")
    }

    static var subtle: Color {
        color("mobileSubtle")
    }

    static var subtleStrong: Color {
        color("mobileSubtleStrong")
    }

    static var card: Color {
        color("mobileCard")
    }

    static var cardAlt: Color {
        color("mobileCardAlt")
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
