#if !BEX_NSH_CONVERSATION
    import AgentCore
    import SwiftUI
    import UIKit

    struct ThreadActivityHeader: View {
        @Environment(\.accessibilityReduceMotion) private var reduceMotion
        @Environment(\.scenePhase) private var scenePhase
        let summary: String
        let canCollapse: Bool
        let isInProgress: Bool
        let expanded: Bool

        var body: some View {
            let label = Text(verbatim: summary)
                .font(.system(size: 16))
                .lineLimit(1).truncationMode(.tail)
            HStack(spacing: 5) {
                label.foregroundStyle(.secondary).overlay {
                    if isInProgress, !reduceMotion, scenePhase == .active {
                        TimelineView(.animation(minimumInterval: 1.0 / 30)) { context in
                            // Share a clock across summary updates without restarting the sweep.
                            let phase = context.date.timeIntervalSinceReferenceDate
                                .truncatingRemainder(dividingBy: 2.8) / 2.8
                            LinearGradient(
                                colors: [.clear, .primary, .clear],
                                startPoint: UnitPoint(x: phase * 2 - 0.7, y: 0.5),
                                endPoint: UnitPoint(x: phase * 2, y: 0.5)
                            )
                        }
                        .mask(label.foregroundStyle(.white))
                        .accessibilityHidden(true)
                        .allowsHitTesting(false)
                    }
                }
                if canCollapse {
                    Image(systemName: expanded ? "chevron.down" : "chevron.right")
                        .font(.caption2.weight(.semibold)).foregroundColor(.secondary)
                }
                if isInProgress {
                    ProgressView().controlSize(.small)
                }
                Spacer(minLength: 0)
            }.contentShape(Rectangle())
        }
    }

    struct ThreadErrorRow: View {
        let error: TurnErrorPresentation

        var body: some View {
            VStack(alignment: .leading, spacing: 4) {
                Text("この応答").font(.caption).foregroundStyle(.secondary)
                HStack(spacing: 6) {
                    if error.isReconnecting {
                        ProgressView().controlSize(.small)
                    }
                    Text(verbatim: error.title).font(.subheadline.weight(.semibold))
                }
                Text(verbatim: error.message).textSelection(.enabled)
                if let details = error.details, !details.isEmpty {
                    DisclosureGroup("詳細を表示") {
                        Text(details).font(.caption).foregroundColor(.secondary).textSelection(.enabled)
                    }.font(.caption)
                }
            }
            .foregroundColor(error.isReconnecting ? .primary : error.isUsageLimit ? .orange : .red)
            .padding(10)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Color(UIColor.secondarySystemBackground))
            .overlay(RoundedRectangle(cornerRadius: 10)
                .stroke((error.isUsageLimit ? Color.orange : .red).opacity(error.isReconnecting ? 0 : 0.2)))
            .clipShape(RoundedRectangle(cornerRadius: 10))
            .accessibilityIdentifier("turn.error")
        }
    }

    struct ThreadMessageRow: View, Equatable {
        let row: VisibleConversationRow
        let item: ConversationItem
        let isUser: Bool
        let media: ConversationMediaAccess
        let selection: ConversationSelectionActions
        var showActions = false
        var timestampMs: UInt64?
        var fork: (() -> Void)?
        var forking = false
        var forkError: ErrorText?
        var restoreUnknown: ((String) -> Void)?
        var discardUnknown: ((String) -> Void)?
        @State private var copied = false

        static func == (lhs: Self, rhs: Self) -> Bool {
            lhs.row == rhs.row && lhs.item === rhs.item
                && lhs.media.host == rhs.media.host && lhs.media.cwd == rhs.media.cwd && lhs.media.draft == rhs.media
                .draft
                && lhs.forking == rhs.forking && lhs.forkError == rhs.forkError
                && (lhs.selection.askInSideChat == nil) == (rhs.selection.askInSideChat == nil)
                && (lhs.restoreUnknown == nil) == (rhs.restoreUnknown == nil)
                && (lhs.discardUnknown == nil) == (rhs.discardUnknown == nil)
        }

        @ViewBuilder private var images: some View {
            let sources = item.data.imageSources
            if item.data.imagePlaceholder {
                ConversationImageSkeleton(label: "画像を生成中", identifier: "image.generation.skeleton")
            }
            let thumbnails = ForEach(sources.indices, id: \.self) { index in
                ConversationImage(source: SessionImage(reference: sources[index]),
                                  label: isUser ? "添付画像" : "画像",
                                  identifier: "message.image.\(item.data.id).\(index)", media: media)
                    .frame(width: isUser ? 80 : nil, height: isUser ? 80 : nil)
                    .clipped()
            }
            if isUser, !sources.isEmpty {
                ViewThatFits(in: .horizontal) {
                    HStack(spacing: 8) { thumbnails }
                    ScrollView(.horizontal) { HStack(spacing: 8) { thumbnails } }.frame(height: 80)
                }
            } else {
                thumbnails
            }
        }

        var body: some View {
            VStack(alignment: isUser ? .trailing : .leading, spacing: 14) {
                VStack(alignment: isUser ? .trailing : .leading, spacing: 12) {
                    images
                    if let text = item.data.body, !text.isEmpty {
                        if isUser {
                            ConversationMarkdown(blocks: item.markdown, media: media, selection: selection,
                                                 textIdentifier: "message.user-text")
                                .padding(14)
                                .background(Color(UIColor.secondarySystemBackground),
                                            in: RoundedRectangle(cornerRadius: 22))
                        } else {
                            ConversationMarkdown(blocks: item.markdown, media: media, selection: selection)
                        }
                    }
                }
                .padding(.leading, isUser ? 42 : 0)
                .frame(maxWidth: .infinity, alignment: isUser ? .trailing : .leading)
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier("item.\(item.data.id)")
                if isUser || showActions {
                    actions
                }
                if isUser, item.data.nativeId == nil {
                    if let title = item.data.title {
                        Text(title).font(.caption).foregroundStyle(.secondary)
                    }
                    if let id = item.source.unknownSubmissionId() {
                        HStack(spacing: 18) {
                            Button { restoreUnknown?(id) } label: {
                                Label("入力欄へ戻す", systemImage: "pencil")
                            }
                            .accessibilityIdentifier("submission.restore." + id)
                            Button(role: .destructive) { discardUnknown?(id) } label: {
                                Label("破棄", systemImage: "trash")
                            }
                            .accessibilityIdentifier("submission.discard." + id)
                        }
                        .labelStyle(.iconOnly)
                        .font(.system(size: 19))
                        .buttonStyle(.plain)
                    }
                }
                if let forkError {
                    ErrorTextView(text: forkError).font(.caption).foregroundColor(.red)
                }
            }
            .padding(.bottom, isUser ? 12 : 8)
        }

        /// Both speakers share copy and time; the user's row mirrors toward its bubble and cannot fork.
        private var actions: some View {
            let text = item.data.body ?? ""
            return HStack(spacing: 20) {
                if isUser {
                    time
                }
                if !text.isEmpty {
                    Button { UIPasteboard.general.string = text; copied = true } label: {
                        Image(systemName: copied ? "checkmark" : "doc.on.doc")
                    }
                    .accessibilityLabel(LocalizedStringKey(copied ? "コピーしました" : isUser ? "発言をコピー" : "回答をコピー"))
                }
                if let fork {
                    Button(action: fork) {
                        // The branch turned to point forward: a new chat continues from here.
                        Image(systemName: "arrow.triangle.branch").rotationEffect(.degrees(90))
                    }
                    .disabled(forking)
                    .accessibilityLabel("ここから会話を分岐")
                    .accessibilityIdentifier("response.fork." + item.data.id)
                }
                if !isUser {
                    time
                }
            }
            .font(.system(size: 19)).foregroundColor(.secondary).buttonStyle(.plain)
            .padding(.vertical, 4)
        }

        @ViewBuilder private var time: some View {
            if let timestampMs {
                Text(Date(timeIntervalSince1970: Double(timestampMs) / 1000), style: .time).font(.caption)
            }
        }
    }

    struct ThreadItemRow: View, Equatable {
        let item: ConversationItem
        let media: ConversationMediaAccess
        let selection: ConversationSelectionActions
        let isExpanded: Bool
        let toggleExpanded: () -> Void
        let loadDetails: () async -> String?
        let watchOutput: (Bool) -> Void
        @State private var detailError: String?
        @State private var retry = 0

        static func == (lhs: Self, rhs: Self) -> Bool {
            lhs.item === rhs.item && lhs.isExpanded == rhs.isExpanded
                && lhs.media.host == rhs.media.host && lhs.media.cwd == rhs.media.cwd && lhs.media.draft == rhs.media
                .draft
                && (lhs.selection.askInSideChat == nil) == (rhs.selection.askInSideChat == nil)
        }

        private var icon: String {
            switch item.data.kind {
            case "command": "terminal"
            case "fileChange": "doc.badge.gearshape"
            case "webSearch": "globe"
            default: "chevron.left.forwardslash.chevron.right"
            }
        }

        var body: some View {
            VStack(alignment: .leading, spacing: 10) {
                if item.data.collapsible {
                    DisclosureGroup(isExpanded: Binding(
                        get: { isExpanded },
                        set: {
                            if $0 != isExpanded {
                                toggleExpanded()
                            }
                        }
                    )) {
                        if isExpanded {
                            let body = item.data.deferred ? "" : item.source.expandedBody()
                            if item.data.deferred {
                                VStack(alignment: .leading, spacing: 10) {
                                    if let detailError {
                                        ErrorMessage(source: detailError).font(.caption).foregroundColor(.red)
                                        Button("再読み込み") { retry += 1 }
                                    } else {
                                        ProgressView("詳細を読み込み中…")
                                    }
                                }
                                .task(id: retry) {
                                    detailError = nil
                                    let error = await loadDetails()
                                    guard !Task.isCancelled else { return }
                                    detailError = error
                                }
                                .id(ObjectIdentifier(item))
                            }
                            if !body.isEmpty {
                                ConversationMarkdown(
                                    blocks: ConversationMarkdownContent.parts(item.source.detailBlocks()),
                                    media: media,
                                    selection: selection
                                )
                            }
                        }
                    } label: {
                        Label {
                            if let title = item.data.title {
                                Text(title).lineLimit(1)
                            }
                        } icon: { Image(systemName: icon) }
                            .font(.system(size: 17))
                            .foregroundColor(.secondary)
                            .padding(.vertical, 5)
                            .accessibilityIdentifier("item.\(item.data.id)")
                    }
                } else {
                    ConversationMarkdown(blocks: item.markdown, media: media, selection: selection)
                        .foregroundColor(item.data.kind == "agent" || item.data.kind == "user" ? .primary : .secondary)
                        .accessibilityIdentifier("item.\(item.data.id)")
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .accessibilityElement(children: .contain)
            .onAppear { watchOutput(isExpanded) }
            .onChange(of: isExpanded) { _, expanded in watchOutput(expanded) }
            .onDisappear { watchOutput(false) }
        }
    }

#endif
