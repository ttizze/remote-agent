import AppKit

struct ConversationPreparation: Decodable, Equatable {
    struct Step: Decodable, Equatable {
        let id: String
        let label: String
        let detail: String?
        let tail: String
        let icon: String
        let dim: Bool
        let running: Bool
        let time: String?
        var shimmer: MessageShimmer? = nil
    }

    struct Detail: Decodable, Equatable { let label: String; let value: String }
    let steps: [Step]
    let details: [Detail]
    let error: String?
}

@MainActor final class NativePreparationView: NSView {
    private var steps: [NativePreparationStep] = []
    private var ids: [String] = []
    private let error = NSTextField(wrappingLabelWithString: "")
    override var isFlipped: Bool {
        true
    }

    override init(frame: NSRect) {
        super.init(frame: frame)
        error.font = .systemFont(ofSize: 12); error.textColor = .systemRed; error.isSelectable = true
        addSubview(error)
    }

    @available(*, unavailable) required init?(coder _: NSCoder) {
        fatalError()
    }

    func update(_ value: ConversationPreparation?, document: MessageDocument) {
        let incoming = value?.steps ?? []
        var next: [NativePreparationStep] = []
        for step in incoming {
            let view = ids.firstIndex(of: step.id).map { steps[$0] } ?? NativePreparationStep(frame: .zero)
            if view.superview !== self {
                addSubview(view)
            }
            view.update(step, document: document); next.append(view)
        }
        for view in steps where !next.contains(where: { $0 === view }) {
            view.removeFromSuperview()
        }
        steps = next; ids = incoming.map(\.id)
        error.stringValue = value?.error ?? ""; error.isHidden = error.stringValue.isEmpty
        isHidden = value == nil
    }

    func measure(_ width: CGFloat) -> CGFloat {
        var y: CGFloat = 0
        for step in steps {
            let height = step.measure(width)
            step.frame = NSRect(x: 0, y: y, width: width, height: height); y += height
        }
        if !error.isHidden {
            let height = ceil((error.stringValue as NSString).boundingRect(
                with: NSSize(width: width, height: .greatestFiniteMagnitude),
                options: .usesLineFragmentOrigin,
                attributes: [.font: error.font!]
            ).height)
            error.frame = NSRect(x: 30, y: y + 4, width: max(1, width - 30), height: height); y = error.frame.maxY
        }
        return y
    }
}

@MainActor private final class NativePreparationStep: NSView {
    private let icon = NSImageView()
    private let progress = NSProgressIndicator()
    private let label = ShimmerLabel(labelWithString: "")
    private let detail = NSTextField(labelWithString: "")
    private let time = NSTextField(labelWithString: "")
    private let output = NSView()
    private let tail = NativeParagraph([])
    private var hasTail = false
    override var isFlipped: Bool {
        true
    }

    override init(frame: NSRect) {
        super.init(frame: frame)
        for view in [icon, progress, label, detail, time, output] {
            addSubview(view)
        }
        output.addSubview(tail); output.wantsLayer = true
        output.layer?.backgroundColor = NSColor(white: 0.065, alpha: 1).cgColor; output.layer?.cornerRadius = 6
        label.font = .systemFont(ofSize: 14); label.lineBreakMode = .byTruncatingTail
        for field in [detail, time] {
            field.font = .systemFont(ofSize: 12)
        }
        for field in [label, detail, time] {
            field.textColor = .secondaryLabelColor
        }
        icon.contentTintColor = .secondaryLabelColor; icon.setAccessibilityElement(false)
        progress.style = .spinning; progress.controlSize = .small; progress.isDisplayedWhenStopped = false
    }

    @available(*, unavailable) required init?(coder _: NSCoder) {
        fatalError()
    }

    func update(_ step: ConversationPreparation.Step, document: MessageDocument) {
        label.stringValue = step.label; detail.stringValue = step.detail ?? ""; time.stringValue = step.time ?? ""
        label.shimmer = step.shimmer
        icon.image = MessageAttributes.icon(step.icon, size: 14); icon.isHidden = step.running
        icon.contentTintColor = step.icon == "close" ? .systemRed : step
            .icon == "alert" ? .systemYellow : .secondaryLabelColor
        if step.running {
            progress.startAnimation(nil)
        } else {
            progress.stopAnimation(nil)
        }
        for view in [icon, label, detail, time] {
            view.alphaValue = step.dim ? 0.4 : 1
        }
        hasTail = !step.tail.isEmpty; output.isHidden = !hasTail
        _ = tail.replace(.text([.plain(step.tail, code: true)])); tail.update(document: document, media: [:])
    }

    func measure(_ width: CGFloat) -> CGFloat {
        icon.frame = NSRect(x: 4, y: 6, width: 16, height: 16); progress.frame = icon.frame
        time.sizeToFit(); detail.sizeToFit()
        time.frame.origin = NSPoint(x: max(30, width - time.frame.width), y: 7)
        detail.frame = NSRect(
            x: max(30, time.frame.minX - detail.frame.width - 8),
            y: 7,
            width: min(detail.frame.width, width / 3),
            height: 16
        )
        label.frame = NSRect(x: 30, y: 5, width: max(0, detail.frame.minX - 36), height: 20)
        guard hasTail else { return 28 }
        let contentWidth = max(1, width - 46)
        let height = tail.measure(contentWidth)
        output.frame = NSRect(x: 30, y: 28, width: max(1, width - 30), height: height + 16)
        tail.frame = NSRect(x: 8, y: 8, width: contentWidth, height: height)
        return output.frame.maxY + 4
    }
}
