import SwiftUI

public enum ConversationColors {
    public static var surface: Color {
        #if os(macOS)
            Color(nsColor: .controlBackgroundColor)
        #else
            Color(uiColor: .secondarySystemBackground)
        #endif
    }

    public static var separator: Color {
        #if os(macOS)
            Color(nsColor: .separatorColor)
        #else
            Color(uiColor: .separator)
        #endif
    }
}

/// The iPhone message layout is shared verbatim with Mac. Only the selectable
/// content and OS actions are supplied by each platform.
public struct ConversationMessage<Images: View, Content: View, Actions: View, Notices: View, Markers: View>: View {
    let user: Bool
    let hasText: Bool
    let collapsible: Bool
    let searching: Bool
    let expandTitle: String
    let collapseTitle: String
    let images: Images
    let markers: Markers
    let identifier: String
    let content: Content
    let actions: Actions
    let notices: Notices
    @State private var expanded = false

    public init(user: Bool, hasText: Bool, collapsible: Bool = false, searching: Bool = false,
                expandTitle: String, collapseTitle: String, identifier: String,
                @ViewBuilder markers: () -> Markers,
                @ViewBuilder images: () -> Images, @ViewBuilder content: () -> Content,
                @ViewBuilder actions: () -> Actions, @ViewBuilder notices: () -> Notices) {
        self.user = user; self.hasText = hasText; self.collapsible = collapsible; self.searching = searching
        self.expandTitle = expandTitle; self.collapseTitle = collapseTitle
        self.markers = markers(); self.identifier = identifier
        self.images = images(); self.content = content(); self.actions = actions(); self.notices = notices()
    }

    public var body: some View {
        VStack(alignment: user ? .trailing : .leading, spacing: 14) {
            markers
            VStack(alignment: user ? .trailing : .leading, spacing: 12) {
                images
                if hasText {
                    if user {
                        content
                            .frame(maxHeight: collapsible && !expanded && !searching ? 176 : nil, alignment: .top)
                            .clipped()
                            .padding(14)
                            .background(ConversationColors.surface, in: RoundedRectangle(cornerRadius: 22))
                    } else {
                        content
                    }
                }
            }
            .padding(.leading, user ? 42 : 0)
            .frame(maxWidth: .infinity, alignment: user ? .trailing : .leading)
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier(identifier)
            if user, collapsible {
                Button { expanded.toggle() } label: { Text(verbatim: expanded ? collapseTitle : expandTitle) }
                    .font(.caption).foregroundStyle(.secondary)
            }
            actions
            notices
        }
        .padding(.bottom, user ? 12 : 8)
    }
}

public struct ConversationActivityHeader: View {
    let summary: String
    let canCollapse: Bool
    let isInProgress: Bool
    let expanded: Bool
    let progress: Bool

    public init(summary: String, canCollapse: Bool, isInProgress: Bool, expanded: Bool,
                progress: Bool = true) {
        self.summary = summary; self.canCollapse = canCollapse
        self.isInProgress = isInProgress; self.expanded = expanded; self.progress = progress
    }

    public var body: some View {
        let label = Text(verbatim: summary).font(.system(size: 16)).lineLimit(1).truncationMode(.tail)
        HStack(spacing: 5) {
            NativeShimmer(label: label, active: isInProgress)
            if canCollapse {
                Image(systemName: expanded ? "chevron.down" : "chevron.right")
                    .font(.caption2.weight(.semibold)).foregroundStyle(.secondary)
            }
            if isInProgress, progress {
                ProgressView().controlSize(.small)
            }
            Spacer(minLength: 0)
        }.contentShape(Rectangle())
    }
}

public struct ConversationPlan<Content: View, Actions: View>: View {
    let title: String
    let content: Content
    let actions: Actions
    public init(title: String, @ViewBuilder content: () -> Content, @ViewBuilder actions: () -> Actions) {
        self.title = title; self.content = content(); self.actions = actions()
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack { Text(verbatim: title).font(.subheadline.weight(.semibold)); Spacer(); actions }
            content
        }
        .padding(12)
        .background(ConversationColors.surface, in: RoundedRectangle(cornerRadius: 12))
        .overlay(RoundedRectangle(cornerRadius: 12).stroke(ConversationColors.separator))
    }
}

public struct ConversationCode<Header: View, Content: View>: View {
    let header: Header
    let content: Content
    public init(@ViewBuilder header: () -> Header, @ViewBuilder content: () -> Content) {
        self.header = header(); self.content = content()
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header.font(.caption.weight(.medium)).foregroundStyle(.secondary)
                .padding(.horizontal, 12).padding(.vertical, 10)
            Divider()
            content
        }
        .background(ConversationColors.surface, in: RoundedRectangle(cornerRadius: 12))
        .overlay(RoundedRectangle(cornerRadius: 12).stroke(ConversationColors.separator, lineWidth: 0.5))
    }
}

public struct ConversationTable<Actions: View, Cell: View>: View {
    let rowLengths: [Int]
    let columnWidth: CGFloat
    let actions: Actions
    let cell: (Int, Int) -> Cell
    public init(rowLengths: [Int], columnWidth: CGFloat,
                @ViewBuilder actions: () -> Actions, @ViewBuilder cell: @escaping (Int, Int) -> Cell) {
        self.rowLengths = rowLengths; self.columnWidth = columnWidth
        self.actions = actions(); self.cell = cell
    }

    public var body: some View {
        VStack(alignment: .trailing, spacing: 6) {
            actions
            ScrollView(.horizontal) {
                VStack(alignment: .leading, spacing: 0) {
                    ForEach(rowLengths.indices, id: \.self) { row in
                        HStack(alignment: .top, spacing: 0) {
                            ForEach(0 ..< rowLengths[row], id: \.self) { column in
                                cell(row, column).frame(width: columnWidth).padding(10)
                            }
                        }
                        .background(row == 0 ? ConversationColors.surface : Color.clear)
                        Divider()
                    }
                }
                .clipShape(RoundedRectangle(cornerRadius: 8))
                .overlay(RoundedRectangle(cornerRadius: 8).stroke(ConversationColors.separator, lineWidth: 0.5))
            }
        }
    }
}

public struct ConversationCopyButton: View {
    let title: String
    let copiedTitle: String
    let action: () -> Void
    @State private var copied = false
    public init(title: String, copiedTitle: String, action: @escaping () -> Void) {
        self.title = title; self.copiedTitle = copiedTitle; self.action = action
    }

    public var body: some View {
        Button { action(); copied = true } label: {
            Image(systemName: copied ? "checkmark" : "doc.on.doc")
        }
        .accessibilityLabel(Text(verbatim: copied ? copiedTitle : title))
        .buttonStyle(.plain)
        .task(id: copied) {
            guard copied else { return }
            do { try await Task.sleep(for: .seconds(2)) } catch { return }
            copied = false
        }
    }
}

public struct ConversationActions<Content: View>: View {
    let content: Content
    public init(@ViewBuilder content: () -> Content) {
        self.content = content()
    }

    public var body: some View {
        HStack(spacing: 20) { content }
            .font(.system(size: 19)).foregroundStyle(.secondary).buttonStyle(.plain).padding(.vertical, 4)
    }
}

public struct ConversationImages<Content: View>: View {
    let content: Content
    public init(@ViewBuilder content: () -> Content) {
        self.content = content()
    }

    public var body: some View {
        ViewThatFits(in: .horizontal) {
            HStack(spacing: 8) { content }
            ScrollView(.horizontal) { HStack(spacing: 8) { content } }.frame(height: 80)
        }
    }
}

public struct ConversationSubagent<Message: View>: View {
    let name: String
    let status: String
    let elapsed: String?
    let enabled: Bool
    let open: () -> Void
    let message: Message
    public init(name: String, status: String, elapsed: String?, enabled: Bool, open: @escaping () -> Void,
                @ViewBuilder message: () -> Message) {
        self.name = name; self.status = status; self.elapsed = elapsed; self.enabled = enabled
        self.open = open; self.message = message()
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Button(action: open) {
                    HStack(spacing: 8) {
                        Image(systemName: "person.crop.circle").overlay(alignment: .bottomTrailing) {
                            Circle().fill(statusColor).frame(width: 7, height: 7)
                        }
                        Text(verbatim: name).lineLimit(1)
                    }
                }.disabled(!enabled)
                Spacer(minLength: 8)
                if let elapsed {
                    Text(verbatim: elapsed).font(.caption).foregroundStyle(.secondary)
                }
            }
            message.padding(.leading, 28)
        }.buttonStyle(.plain)
    }

    private var statusColor: Color {
        switch status {
        case "running": .accentColor
        case "completed": .green
        case "failed": .red
        default: .secondary
        }
    }
}

public struct ConversationDivider: View {
    let label: String
    public init(label: String) {
        self.label = label
    }

    public var body: some View {
        HStack(spacing: 8) {
            Image(systemName: "arrow.down.right.and.arrow.up.left")
            Text(verbatim: label)
            Rectangle().frame(height: 1)
        }.font(.caption).foregroundStyle(.secondary)
    }
}

public struct ConversationPreparationStep: View {
    let label: String
    let detail: String?
    let tail: String
    let symbol: String
    let pending: Bool
    let running: Bool
    let failed: Bool
    let elapsed: String?
    public init(label: String, detail: String?, tail: String, symbol: String,
                pending: Bool, running: Bool, failed: Bool, elapsed: String?) {
        self.label = label; self.detail = detail; self.tail = tail; self.symbol = symbol
        self.pending = pending; self.running = running; self.failed = failed; self.elapsed = elapsed
    }

    public var body: some View {
        HStack(spacing: 8) {
            if running {
                ProgressView().controlSize(.mini).frame(width: 20)
            } else {
                Image(systemName: symbol).frame(width: 20)
            }
            Text(verbatim: label).frame(maxWidth: .infinity, alignment: .leading)
            if let detail {
                Text(verbatim: detail).font(.caption)
            }
            if let elapsed {
                Text(verbatim: elapsed).font(.caption)
            }
        }
        .opacity(pending ? 0.4 : 1).foregroundStyle(failed ? Color.red : .secondary)
        if !tail.isEmpty {
            Text(verbatim: tail).font(.caption.monospaced()).textSelection(.enabled)
                .padding(8).frame(maxWidth: .infinity, alignment: .leading)
                .background(.quaternary, in: RoundedRectangle(cornerRadius: 6)).padding(.leading, 28)
        }
    }
}

public struct ConversationPreparation<Header: View, Steps: View, Actions: View, Details: View>: View {
    let header: Header
    let showSteps: Bool
    let steps: Steps
    let error: String?
    let actions: Actions
    let details: Details
    public init(showSteps: Bool, error: String?, @ViewBuilder header: () -> Header,
                @ViewBuilder steps: () -> Steps, @ViewBuilder actions: () -> Actions,
                @ViewBuilder details: () -> Details) {
        self.header = header(); self.showSteps = showSteps; self.steps = steps(); self.error = error
        self.actions = actions(); self.details = details()
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            header
            if showSteps {
                Divider(); steps
            }
            if let error {
                Text(verbatim: error).font(.caption).foregroundStyle(.red)
            }
            ViewThatFits(in: .horizontal) {
                HStack { actions }
                VStack(alignment: .leading) { actions }
            }
            details
        }.font(.subheadline).foregroundStyle(.secondary).monospacedDigit()
            .accessibilityIdentifier("task.preparation")
    }
}

public struct ConversationWork<Content: View>: View {
    let title: String
    let symbol: String
    let active: Bool
    @Binding var expanded: Bool
    let content: Content
    public init(
        title: String,
        symbol: String,
        expanded: Binding<Bool>,
        active: Bool = false,
        @ViewBuilder content: () -> Content
    ) {
        self.title = title; self.symbol = symbol; self.active = active; _expanded = expanded; self.content = content()
    }

    public var body: some View {
        DisclosureGroup(isExpanded: $expanded) { content } label: {
            Label { NativeShimmer(label: Text(verbatim: title).lineLimit(1), active: active) } icon: {
                Image(systemName: symbol)
            }
            .font(.system(size: 17)).foregroundStyle(.secondary).padding(.vertical, 5)
        }
    }
}

public struct ConversationError<Content: View, Details: View>: View {
    let context: String
    let title: String
    let reconnecting: Bool
    let usageLimit: Bool
    let content: Content
    let details: Details
    public init(context: String, title: String, reconnecting: Bool, usageLimit: Bool,
                @ViewBuilder content: () -> Content, @ViewBuilder details: () -> Details) {
        self.context = context; self.title = title; self.reconnecting = reconnecting; self.usageLimit = usageLimit
        self.content = content(); self.details = details()
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(verbatim: context).font(.caption).foregroundStyle(.secondary)
            HStack(spacing: 6) {
                if reconnecting {
                    ProgressView().controlSize(.small)
                }
                Text(verbatim: title).font(.subheadline.weight(.semibold))
            }
            content
            details
        }
        .foregroundStyle(reconnecting ? Color.primary : usageLimit ? .orange : .red)
        .padding(10).frame(maxWidth: .infinity, alignment: .leading)
        .background(ConversationColors.surface)
        .overlay(RoundedRectangle(cornerRadius: 10)
            .stroke((usageLimit ? Color.orange : .red).opacity(reconnecting ? 0 : 0.2)))
        .clipShape(RoundedRectangle(cornerRadius: 10)).accessibilityIdentifier("turn.error")
    }
}
