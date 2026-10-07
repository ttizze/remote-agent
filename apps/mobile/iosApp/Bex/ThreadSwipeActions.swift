import AgentCore
import SwiftUI
import UIKit

/// One circle a swiped row reveals.
struct SwipeActionSpec {
    let symbol: String
    let label: String
    let accessibilityLabel: String
    let run: () -> Void
}

/// The secondary circle: a menu of choices, such as the snooze wake times.
struct SwipeMenuSpec {
    let symbol: String
    let label: String
    let accessibilityLabel: String
    let title: String
    let choices: [ThreadMenuChild]
    let choose: (ThreadMenuAction) -> Void
}

/// A row that slides left to reveal circle actions. Releasing past the reveal
/// threshold keeps them open; a full swipe commits the primary action.
struct ThreadSwipeable<Content: View>: View {
    let id: String
    @Binding var openRow: String?
    let primary: SwipeActionSpec
    let secondary: SwipeMenuSpec?
    /// The smaller circles of slim rows.
    let compact: Bool
    let background: Color
    @ViewBuilder let content: (_ close: @escaping () -> Void) -> Content

    @State private var offset: CGFloat = 0
    @State private var restingOffset: CGFloat = 0
    @State private var width: CGFloat = 0
    @State private var armed = false
    @State private var committing = false

    private static var itemWidth: CGFloat {
        58
    }

    private var actionsWidth: CGFloat {
        secondary == nil ? Self.itemWidth : Self.itemWidth * 2
    }

    private var fullSwipeThreshold: CGFloat {
        max(actionsWidth + 44, (width - 32) * 0.58)
    }

    private var reveal: CGFloat {
        max(-offset, 0)
    }

    var body: some View {
        ZStack(alignment: .trailing) {
            if reveal > 0 {
                actions
            }
            content(close)
                .background(background)
                .offset(x: offset)
        }
        .background(background)
        .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { width = $0 }
        .gesture(HorizontalPan(
            open: restingOffset < 0, enabled: !committing, changed: drag, ended: release,
            cancelled: { settle(at: restingOffset) }
        ))
        .onChange(of: openRow) { _, row in
            if row != id, restingOffset != 0 {
                settle(at: 0)
            }
        }
        .accessibilityAction(named: Text(primary.label), primary.run)
    }

    private var actions: some View {
        HStack(spacing: 0) {
            SwipeCircle(
                symbol: primary.symbol, label: primary.label, primaryTone: true, compact: compact,
                geometry: geometry(entry: secondary == nil
                    ? (8, Self.itemWidth * 0.72)
                    : (Self.itemWidth * 0.55, Self.itemWidth * 2 * 0.85), stretches: true)
            ) {
                commit()
            }
            .accessibilityLabel(primary.accessibilityLabel)
            .zIndex(2)
            if let secondary {
                Menu {
                    Section(secondary.title) {
                        ForEach(Array(secondary.choices.enumerated()), id: \.offset) { _, choice in
                            Button {
                                close()
                                secondary.choose(choice.action)
                            } label: {
                                Text(choice.label)
                                if let detail = choice.detail {
                                    Text(detail)
                                }
                            }
                        }
                    }
                } label: {
                    SwipeCircle(
                        symbol: secondary.symbol, label: secondary.label, primaryTone: false, compact: compact,
                        geometry: geometry(entry: (8, Self.itemWidth * 0.72), stretches: false)
                    ) {}
                        .allowsHitTesting(false)
                }
                .accessibilityLabel(secondary.accessibilityLabel)
                .zIndex(1)
            }
        }
        .frame(width: actionsWidth)
        .frame(maxHeight: .infinity)
    }

    private func geometry(entry: (CGFloat, CGFloat), stretches: Bool) -> SwipeGeometry {
        SwipeGeometry(
            reveal: reveal, actionsWidth: actionsWidth, fullSwipeThreshold: fullSwipeThreshold,
            entryStart: entry.0, entryEnd: entry.1, stretches: stretches
        )
    }

    private func drag(_ translation: CGFloat) {
        if restingOffset == 0, translation < 0, openRow != id {
            openRow = id
        }
        offset = min(0, restingOffset + translation)
        let nowArmed = reveal >= fullSwipeThreshold
        if nowArmed, !armed {
            Haptics.medium()
        }
        armed = nowArmed
    }

    private func release(_ translation: CGFloat, _ velocity: CGFloat) {
        armed = false
        if reveal >= fullSwipeThreshold {
            commit()
            return
        }
        let projected = max(-(restingOffset + translation + velocity * 0.05), 0)
        settle(at: projected > actionsWidth * 0.42 ? -actionsWidth : 0)
    }

    /// Runs the primary action; the row slides away while the list moves it.
    private func commit() {
        committing = true
        withAnimation(.easeOut(duration: 0.22)) { offset = -max(width, reveal) }
        primary.run()
        Task { @MainActor in
            try? await Task.sleep(for: .milliseconds(600))
            committing = false
            settle(at: 0)
        }
    }

    private func close() {
        settle(at: 0)
    }

    private func settle(at target: CGFloat) {
        armed = false
        restingOffset = target
        if target == 0, openRow == id {
            openRow = nil
        }
        withAnimation(.interpolatingSpring(mass: 0.7, stiffness: 330, damping: 26)) { offset = target }
    }
}

/// Where a circle stands for the current reveal: it fades and grows in, and
/// the full-swipe circle stretches left while the other fades out.
struct SwipeGeometry {
    let reveal: CGFloat
    let actionsWidth: CGFloat
    let fullSwipeThreshold: CGFloat
    let entryStart: CGFloat
    let entryEnd: CGFloat
    let stretches: Bool

    private static func progress(_ value: CGFloat, from start: CGFloat, to end: CGFloat) -> CGFloat {
        guard end > start else { return value >= end ? 1 : 0 }
        return min(max((value - start) / (end - start), 0), 1)
    }

    var entry: CGFloat {
        Self.progress(reveal, from: entryStart, to: entryEnd)
    }

    private var excess: CGFloat {
        max(reveal - actionsWidth, 0)
    }

    var stretch: CGFloat {
        stretches ? excess : 0
    }

    var opacity: CGFloat {
        stretches ? entry
            : entry * (1 - Self.progress(reveal, from: actionsWidth, to: fullSwipeThreshold + 20))
    }

    var offset: CGFloat {
        22 * (1 - entry) - (stretches ? 0 : excess)
    }

    var scale: CGFloat {
        0.78 + 0.22 * entry
    }

    var iconOffset: CGFloat {
        -stretch * (0.5 + Self.progress(reveal, from: fullSwipeThreshold, to: fullSwipeThreshold + 20) * 0.5)
    }

    var labelOpacity: CGFloat {
        stretches ? 1 - Self.progress(reveal, from: fullSwipeThreshold - 24, to: fullSwipeThreshold) : 1
    }

    var labelOffset: CGFloat {
        stretches ? -excess * 0.5 : 0
    }
}

private struct SwipeCircle: View {
    let symbol: String
    let label: String
    let primaryTone: Bool
    let compact: Bool
    let geometry: SwipeGeometry
    let action: () -> Void

    var body: some View {
        let size: CGFloat = compact ? 28 : 36
        Button(action: action) {
            VStack(spacing: 0) {
                ZStack(alignment: .leading) {
                    Capsule().fill(primaryTone ? AppTheme.primary : AppTheme.color("mobileSecondary"))
                        .frame(width: size + geometry.stretch, height: size)
                        .offset(x: -geometry.stretch)
                    Image(systemName: symbol).font(.system(size: compact ? 13 : 15, weight: .medium))
                        .foregroundStyle(primaryTone ? AppTheme.color("mobilePrimaryForeground")
                            : AppTheme.color("mobileSecondaryForeground"))
                        .frame(width: size, height: size)
                        .offset(x: geometry.iconOffset)
                }
                .frame(width: size, height: size, alignment: .leading)
                Text(label).font(AppTheme.font(11, weight: .medium)).foregroundStyle(AppTheme.muted).lineLimit(1)
                    .frame(height: 14).padding(.top, compact ? 0 : 2)
                    .opacity(geometry.labelOpacity)
                    .offset(x: geometry.labelOffset)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .contentShape(Rectangle())
        }
        .buttonStyle(SwipeCirclePressStyle())
        .frame(width: 58)
        .frame(maxHeight: .infinity)
        .opacity(geometry.opacity)
        .offset(x: geometry.offset)
        .scaleEffect(geometry.scale)
    }
}

private struct SwipeCirclePressStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label.opacity(configuration.isPressed ? 0.72 : 1)
    }
}

/// A pan that starts only when the finger moves sideways (leftward unless
/// the row is open), so vertical scrolling keeps the list.
private struct HorizontalPan: UIGestureRecognizerRepresentable {
    let open: Bool
    let enabled: Bool
    let changed: @MainActor (CGFloat) -> Void
    let ended: @MainActor (CGFloat, CGFloat) -> Void
    let cancelled: @MainActor () -> Void

    func makeUIGestureRecognizer(context: Context) -> UIPanGestureRecognizer {
        let pan = UIPanGestureRecognizer()
        pan.delegate = context.coordinator
        pan.allowedScrollTypesMask = .continuous
        return pan
    }

    func updateUIGestureRecognizer(_ recognizer: UIPanGestureRecognizer, context: Context) {
        context.coordinator.open = open
        recognizer.isEnabled = enabled
    }

    func handleUIGestureRecognizerAction(_ recognizer: UIPanGestureRecognizer, context _: Context) {
        let translation = recognizer.translation(in: recognizer.view).x
        switch recognizer.state {
        case .began, .changed: changed(translation)
        case .ended: ended(translation, recognizer.velocity(in: recognizer.view).x)
        case .cancelled, .failed: cancelled()
        default: break
        }
    }

    func makeCoordinator(converter _: CoordinateSpaceConverter) -> Coordinator {
        Coordinator()
    }

    final class Coordinator: NSObject, UIGestureRecognizerDelegate {
        var open = false

        func gestureRecognizerShouldBegin(_ recognizer: UIGestureRecognizer) -> Bool {
            guard let pan = recognizer as? UIPanGestureRecognizer else { return false }
            let velocity = pan.velocity(in: pan.view)
            return abs(velocity.x) > abs(velocity.y) && (open || velocity.x < 0)
        }
    }
}
