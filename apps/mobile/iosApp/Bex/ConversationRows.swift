import AgentCore
import SwiftUI
import UIKit

struct ThreadActivityHeader: View {
    let turn: ActivityPresentation
    let expanded: Bool
    var body: some View {
        HStack(spacing: 5) {
            Text(turn.activitySummary)
                .font(.system(size: 16)).foregroundColor(.secondary)
                .lineLimit(1).truncationMode(.tail)
            if turn.activityCanCollapse {
                Image(systemName: expanded ? "chevron.down" : "chevron.right")
                    .font(.caption2.weight(.semibold)).foregroundColor(.secondary)
            }
            if turn.isInProgress {
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
            HStack(spacing: 6) {
                if error.isReconnecting {
                    ProgressView().controlSize(.small)
                }
                Text(error.title).font(.subheadline.weight(.semibold))
            }
            Text(error.message).textSelection(.enabled)
            if let details = error.details, !details.isEmpty {
                Text(details).font(.caption).foregroundColor(.secondary).textSelection(.enabled)
            }
        }
        .foregroundColor(error.isReconnecting ? .primary : .red)
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.red.opacity(error.isReconnecting ? 0.06 : 0.12))
        .clipShape(RoundedRectangle(cornerRadius: 10))
        .accessibilityIdentifier("turn.error")
    }
}

struct ThreadMessageRow: View {
    @Environment(\.colorScheme) private var colorScheme
    let item: ConversationItem
    let isUser: Bool
    let media: ConversationMediaAccess
    let selection: ConversationSelectionActions
    var fork: ((@escaping (String?) -> Void) -> Void)?
    var restoreUnknown: ((String) -> Void)?
    var discardUnknown: ((String) -> Void)?
    @State private var forking = false
    @State private var forkError: String?
    @State private var copied = false
    @State private var selectingText = false

    @ViewBuilder private var images: some View {
        let sources = item.data.imageSources
        if item.data.imagePlaceholder {
            ConversationImageSkeleton(label: "画像を生成中", identifier: "image.generation.skeleton")
        }
        let thumbnails = ForEach(sources.indices, id: \.self) { index in
            ConversationImage(source: SessionImage(reference: sources[index]),
                              label: isUser ? "添付画像" : "生成画像",
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
                        Text(text).font(.custom("DMSans-Regular", size: 16, relativeTo: .body))
                            .padding(14)
                            .background(Color(paletteRGB: colorScheme.nativePalette.userBubble),
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
            .contextMenu {
                if isUser, let text = item.data.body, !text.isEmpty {
                    Button {
                        UIPasteboard.general.string = text
                    } label: {
                        Label("コピー", systemImage: "doc.on.doc")
                    }
                    Button {
                        selectingText = true
                    } label: {
                        Label("テキストを選択", systemImage: "text.cursor")
                    }
                }
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
            if item.data.kind == "agent", let text = item.data.body {
                HStack(spacing: 20) {
                    Button { UIPasteboard.general.string = text; copied = true } label: {
                        Image(systemName: copied ? "checkmark" : "doc.on.doc")
                    }.accessibilityLabel(copied ? "コピーしました" : "回答をコピー")
                    if let fork {
                        Button {
                            forking = true; forkError = nil
                            fork { error in forking = false; forkError = error }
                        } label: {
                            Image(systemName: "arrow.triangle.branch")
                        }
                        .disabled(forking)
                        .accessibilityLabel("ここから会話を分岐")
                        .accessibilityIdentifier("response.fork." + item.data.id)
                    }
                }
                .font(.system(size: 19)).foregroundColor(.secondary).buttonStyle(.plain)
                .padding(.vertical, 4)
                if let forkError {
                    Text(forkError).font(.caption).foregroundColor(.red)
                }
            }
        }
        .padding(.bottom, isUser ? 12 : 8)
        .sheet(isPresented: $selectingText) {
            NavigationStack {
                if let text = item.data.body {
                    MessageTextSelection(text: text)
                        .navigationTitle("テキストを選択")
                        .navigationBarTitleDisplayMode(.inline)
                        .toolbar {
                            ToolbarItem(placement: .confirmationAction) {
                                Button("完了") { selectingText = false }
                            }
                        }
                }
            }
        }
    }
}

struct ThreadItemRow: View {
    let item: ConversationItem
    let media: ConversationMediaAccess
    let selection: ConversationSelectionActions
    let isExpanded: Bool
    let toggleExpanded: () -> Void
    let loadDetails: () async -> String?
    @State private var detailError: String?
    @State private var retry = 0

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
                                    Text(detailError).font(.caption).foregroundColor(.red)
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
                            Text(body).font(.system(.subheadline, design: .monospaced)).textSelection(.enabled)
                                .padding(12).frame(maxWidth: .infinity, alignment: .leading)
                                .background(
                                    Color(UIColor.secondarySystemBackground),
                                    in: RoundedRectangle(cornerRadius: 12)
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
    }
}

/// UIKit supplies selection handles and copies only the selected range.
private struct MessageTextSelection: UIViewRepresentable {
    let text: String

    func makeUIView(context _: Context) -> UITextView {
        let view = UITextView()
        view.scrollsToTop = false
        view.isEditable = false
        view.isSelectable = true
        view.font = .systemFont(ofSize: 18)
        view.textContainerInset = UIEdgeInsets(top: 16, left: 16, bottom: 16, right: 16)
        view.backgroundColor = .clear
        view.accessibilityIdentifier = "message.text-selection"
        return view
    }

    func updateUIView(_ view: UITextView, context _: Context) {
        if view.text != text {
            view.text = text
        }
    }
}
