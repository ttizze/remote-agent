import SwiftUI

/// The existing iPhone effect, shared with Mac conversation controls. Their
/// text stays native, while the overlay masks the moving band to its glyphs.
public struct NativeShimmer<Label: View>: View {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.scenePhase) private var scenePhase
    private let label: Label
    private let active: Bool
    private let color: Color
    private let period: Double
    private let band: Double?
    private let focus: Bool
    private let stops: [[Double]]

    public init(label: Label, active: Bool, color: Color = .secondary, period: Double = 2.8,
                band: Double? = nil, focus: Bool = true,
                stops: [[Double]] = [[0, 0], [0.5, 1], [1, 0]]) {
        self.label = label; self.active = active; self.color = color
        self.period = period; self.band = band; self.focus = focus; self.stops = stops
    }

    public var body: some View {
        label.foregroundStyle(color).overlay {
            if active, !reduceMotion, scenePhase == .active {
                TimelineView(.animation(minimumInterval: 1.0 / 30)) { context in
                    GeometryReader { geometry in
                        let duration = max(0.1, period)
                        let phase = context.date.timeIntervalSinceReferenceDate
                            .truncatingRemainder(dividingBy: duration) / duration
                        let span = band.map { $0 / max(1, geometry.size.width) } ?? 0.7
                        let travel = band == nil ? 2 : 1 + span * (focus ? 2 : 1)
                        let left = phase * travel - span
                        LinearGradient(stops: stops.map { .init(color: Color.primary.opacity($0[1]), location: $0[0]) },
                                       startPoint: UnitPoint(x: left, y: 0.5),
                                       endPoint: UnitPoint(x: left + span, y: 0.5))
                    }
                }
                .mask(label.foregroundStyle(.white))
                .accessibilityHidden(true)
                .allowsHitTesting(false)
            }
        }
    }
}
