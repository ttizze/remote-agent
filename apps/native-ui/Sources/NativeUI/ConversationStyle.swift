import SwiftUI

#if os(macOS)
    private typealias ConversationPlatformColor = NSColor
#else
    private typealias ConversationPlatformColor = UIColor
#endif

/// NSH's conversation palette, with native system colors for a light appearance.
public enum ConversationColors {
    public static var surface: Color {
        adaptive(0x0F0F0F, light: .windowSurface)
    }

    public static var bubble: Color {
        adaptive(0x141414, light: .windowSurface)
    }

    public static var code: Color {
        adaptive(0x111111, light: .windowSurface)
    }

    public static var separator: Color {
        adaptive(0x161616, light: .conversationSeparator)
    }

    public static var cardBorder: Color {
        adaptive(0x1B1B1B, light: .conversationSeparator)
    }

    public static var muted: Color {
        adaptive(0xAAAAAA, light: .conversationSecondary)
    }

    public static var foreground: Color {
        adaptive(0xE6E6E6, light: .conversationForeground)
    }

    private static func adaptive(_ dark: UInt32, light: ConversationPlatformColor) -> Color {
        let color = ConversationPlatformColor(red: CGFloat(dark >> 16 & 255) / 255,
                                              green: CGFloat(dark >> 8 & 255) / 255,
                                              blue: CGFloat(dark & 255) / 255, alpha: 1)
        #if os(macOS)
            return Color(nsColor: NSColor(name: nil) { appearance in
                appearance.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua ? color : light
            })
        #else
            return Color(uiColor: UIColor { traits in traits.userInterfaceStyle == .dark ? color : light })
        #endif
    }
}

private extension ConversationPlatformColor {
    static var windowSurface: ConversationPlatformColor {
        #if os(macOS)
            .controlBackgroundColor
        #else
            .secondarySystemBackground
        #endif
    }

    static var conversationSeparator: ConversationPlatformColor {
        #if os(macOS)
            .separatorColor
        #else
            .separator
        #endif
    }

    static var conversationSecondary: ConversationPlatformColor {
        #if os(macOS)
            .secondaryLabelColor
        #else
            .secondaryLabel
        #endif
    }

    static var conversationForeground: ConversationPlatformColor {
        #if os(macOS)
            .labelColor
        #else
            .label
        #endif
    }
}

/// NSH Markdown sizes and paragraph rhythm, used by both TextKit adapters.
public enum ConversationTypography {
    public static func size(heading: Int?, code: Bool, table: Bool = false) -> CGFloat {
        let headings: [CGFloat] = [20, 18, 16, 14, 14, 14]
        return heading.map { headings[min(5, max(0, $0 - 1))] } ?? (code ? 13 : table ? 12 : 14)
    }
}

struct ConversationActionStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label.frame(minWidth: 24, minHeight: 24).contentShape(Rectangle())
            .opacity(configuration.isPressed ? 0.7 : 1)
    }
}

/// Ask native selectable content for its ideal width, then constrain its bubble.
struct ConversationBubbleLayout: Layout {
    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache _: inout ()) -> CGSize {
        guard let content = subviews.first else { return .zero }
        let ideal = content.sizeThatFits(.unspecified)
        let width = min(ideal.width, (proposal.width ?? ideal.width) * 0.8)
        return content.sizeThatFits(ProposedViewSize(width: width, height: proposal.height))
    }

    func placeSubviews(in bounds: CGRect, proposal _: ProposedViewSize, subviews: Subviews, cache _: inout ()) {
        subviews.first?.place(at: bounds.origin, proposal: ProposedViewSize(bounds.size))
    }
}
