import SwiftUI

public struct ConversationInputMarker: View {
    let title: String
    let description: String
    let steer: Bool

    public init(title: String, description: String, steer: Bool) {
        self.title = title; self.description = description; self.steer = steer
    }

    public var body: some View {
        HStack(spacing: 4) {
            if steer { Image(systemName: "arrow.turn.down.right") }
            Text(verbatim: title)
        }
        .font(.caption).foregroundStyle(.secondary)
        .accessibilityHint(Text(verbatim: description))
        #if os(macOS)
            .help(Text(verbatim: description))
        #endif
    }
}

/// Native conversation components shared by the Apple clients.
public struct ConversationMessage<Images: View, Content: View, Actions: View, Notices: View, Markers: View>: View {
    let user: Bool
    let hasText: Bool
    let hasImages: Bool
    let collapsible: Bool
    let searching: Bool
    let persistentActions: Bool
    let expandTitle: String
    let collapseTitle: String
    let expand: () -> Void
    let images: Images
    let markers: Markers
    let identifier: String
    let content: Content
    let actions: Actions
    let notices: Notices
    @State private var expanded = false
    @State private var hovered = false

    public init(user: Bool, hasText: Bool, hasImages: Bool = false, collapsible: Bool = false, searching: Bool = false,
                persistentActions: Bool = false,
                expandTitle: String, collapseTitle: String, identifier: String, expand: @escaping () -> Void = {},
                @ViewBuilder markers: () -> Markers,
                @ViewBuilder images: () -> Images, @ViewBuilder content: () -> Content,
                @ViewBuilder actions: () -> Actions, @ViewBuilder notices: () -> Notices) {
        self.hasImages = hasImages; self.persistentActions = persistentActions
        self.user = user; self.hasText = hasText; self.collapsible = collapsible; self.searching = searching
        self.expandTitle = expandTitle; self.collapseTitle = collapseTitle; self.expand = expand
        self.markers = markers(); self.identifier = identifier
        self.images = images(); self.content = content(); self.actions = actions(); self.notices = notices()
    }

    public var body: some View {
        VStack(alignment: user ? .trailing : .leading, spacing: 4) {
            markers
            images
            if hasText {
                if user {
                    ConversationBubbleLayout {
                        content
                            .frame(maxHeight: folded ? 176 : nil, alignment: .top).clipped()
                            .overlay(alignment: .bottom) {
                                if folded {
                                    LinearGradient(
                                        colors: [.clear, ConversationColors.bubble],
                                        startPoint: .top,
                                        endPoint: .bottom
                                    )
                                    .frame(height: 28).allowsHitTesting(false)
                                }
                            }
                            .padding(12)
                            .background(ConversationColors.bubble, in: UnevenRoundedRectangle(
                                topLeadingRadius: 18, bottomLeadingRadius: 18, bottomTrailingRadius: 18,
                                topTrailingRadius: hasImages ? 6 : 18
                            ))
                            .accessibilityElement(children: .contain)
                            .accessibilityIdentifier("conversation.user-bubble")
                    }.frame(maxWidth: .infinity, alignment: .trailing)
                } else {
                    content.padding(.horizontal, 4)
                }
            }
            if user, collapsible {
                Button { expand(); expanded.toggle() } label: { Text(verbatim: expanded ? collapseTitle : expandTitle) }
                    .font(.system(size: 12)).foregroundStyle(ConversationColors.muted).padding(.horizontal, 8)
                    .frame(height: 24).padding(.top, 2)
            }
            actions.opacity(actionsVisible ? 1 : 0).accessibilityHidden(!actionsVisible)
            notices
        }
        .frame(maxWidth: .infinity, alignment: user ? .trailing : .leading)
        .accessibilityElement(children: .contain).accessibilityIdentifier(identifier)
        .onHover { hovered = $0 }
    }

    private var folded: Bool {
        collapsible && !expanded && !searching
    }

    private var actionsVisible: Bool {
        #if os(macOS)
            hovered || persistentActions
        #else
            true
        #endif
    }
}

public struct ConversationActivityHeader: View {
    let summary: String
    let canCollapse: Bool
    let isInProgress: Bool
    let expanded: Bool

    public init(summary: String, canCollapse: Bool, isInProgress: Bool, expanded: Bool) {
        self.summary = summary; self.canCollapse = canCollapse
        self.isInProgress = isInProgress; self.expanded = expanded
    }

    public var body: some View {
        let label = Text(verbatim: summary).font(.system(size: 14)).lineLimit(1).truncationMode(.tail)
        HStack(spacing: 5) {
            NativeShimmer(label: label, active: isInProgress)
            if canCollapse {
                Image(systemName: expanded ? "chevron.down" : "chevron.right")
                    .font(.caption2.weight(.semibold)).foregroundStyle(.secondary)
            }
            Spacer(minLength: 0)
        }.contentShape(Rectangle())
    }
}

public struct ConversationPlan<Content: View, Actions: View>: View {
    let title: String
    let badge: String
    let content: Content
    let actions: Actions
    public init(title: String, badge: String, @ViewBuilder content: () -> Content,
                @ViewBuilder actions: () -> Actions) {
        self.title = title; self.badge = badge; self.content = content(); self.actions = actions()
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack(spacing: 8) {
                Text(verbatim: badge).font(.system(size: 12, weight: .medium)).padding(.horizontal, 5).frame(height: 18)
                    .background(ConversationColors.separator, in: RoundedRectangle(cornerRadius: 4))
                    .overlay(RoundedRectangle(cornerRadius: 4).stroke(ConversationColors.cardBorder))
                Text(verbatim: title).font(.system(size: 14, weight: .medium))
                Spacer(minLength: 0)
                actions
            }
            content
        }
        .padding(20).foregroundStyle(ConversationColors.foreground)
        .background(ConversationColors.surface, in: RoundedRectangle(cornerRadius: 24))
        .overlay(RoundedRectangle(cornerRadius: 24).stroke(ConversationColors.cardBorder))
        .padding(.horizontal, 4).padding(.vertical, 2)
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
        .background(ConversationColors.code, in: RoundedRectangle(cornerRadius: 8))
        .accessibilityElement(children: .contain).accessibilityIdentifier("conversation.code")
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
                .overlay(RoundedRectangle(cornerRadius: 8).stroke(ConversationColors.cardBorder, lineWidth: 0.5))
            }
            actions
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
        .buttonStyle(ConversationActionStyle())
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
        HStack(spacing: 8) { content }
            .font(.system(size: 12)).foregroundStyle(ConversationColors.muted).buttonStyle(ConversationActionStyle())
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
        Button(action: open) {
            HStack(spacing: 10) {
                Image(systemName: "person.crop.circle").font(.system(size: 14)).frame(width: 24, height: 24)
                    .background(ConversationColors.bubble, in: Circle())
                    .overlay(Circle().stroke(ConversationColors.cardBorder))
                    .overlay(alignment: .bottomTrailing) {
                        Circle().fill(statusColor).frame(width: 8, height: 8)
                            .overlay(Circle().stroke(ConversationColors.surface, lineWidth: 2)).offset(x: 1, y: 1)
                    }
                VStack(alignment: .leading, spacing: 0) {
                    Text(verbatim: name).font(.system(size: 12, weight: .medium))
                        .foregroundStyle(ConversationColors.foreground).lineLimit(1)
                    message.font(.system(size: 11)).foregroundStyle(ConversationColors.muted)
                }.frame(maxWidth: .infinity, alignment: .leading)
                if let elapsed {
                    Text(verbatim: elapsed).font(.system(size: 11)).foregroundStyle(ConversationColors.muted)
                }
            }.padding(.horizontal, 8).padding(.vertical, 6)
        }.buttonStyle(.plain).disabled(!enabled).accessibilityLabel(Text(verbatim: name))
    }

    private var statusColor: Color {
        switch status {
        case "running": .accentColor
        case "completed": Color(red: 1.0 / 255, green: 178.0 / 255, blue: 118.0 / 255)
        case "failed": .red
        default: ConversationColors.muted
        }
    }
}

public struct ConversationDivider: View {
    let label: String
    public init(label: String) {
        self.label = label
    }

    public var body: some View {
        HStack(spacing: 12) {
            ConversationColors.separator.frame(height: 1)
            HStack(spacing: 6) {
                Image(systemName: "arrow.down.right.and.arrow.up.left")
                Text(verbatim: label)
            }.fixedSize(horizontal: true, vertical: false)
            ConversationColors.separator.frame(height: 1)
        }
        .font(.system(size: 12)).foregroundStyle(ConversationColors.muted)
        .padding(.vertical, 4)
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
            NativeShimmer(label: Text(verbatim: label).lineLimit(1), active: running)
                .frame(maxWidth: .infinity, alignment: .leading)
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
                .background(ConversationColors.code, in: RoundedRectangle(cornerRadius: 6)).padding(.leading, 30)
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
            header.padding(.bottom, 8)
                .overlay(alignment: .bottom) { ConversationColors.separator.frame(height: 1) }
            if showSteps {
                steps
            }
            if let error {
                Text(verbatim: error).font(.caption).foregroundStyle(.red)
            }
            ViewThatFits(in: .horizontal) {
                HStack { actions }
                VStack(alignment: .leading) { actions }
            }
            details
        }.font(.system(size: 14)).foregroundStyle(ConversationColors.muted).monospacedDigit()
            .accessibilityIdentifier("task.preparation")
    }
}

public struct ConversationWork<Content: View>: View {
    let title: String
    let symbol: String?
    let active: Bool
    let accessibilityLabel: String?
    @Binding var expanded: Bool
    let content: Content
    public init(
        title: String,
        symbol: String?,
        expanded: Binding<Bool>,
        active: Bool = false,
        accessibilityLabel: String? = nil,
        @ViewBuilder content: () -> Content
    ) {
        self.accessibilityLabel = accessibilityLabel
        self.title = title; self.symbol = symbol; self.active = active; _expanded = expanded; self.content = content()
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Button { expanded.toggle() } label: {
                HStack(spacing: 6) {
                    if let symbol { Image(systemName: symbol).font(.system(size: 16)).frame(width: 24, height: 24) }
                    NativeShimmer(label: Text(verbatim: title).lineLimit(1), active: active)
                        .frame(maxWidth: .infinity, alignment: .leading)
                    Image(systemName: expanded ? "chevron.down" : "chevron.right")
                        .font(.system(size: 12)).frame(width: 16, height: 16).opacity(0.7)
                }.font(.system(size: 14)).foregroundStyle(ConversationColors.muted)
            }.buttonStyle(.plain).accessibilityLabel(Text(verbatim: accessibilityLabel ?? title))
            if expanded {
                content.padding(.leading, 28)
            }
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
