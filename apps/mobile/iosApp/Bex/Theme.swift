import AgentCore
import SwiftUI
import UIKit

/// The stock palette from core, resolved per appearance, plus the mobile type scale.
enum AppTheme {
    private static var appearance = MobileAppearanceState.load()

    private static var coreAppearance: AgentCore.MobileAppearance {
        appearance.coreValue
    }

    private static var typography: AgentCore.MobileTypography {
        mobileTypography(appearance: coreAppearance)
    }

    static var preferredColorScheme: ColorScheme? {
        switch appearance.colorScheme {
        case .system: nil
        case .light: .light
        case .dark: .dark
        }
    }

    static func update(_ next: MobileAppearanceState) {
        appearance = next.normalized()
        appearance.save()
        NotificationCenter.default.post(name: .mobileAppearanceDidChange, object: nil)
    }

    static func updateTerminalFontSize(_ size: Double) {
        var next = appearance
        next.terminalFontSize = size
        update(next)
    }

    private static var light: [String: String] {
        mobileThemeColors(themeId: appearance.lightTheme ?? appearance.theme, dark: false)
    }

    private static var dark: [String: String] {
        mobileThemeColors(themeId: appearance.darkTheme ?? appearance.theme, dark: true)
    }

    static func color(_ role: String) -> Color {
        Color(uiColor: uiColor(role))
    }

    static func uiColor(_ role: String) -> UIColor {
        let lightColor = hex(light[role, default: "#27272a"])
        let darkColor = hex(dark[role, default: "#f5f5f5"])
        return UIColor { traits in
            switch appearance.colorScheme {
            case .light: lightColor
            case .dark: darkColor
            case .system: traits.userInterfaceStyle == .dark ? darkColor : lightColor
            }
        }
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
        .custom(fontName(weight), size: resolvedFontSize(size))
    }

    private static func resolvedFontSize(_ size: CGFloat) -> CGFloat {
        let sizes: [CGFloat: CGFloat] = [
            11: CGFloat(typography.microFontSize),
            12: CGFloat(typography.captionFontSize),
            13: CGFloat(typography.labelFontSize),
            14: CGFloat(typography.footnoteFontSize),
            15: CGFloat(typography.markdownH4FontSize),
            16: CGFloat(typography.bodyFontSize),
            18: CGFloat(typography.headlineFontSize),
            21: CGFloat(typography.titleFontSize),
            26: CGFloat(typography.largeTitleFontSize),
            30: CGFloat(typography.displayFontSize)
        ]
        return sizes[size] ?? size * CGFloat(typography.baseFontSize) / 16
    }

    static func markdownFont(_ header: UInt8?, weight: Font.Weight = .regular) -> Font {
        .custom(fontName(weight), size: markdownFontSize(header))
    }

    private static func fontName(_ weight: Font.Weight) -> String {
        switch weight {
        case .bold, .heavy, .black, .semibold: "DMSans-Bold"
        case .medium: "DMSans-Medium"
        default: "DMSans-Regular"
        }
    }

    static func mono(_ size: CGFloat, weight: Font.Weight = .regular) -> Font {
        let scaled = size == 13 ? CGFloat(typography.codeFontSize) : size * CGFloat(typography.baseFontSize) / 16
        return .custom("Menlo", size: scaled).weight(weight)
    }

    static func markdownMono(weight: Font.Weight = .regular) -> Font {
        .custom("Menlo", size: CGFloat(typography.markdownCodeFontSize)).weight(weight)
    }

    static func terminalMono() -> Font {
        .custom("Menlo", size: CGFloat(typography.terminalFontSize)).monospaced().weight(.regular)
    }

    static var codeFontSize: CGFloat {
        CGFloat(typography.codeFontSize)
    }

    static var codeLineHeight: CGFloat {
        CGFloat(typography.codeLineHeight)
    }

    static var codeLineNumberFontSize: CGFloat {
        CGFloat(typography.codeLineNumberFontSize)
    }

    static var markdownCodeFontSize: CGFloat {
        CGFloat(typography.markdownCodeFontSize)
    }

    static var markdownCodeLineHeight: CGFloat {
        CGFloat(typography.markdownCodeLineHeight)
    }

    static var markdownBodyLineHeight: CGFloat {
        CGFloat(typography.markdownBodyLineHeight)
    }

    static var codeWordWrap: Bool {
        appearance.codeWordWrap
    }

    static func markdownFontSize(_ header: UInt8?) -> CGFloat {
        switch header {
        case nil: CGFloat(typography.markdownBodyFontSize)
        case 1: CGFloat(typography.markdownH1FontSize)
        case 2: CGFloat(typography.markdownH2FontSize)
        case 3: CGFloat(typography.markdownH3FontSize)
        default: CGFloat(typography.markdownH4FontSize)
        }
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

/// The small local store for mobile-only appearance controls. Core owns the
/// ranges and resolved palette; this value only bridges UserDefaults to the
/// SwiftUI controls.
struct MobileAppearanceState: Equatable {
    enum ColorScheme: String, CaseIterable, Equatable, Hashable, Codable {
        case system, light, dark

        var label: String {
            rawValue.capitalized
        }
    }

    var colorScheme: ColorScheme
    var theme: String?
    var lightTheme: String?
    var darkTheme: String?
    var baseFontSize: Int
    var codeFontSize: Int?
    var terminalFontSize: Double?
    var codeWordWrap: Bool
    private static let key = "mobile.appearance"

    static func load() -> Self {
        guard let data = UserDefaults.standard.data(forKey: key),
              let value = try? JSONDecoder().decode(Self.self, from: data) else {
            return Self(core: mobileAppearanceDefault())
        }
        return value.normalized()
    }

    func save() {
        if let data = try? JSONEncoder().encode(self) {
            UserDefaults.standard.set(data, forKey: Self.key)
        }
        UserDefaults.standard.set(resolvedTerminalFontSize, forKey: "terminal.fontSize")
    }

    var resolvedTerminalFontSize: Double {
        mobileTypography(appearance: coreValue).terminalFontSize
    }

    var resolvedCodeFontSize: Int {
        Int(mobileTypography(appearance: coreValue).codeFontSize)
    }

    func normalized() -> Self {
        Self(core: normalizeMobileAppearance(appearance: coreValue))
    }

    func assigningTheme(_ id: String?, dark: Bool) -> Self {
        Self(core: mobileAssignTheme(appearance: coreValue, dark: dark, themeId: id))
    }

    fileprivate var coreValue: AgentCore.MobileAppearance {
        let scheme: AgentCore.MobileColorScheme = switch colorScheme {
        case .system: .system
        case .light: .light
        case .dark: .dark
        }
        return AgentCore.MobileAppearance(
            colorScheme: scheme,
            theme: theme,
            lightTheme: lightTheme,
            darkTheme: darkTheme,
            baseFontSize: UInt32(clamping: baseFontSize),
            codeFontSize: codeFontSize.map { UInt32(clamping: $0) },
            terminalFontSize: terminalFontSize,
            codeWordWrap: codeWordWrap
        )
    }

    fileprivate init(core: AgentCore.MobileAppearance) {
        colorScheme = switch core.colorScheme {
        case .system: .system
        case .light: .light
        case .dark: .dark
        @unknown default: .system
        }
        theme = core.theme
        lightTheme = core.lightTheme
        darkTheme = core.darkTheme
        baseFontSize = Int(core.baseFontSize)
        codeFontSize = core.codeFontSize.map(Int.init)
        terminalFontSize = core.terminalFontSize
        codeWordWrap = core.codeWordWrap
    }
}

extension MobileAppearanceState: Codable {}

extension Notification.Name {
    static let mobileAppearanceDidChange = Notification.Name("mobile.appearance.didChange")
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
