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

struct ThreadRequestRow: View {
    let request: Request
    let respond: (Answer, @escaping (String?) -> Void) -> Void
    @State private var answers: [String: String] = [:]
    @State private var rawResponse = "{}"
    @State private var busy = false
    @State private var error: String?

    private var params: [String: JsonValue] {
        request.params
    }

    private var questions: [JsonValue] {
        params["questions"]?.array ?? []
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(request.title).font(.subheadline.weight(.semibold))
                .accessibilityIdentifier("request.\(request.key)")
            Text(request.body).textSelection(.enabled)
            DisclosureGroup("詳細") {
                Text(request.paramsJson).font(.caption.monospaced()).textSelection(.enabled)
            }
            if request.kind == .questions {
                ForEach(Array(questions.enumerated()), id: \.offset) { _, question in
                    questionView(question)
                }
                Button("回答を送信") {
                    submit(.questions(answers: answers))
                }.disabled(questions.contains { (answers[$0["id"]?.string ?? ""] ?? "").isEmpty })
            } else if request.kind == .permissions {
                HStack {
                    Button("このターンで許可") { submit(.permissions(allow: true)) }
                    Button("拒否") { submit(.permissions(allow: false)) }
                }
            } else if request.kind == .commandApproval || request.kind == .fileApproval {
                ForEach(Array(request.decisions.enumerated()), id: \.offset) { index, decision in
                    Button(request.decisionLabels[index]) { submit(.decision(index: UInt32(index))) }
                        .accessibilityIdentifier(decision
                            .string == "accept" ? "request.accept" : "request.decision.\(index)")
                }
            } else {
                Text("応答 JSON").font(.caption)
                TextEditor(text: $rawResponse).font(.body.monospaced()).frame(minHeight: 100)
                Button("応答を送信") {
                    do {
                        let value = try parseJsonValue(text: rawResponse)
                        guard case .object = value else { error = "JSON オブジェクトを入力してください"; return }
                        submit(.raw(value: value))
                    } catch { self.error = error.localizedDescription }
                }
            }
            if busy {
                ProgressView()
            }
            if let error {
                Text(error).foregroundColor(.red)
            }
        }
        .disabled(busy || !request.canRespond)
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.orange.opacity(0.14))
        .clipShape(RoundedRectangle(cornerRadius: 10))
    }

    @ViewBuilder private func questionView(_ question: JsonValue) -> some View {
        let id = question["id"]?.string ?? ""
        let binding = Binding<String>(get: { answers[id] ?? "" }, set: { answers[id] = $0 })
        Text(question["question"]?.string ?? "回答")
        if let options = question["options"]?.array {
            ForEach(Array(options.enumerated()), id: \.offset) { _, option in
                Button(option["label"]?.string ?? "") { answers[id] = option["label"]?.string }
                    .buttonStyle(.bordered)
            }
        }
        if question["isSecret"]?.bool == true {
            SecureField("回答", text: binding)
        } else {
            TextField("回答", text: binding).textFieldStyle(.roundedBorder).accessibilityIdentifier("request.answer")
        }
    }

    private func submit(_ answer: Answer) {
        busy = true
        error = nil
        respond(answer) { message in
            busy = false; error = message
        }
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
        let thumbnails = ForEach(sources.indices, id: \.self) { index in
            ConversationImage(source: SessionImage(reference: sources[index]),
                              label: item.data.kind == "imageGeneration" ? "生成画像" : "添付画像",
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
                if !item.data.body.isEmpty {
                    if isUser {
                        Text(item.data.body).font(.system(size: 18))
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
            .contextMenu {
                if isUser, !item.data.body.isEmpty {
                    Button {
                        UIPasteboard.general.string = item.data.body
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
                Text(item.data.title).font(.caption).foregroundStyle(.secondary)
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
            if item.data.kind == "agent" {
                HStack(spacing: 20) {
                    Button { UIPasteboard.general.string = item.data.body; copied = true } label: {
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
                MessageTextSelection(text: item.data.body)
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
                        Text(item.data.title)
                            .lineLimit(1)
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
