import AgentCore
import SwiftUI

/// What a feed row can ask of its screen.
struct FeedActions {
    let perform: (Intent) -> Void
    let toggle: (WritableKeyPath<TimelineDisclosure, [String]>, String) -> Void
    let fork: (String) -> Void
    let openThread: (String) -> Void
    let download: (String, String) async throws -> URL
    /// "Use template" on an artifact card adds its prompt to the draft.
    let useArtifactTemplate: (ArtifactTemplate) -> Void
}

struct FeedRowView: View, Equatable {
    static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.row == rhs.row && lhs.forking == rhs.forking
    }

    let row: TimelineRow
    let actions: FeedActions
    var forking = false

    var body: some View {
        content.padding(.bottom, row.continuesWorkLog ? 1 : bottomGap)
    }

    private var bottomGap: CGFloat {
        switch row.kind {
        case .userMessage, .pendingMessage: 17.5
        case let .assistantMessage(message): message.meta == nil ? 3.5 : 17.5
        case .assistantMeta: 17.5
        case .handoff, .contextCompaction: 10.5
        default: 3.5
        }
    }

    @ViewBuilder
    private var content: some View {
        switch row.kind {
        case let .userMessage(message):
            UserBubble(
                text: message.text, attachments: message.attachments, createdAt: row.createdAt,
                badge: message.badge, attribution: message.decorations.attribution, actions: actions
            )
            .modifier(EntryFade(createdAt: row.createdAt, rises: true))
        case let .pendingMessage(message):
            UserBubble(
                text: message.text, attachments: message.attachments, createdAt: nil,
                badge: nil, attribution: nil, actions: actions
            )
            .modifier(EntryFade(createdAt: row.createdAt, rises: true))
        case let .assistantMessage(message):
            VStack(alignment: .leading, spacing: 3.5) {
                if !message.text.isEmpty {
                    ConversationMarkdown(source: message.text, useArtifactTemplate: actions.useArtifactTemplate)
                        .padding(.horizontal, 3.5)
                }
                if !message.attachments.isEmpty {
                    MessageAttachments(attachments: message.attachments, download: actions.download)
                }
                if let meta = message.meta {
                    AssistantMetaRow(meta: meta, createdAt: row.createdAt, forking: forking, fork: actions.fork)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .modifier(EntryFade(createdAt: row.createdAt, rises: false))
        case let .assistantMeta(_, meta):
            AssistantMetaRow(meta: meta, createdAt: row.createdAt, forking: forking, fork: actions.fork)
        default:
            WorkFeedRow(row: row, actions: actions)
        }
    }
}

/// Work log, folds, dividers and cards: everything that is not a message.
private struct WorkFeedRow: View {
    let row: TimelineRow
    let actions: FeedActions

    var body: some View {
        switch row.kind {
        case let .work(rows, _):
            VStack(spacing: 1) {
                ForEach(Array(rows.enumerated()), id: \.offset) { _, entry in
                    WorkLogRowView(row: entry, actions: actions)
                }
            }
        case let .liveWork(_, entry, _, _, _, _):
            WorkLogRowView(row: entry, actions: actions)
        case let .workToggle(toggle):
            WorkToggleView(toggle: toggle) {
                Haptics.selection()
                actions.toggle(\.expandedWorkGroups, toggle.groupId)
            }
        case .thinking:
            ThinkingRow()
        case .working:
            WorkingSinceRow(createdAt: row.createdAt)
        case let .fold(fold):
            FoldRowView(fold: fold) {
                if let attempt = fold.attempt {
                    actions.toggle(\.expandedAttempts, attempt)
                } else {
                    actions.toggle(\.expandedRuns, fold.run)
                }
            }
        case let .contextCompaction(label, active):
            ContextDivider(label: label, icon: "arrow.down.right.and.arrow.up.left", danger: false, shimmer: active)
        case let .lifecycle(lifecycle):
            LifecycleRowView(row: lifecycle, actions: actions)
        case let .subagents(card):
            SubagentGroupView(card: card, toggle: {
                Haptics.selection()
                actions.toggle(\.expandedWorkGroups, row.id)
            },
            open: actions.openThread)
        case let .handoff(divider):
            HandoffRow(divider: divider)
        case let .proposedPlan(plan):
            PlanCardView(plan: plan)
        case .worktreeSetup, .userMessage, .pendingMessage, .assistantMessage, .assistantMeta:
            EmptyView()
        }
    }
}

struct UserBubble: View {
    let text: String
    let attachments: [Attachment]
    let createdAt: Int64?
    let badge: IntentBadge?
    let attribution: AgentAttribution?
    let actions: FeedActions

    var body: some View {
        VStack(alignment: .trailing, spacing: 3.5) {
            if let attribution {
                Button(attribution.label) {
                    if let thread = attribution.senderThread {
                        actions.openThread(thread)
                    }
                }
                .buttonStyle(.plain)
                .font(AppTheme.font(12, weight: .medium)).foregroundStyle(AppTheme.muted.opacity(0.6))
            }
            VStack(alignment: .leading, spacing: 7) {
                if !attachments.isEmpty {
                    MessageAttachments(attachments: attachments, download: actions.download)
                }
                if !text.isEmpty {
                    Text(text).font(AppTheme.font(16)).lineSpacing(4).textSelection(.enabled)
                        .foregroundStyle(AppTheme.text)
                }
            }
            .padding(.horizontal, 12.25).padding(.vertical, 8.75)
            .background(AppTheme.color("mobileUserBubble"), in: RoundedRectangle(cornerRadius: 20))
            .containerRelativeFrame(.horizontal, alignment: .trailing) { width, _ in width * 0.85 }
            .fixedSize(horizontal: false, vertical: true)
            HStack(spacing: 3.5) {
                if let badge {
                    Text(badge.label).font(AppTheme.font(12, weight: .medium)).tracking(0.4)
                        .padding(.horizontal, 5.25).padding(.vertical, 1.75)
                        .background(badge.tone.color.opacity(0.14), in: Capsule())
                        .foregroundStyle(badge.tone.color)
                        .accessibilityLabel(badge.accessibilityLabel)
                }
                Text(createdAt.map(FeedTime.clock) ?? "Pending")
                    .font(AppTheme.font(13, weight: .medium)).monospacedDigit()
                    .foregroundStyle(AppTheme.muted)
                CopyButton(text: text)
            }
        }
        .frame(maxWidth: .infinity, alignment: .trailing)
    }
}

extension IntentTone {
    var color: Color {
        self == .queued ? AppTheme.amber : AppTheme.sky
    }
}

struct CopyButton: View {
    let text: String
    @State private var copied = false

    var body: some View {
        Button {
            Haptics.copy(text)
            copied = true
            Task {
                try? await Task.sleep(for: .seconds(1.2))
                copied = false
            }
        } label: {
            Image(systemName: copied ? "checkmark" : "doc.on.doc").font(.system(size: 13))
                .frame(width: 28, height: 28)
        }
        .buttonStyle(.plain)
        .foregroundStyle(AppTheme.muted)
        .accessibilityLabel("Copy")
    }
}

struct AssistantMetaRow: View {
    let meta: AssistantMeta
    let createdAt: Int64?
    let forking: Bool
    let fork: (String) -> Void

    var body: some View {
        HStack(spacing: 3.5) {
            if let action = meta.fork {
                Button { fork(action.run) } label: {
                    Group {
                        if forking {
                            ProgressView().controlSize(.mini)
                        } else {
                            Image(systemName: "arrow.triangle.branch").font(.system(size: 13))
                        }
                    }
                    .frame(width: 24.5, height: 24.5)
                }
                .buttonStyle(.plain)
                .disabled(forking)
                .accessibilityLabel(action.label)
            }
            if meta.copy.visible, let text = meta.copy.text {
                CopyButton(text: text)
            }
            if let chip = meta.statusChip {
                Text(chip).font(AppTheme.font(12, weight: .medium)).foregroundStyle(AppTheme.muted)
            }
            if meta.showTimestamp, let createdAt {
                Text(FeedTime.clock(createdAt)).font(AppTheme.font(13, weight: .medium)).monospacedDigit()
            }
        }
        .foregroundStyle(AppTheme.muted)
        .padding(.top, 3.5)
    }
}

enum FeedTime {
    static func clock(_ millis: Int64) -> String {
        Date(timeIntervalSince1970: Double(millis) / 1000).formatted(date: .omitted, time: .shortened)
    }
}

struct FoldRowView: View {
    let fold: FoldRow
    let toggle: () -> Void

    var body: some View {
        Button(action: toggle) {
            HStack(spacing: 5) {
                Text(fold.label).font(AppTheme.font(14, weight: .medium)).monospacedDigit()
                Image(systemName: "chevron.right").font(.system(size: 13))
                    .rotationEffect(.degrees(fold.expanded ? 90 : 0))
                Spacer()
            }
            .foregroundStyle(AppTheme.muted)
            .padding(.horizontal, 7)
            .frame(minHeight: 42)
            .overlay(alignment: .bottom) { Rectangle().fill(AppTheme.borderSubtle).frame(height: 1) }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }
}

/// A message that appears moments after it was written fades in; a user's
/// message also rises into place.
private struct EntryFade: ViewModifier {
    let rises: Bool
    @State private var shown: Bool

    init(createdAt: Int64?, rises: Bool) {
        self.rises = rises
        let now = Int64(Date().timeIntervalSince1970 * 1000)
        _shown = State(initialValue: !feedEntryFadesIn(createdAtMs: createdAt, nowMs: now))
    }

    func body(content: Content) -> some View {
        content
            .opacity(shown ? 1 : 0)
            .offset(y: shown || !rises ? 0 : 25)
            .onAppear {
                guard !shown else { return }
                withAnimation(.easeInOut(duration: 0.22)) { shown = true }
            }
    }
}

/// A selection tick as a response starts streaming into the feed and, at most
/// every 320 ms, as its text grows.
struct StreamingHaptics: ViewModifier {
    let thread: String
    let streaming: StreamingMessageMark?
    @State private var last: StreamHaptic?

    func body(content: Content) -> some View {
        content.onChange(of: Watch(thread: thread, streaming: streaming), initial: true) { _, watch in
            let next = streamHaptic(
                previous: last, threadId: watch.thread, streaming: watch.streaming,
                nowMs: Int64(Date().timeIntervalSince1970 * 1000)
            )
            if next.tick {
                Haptics.selection()
            }
            last = next
        }
    }

    private struct Watch: Equatable {
        let thread: String
        let streaming: StreamingMessageMark?
    }
}
