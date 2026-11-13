import AgentCore
import SwiftUI
import UIKit

struct ThreadErrorRow: View {
    let error: TurnErrorPresentation

    var body: some View {
        ConversationError(context: String(localized: "この応答"), title: error.title,
                          reconnecting: error.isReconnecting, usageLimit: error.isUsageLimit) {
            Text(verbatim: error.message).textSelection(.enabled)
        } details: {
            if let details = error.details, !details.isEmpty {
                DisclosureGroup("詳細を表示") {
                    Text(details).font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
                }.font(.caption)
            }
        }
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
    var edit: (() -> Void)?
    var editingEnabled = true

    static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.row == rhs.row && lhs.item === rhs.item
            && lhs.media.host == rhs.media.host && lhs.media.cwd == rhs.media.cwd && lhs.media.draft == rhs.media
            .draft
            && lhs.forking == rhs.forking && lhs.forkError == rhs.forkError
            && (lhs.selection.askInSideChat == nil) == (rhs.selection.askInSideChat == nil)
            && (lhs.restoreUnknown == nil) == (rhs.restoreUnknown == nil)
            && (lhs.discardUnknown == nil) == (rhs.discardUnknown == nil)
            && (lhs.edit == nil) == (rhs.edit == nil) && lhs.editingEnabled == rhs.editingEnabled
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
            ConversationImages { thumbnails }
        } else {
            thumbnails
        }
    }

    var body: some View {
        ConversationMessage(user: isUser, hasText: !(item.data.body ?? "").isEmpty,
                            hasImages: !item.data.imageSources.isEmpty,
                            collapsible: item.source.collapsesUserText(),
                            expandTitle: String(localized: "全文を表示"), collapseTitle: String(localized: "折りたたむ"),
                            identifier: "item.\(item.data.id)") {
            if row.data().queued {
                Text("キューから送信").font(.caption).foregroundStyle(.secondary)
            }
            if row.data().steer {
                Text("追加の指示").font(.caption).foregroundStyle(.secondary)
            }
        } images: {
            images
        } content: {
            ConversationMarkdown(blocks: item.markdown, media: media, selection: selection,
                                 textIdentifier: isUser ? "message.user-text" : "message.assistant-text")
        } actions: {
            if isUser || showActions {
                actions
            }
        } notices: {
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
    }

    /// Both speakers share copy and time; the user's row mirrors toward its bubble and cannot fork.
    private var actions: some View {
        let text = item.data.body ?? ""
        return ConversationActions {
            if isUser {
                time
            }
            if let edit {
                Button(action: edit) { Image(systemName: "pencil") }
                    .disabled(!editingEnabled)
                    .accessibilityLabel("ここから編集")
                    .accessibilityIdentifier("message.edit." + item.data.id)
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
            if !text.isEmpty {
                ConversationCopyButton(title: String(localized: isUser ? "発言をコピー" : "回答をコピー"),
                                       copiedTitle: String(localized: "コピーしました")) {
                    UIPasteboard.general.string = text
                }
            }
            if !isUser {
                time
            }
        }
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
                ConversationWork(title: item.data.title ?? "", symbol: icon, expanded: Binding(
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
                }.accessibilityIdentifier("item.\(item.data.id)")
            } else {
                ConversationMarkdown(blocks: item.markdown, media: media, selection: selection)
                    .foregroundColor(item.data.kind == "agent" || item.data.kind == "user" ? .primary : .secondary)
                    .accessibilityIdentifier("item.\(item.data.id)")
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .contain)
    }
}
