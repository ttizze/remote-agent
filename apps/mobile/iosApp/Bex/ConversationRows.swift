import RemoteAgentMobile
import SwiftUI
import UIKit

struct ThreadActivityHeader: View {
    let turn: IosTurnView
    let expanded: Bool
    var body: some View {
        HStack(spacing: 5) {
            Text(turn.activitySummary ?? "")
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
    let request: IosTurnRequestView
    let model: BexAppViewModel
    @State private var answers: [String: String] = [:]
    @State private var rawResponse = "{}"
    @State private var busy = false
    @State private var error: String?
    @State private var resolved = false

    var body: some View {
        if !resolved {
            VStack(alignment: .leading, spacing: 10) {
                Text(request.title).font(.subheadline.weight(.semibold))
                    .accessibilityIdentifier("request.\(request.id)")
                Text(request.body).textSelection(.enabled)
                DisclosureGroup("詳細") {
                    Text(request.paramsJson).font(.caption.monospaced()).textSelection(.enabled)
                }
                if request.form == "questions" {
                    let questions = request.questions
                    ForEach(questions.indices, id: \.self) { index in
                        questionView(questions[index])
                    }
                    Button("回答を送信") {
                        submit(RequestAnswerAnswers(answers: answers))
                    }
                } else if request.form == "permissions" {
                    HStack {
                        Button("このターンで許可") { submit(RequestAnswerPermissions(allow: true)) }
                        Button("拒否") { submit(RequestAnswerPermissions(allow: false)) }
                    }
                } else if request.form == "decision" {
                    VStack(alignment: .leading, spacing: 8) { decisionButtons }
                } else {
                    Text("応答 JSON").font(.caption)
                    TextEditor(text: $rawResponse).font(.body.monospaced()).frame(minHeight: 100)
                    Button("応答を送信") {
                        submit(RequestAnswerRaw(json: rawResponse))
                    }
                }
                if busy {
                    ProgressView()
                }
                if let error {
                    Text(error).foregroundColor(.red)
                }
            }
            .disabled(busy)
            .padding(10)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Color.orange.opacity(0.14))
            .clipShape(RoundedRectangle(cornerRadius: 10))
        }
    }

    private var decisionButtons: some View {
        let decisions = request.decisions
        return ForEach(decisions.indices, id: \.self) { index in
            Button(decisions[index]) { submit(RequestAnswerDecision(index: Int32(index))) }
                .accessibilityIdentifier(index == 0 ? "request.accept" : "request.decision.\(index)")
        }
    }

    @ViewBuilder private func questionView(_ question: RequestQuestion) -> some View {
        let id = question.id
        let options = question.options
        let binding = Binding<String>(get: { answers[id] ?? "" }, set: { answers[id] = $0 })
        Text(question.prompt)
        ForEach(options.indices, id: \.self) { index in
            Button(options[index]) { answers[id] = options[index] }.buttonStyle(.bordered)
        }
        if question.secret {
            SecureField("回答", text: binding)
        } else {
            TextField("回答", text: binding).textFieldStyle(.roundedBorder).accessibilityIdentifier("request.answer")
        }
    }

    private func submit(_ answer: RequestAnswer) {
        busy = true
        error = nil
        model.respond(request, answer: answer) { message in
            busy = false; error = message; resolved = message == nil
        }
    }
}

struct ThreadErrorRow: View {
    let error: IosTurnErrorView

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
    let item: IosItemView
    let isUser: Bool
    let model: BexAppViewModel
    var forkTurnId: String?
    @State private var forking = false
    @State private var forkError: String?
    @State private var sharing = false
    @State private var expanded = false
    @State private var copied = false

    var body: some View {
        VStack(alignment: isUser ? .trailing : .leading, spacing: 14) {
            VStack(alignment: isUser ? .trailing : .leading, spacing: 12) {
                let sources = item.imageSources
                ForEach(sources.indices, id: \.self) { index in
                    ConversationImage(
                        source: sources[index],
                        label: item.kind == "imageGeneration" ? "生成画像" : "添付画像",
                        identifier: "message.image.\(item.id).\(index)",
                        model: model
                    )
                }
                if !item.collapsedBody.isEmpty {
                    if isUser {
                        Text(item.collapsedBody).font(.system(size: 18)).textSelection(.enabled)
                    } else {
                        ConversationMarkdown(text: item.collapsedBody, model: model)
                    }
                }
            }
            .padding(isUser ? 14 : 0)
            .background(
                isUser ? Color(UIColor.secondarySystemBackground) : Color.clear,
                in: RoundedRectangle(cornerRadius: 22)
            )
            .padding(.leading, isUser ? 42 : 0)
            .frame(maxWidth: .infinity, alignment: isUser ? .trailing : .leading)
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier("item.\(item.id)")
            if item.kind == "agent" {
                HStack(spacing: 20) {
                    Button { UIPasteboard.general.string = item.collapsedBody; copied = true } label: {
                        Image(systemName: copied ? "checkmark" : "doc.on.doc")
                    }.accessibilityLabel(copied ? "コピーしました" : "回答をコピー")
                    Button { sharing = true } label: { Image(systemName: "square.and.arrow.up") }
                        .accessibilityLabel("回答を共有")
                    Button { expanded = true } label: { Image(systemName: "arrow.up.left.and.arrow.down.right") }
                        .accessibilityLabel("回答を広げて表示")
                    if let forkTurnId {
                        Button { fork(through: forkTurnId) } label: {
                            Image(systemName: "arrow.triangle.branch")
                        }
                        .disabled(forking)
                        .accessibilityLabel("ここから会話を分岐")
                        .accessibilityIdentifier("response.fork." + item.id)
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
        .sheet(isPresented: $sharing) { ResponseShareSheet(text: item.collapsedBody) }
        .sheet(isPresented: $expanded) {
            NavigationView {
                ScrollView { ConversationMarkdown(text: item.collapsedBody, model: model).padding(20) }
                    .navigationTitle("回答").navigationBarTitleDisplayMode(.inline)
                    .toolbar { Button("閉じる") { expanded = false } }
            }.preferredColorScheme(.dark)
        }
    }

    private func fork(through turnId: String) {
        guard !forking, let threadId = model.state.selectedThreadId,
              let host = model.state.selectedProfileId else { return }
        forking = true
        forkError = nil
        model.forkThread(threadId, through: turnId) { result, error in
            forking = false
            guard model.state.selectedProfileId == host, model.state.selectedThreadId == threadId else { return }
            if let thread = result?["thread"] as? [String: Any], let id = thread["id"] as? String {
                model.openThread(id)
            } else {
                forkError = error ?? "会話を分岐できませんでした。"
            }
        }
    }
}

private struct ResponseShareSheet: UIViewControllerRepresentable {
    let text: String
    func makeUIViewController(context _: Context) -> UIActivityViewController {
        UIActivityViewController(activityItems: [text], applicationActivities: nil)
    }

    func updateUIViewController(_: UIActivityViewController, context _: Context) {}
}

struct ThreadItemRow: View {
    let item: IosItemView
    let model: BexAppViewModel
    let isExpanded: Bool
    let toggleExpanded: () -> Void
    let loadDetails: () async -> (String?, String?)
    @State private var loadedBody: String?
    @State private var detailError: String?
    @State private var loadedVersion: String?
    @State private var retry = 0

    private var icon: String {
        switch item.kind {
        case "command": "terminal"
        case "fileChange": "doc.badge.gearshape"
        case "webSearch": "globe"
        default: "chevron.left.forwardslash.chevron.right"
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            if item.isCollapsible {
                DisclosureGroup(isExpanded: Binding(
                    get: { isExpanded },
                    set: {
                        if $0 != isExpanded {
                            toggleExpanded()
                        }
                    }
                )) {
                    if isExpanded {
                        let body = item.isDeferred ? loadedBody ?? "" : item.expandedBody()
                        if item.isDeferred, loadedBody == nil {
                            if let detailError {
                                Text(detailError).font(.caption).foregroundColor(.red)
                                Button("再読み込み") { retry += 1 }
                            } else {
                                ProgressView("詳細を読み込み中…")
                            }
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
                        Text(item.title)
                            .lineLimit(1)
                    } icon: { Image(systemName: icon) }
                        .font(.system(size: 17))
                        .foregroundColor(.secondary)
                        .padding(.vertical, 5)
                        .accessibilityIdentifier("item.\(item.id)")
                }
            } else {
                ConversationMarkdown(text: item.collapsedBody, model: model)
                    .foregroundColor(item.kind == "agent" || item.kind == "user" ? .primary : .secondary)
                    .accessibilityIdentifier("item.\(item.id)")
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .contain)
        .task(id: "\(isExpanded):\(item.contentVersion):\(retry)") {
            if loadedVersion != item.contentVersion {
                loadedBody = nil; detailError = nil
            }
            guard isExpanded, item.isDeferred, loadedBody == nil else { return }
            detailError = nil
            let (body, error) = await loadDetails()
            guard !Task.isCancelled else { return }
            loadedBody = body
            detailError = error
            loadedVersion = item.contentVersion
        }
    }
}
