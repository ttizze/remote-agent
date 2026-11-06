import AgentCore
import SwiftUI

/// The request the composer answers other than a question, docked above it as in T3 Code.
struct RequestPanel: View {
    let request: Request
    let busy: Bool
    let error: ErrorText?
    let respond: (Answer) -> Void
    @State private var response = ""
    @State private var inputError: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 8) {
                Label(LocalizedStringKey(request.title), systemImage: "hand.raised")
                    .font(.subheadline).foregroundStyle(.secondary)
                    .accessibilityIdentifier("request.\(request.id)")
                Spacer(minLength: 0)
                if busy {
                    ProgressView()
                }
            }
            if !request.body.isEmpty {
                Text(request.body).textSelection(.enabled)
            }
            if !request.details.isEmpty {
                DisclosureGroup("詳細") {
                    BoundedScroll(maxHeight: 200) {
                        Text(request.details).font(.caption.monospaced()).textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                }
            }
            answerControls
            if let inputError {
                AppText(inputError).font(.caption).foregroundColor(.red)
            } else if let error {
                ErrorTextView(text: error).font(.caption).foregroundColor(.red)
            }
        }
        .onAppear { response = requestInputDefault(body: request.requestBody) }
        .disabled(busy || !request.canRespond)
        .dockedPanel()
    }

    @ViewBuilder
    private var answerControls: some View {
        switch request.requestBody {
        case let .approval(_, _, _, choices): choiceButtons(choices) { .approval(choiceId: $0) }
        case let .permission(_, _, choices): choiceButtons(choices) { .permission(choiceId: $0) }
        // The question panel answers questions.
        case .question: EmptyView()
        case let .elicitation(_, _, input):
            if case let .url(url) = input {
                if let url = URL(string: url) {
                    Link("リンクを開く", destination: url)
                }
            } else {
                if case let .form(fields) = input {
                    ForEach(fields, id: \.name) { field in
                        Text("\(field.title) (\(field.name))\(field.required ? " *" : "")")
                        if !field.description.isEmpty {
                            Text(field.description).font(.caption)
                        }
                    }
                }
                responseEditor
            }
            FlowLayout {
                Button("確認して送信") { submitResponse() }.requestButtonStyle(.primary)
                Button("辞退") { respond(.elicitation(action: .decline)) }.requestButtonStyle(.danger)
                Button("キャンセル") { respond(.elicitation(action: .cancel)) }
                    .requestButtonStyle(.secondary)
            }
        case .toolExecution:
            responseEditor
            Button("実行結果を送信") { submitResponse() }.requestButtonStyle(.primary)
        }
    }

    private var responseEditor: some View {
        TextEditor(text: $response).font(.body.monospaced()).frame(minHeight: 100, maxHeight: 200)
    }

    private func choiceButtons(_ choices: [Choice], answer: @escaping (String) -> Answer) -> some View {
        // Choices sharing a description, such as the scopes of one permission, explain it once.
        let notes = choices.map(\.description).filter { !$0.isEmpty }
            .reduce(into: [String]()) { notes, note in
                if !notes.contains(note) {
                    notes.append(note)
                }
            }
        return VStack(alignment: .leading, spacing: 8) {
            FlowLayout {
                ForEach(choices, id: \.id) { choice in
                    Button(choice.label) { respond(answer(choice.id)) }
                        .requestButtonStyle(choiceTone(effect: choice.effect))
                        .accessibilityIdentifier(
                            choice.id == choices.first?.id ? "request.accept" : "request.choice.\(choice.id)"
                        )
                }
            }
            ForEach(notes, id: \.self) { Text($0).font(.caption).foregroundStyle(.secondary) }
        }
    }

    private func submitResponse() {
        do {
            let answer = try requestAnswerFromJson(body: request.requestBody, text: response)
            inputError = nil
            respond(answer)
        } catch {
            inputError = error.localizedDescription
        }
    }
}

extension View {
    /// Request actions in the tone the core ranks them.
    @ViewBuilder func requestButtonStyle(_ tone: ActionTone) -> some View {
        switch tone {
        case .primary: buttonStyle(.borderedProminent).controlSize(.large)
        case .danger: buttonStyle(.bordered).tint(.red).controlSize(.large)
        case .secondary: buttonStyle(.bordered).tint(.primary).controlSize(.large)
        }
    }
}

/// Wraps actions onto new rows when they do not fit on one.
struct FlowLayout: Layout {
    var spacing: CGFloat = 10

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache _: inout ()) -> CGSize {
        let frames = frames(subviews, width: proposal.width ?? .infinity)
        return CGSize(
            width: proposal.width ?? frames.map(\.maxX).max() ?? 0,
            height: frames.map(\.maxY).max() ?? 0
        )
    }

    func placeSubviews(in bounds: CGRect, proposal _: ProposedViewSize, subviews: Subviews, cache _: inout ()) {
        for (subview, frame) in zip(subviews, frames(subviews, width: bounds.width)) {
            subview.place(
                at: CGPoint(x: bounds.minX + frame.minX, y: bounds.minY + frame.minY),
                proposal: ProposedViewSize(frame.size)
            )
        }
    }

    private func frames(_ subviews: Subviews, width: CGFloat) -> [CGRect] {
        var frames: [CGRect] = []
        var origin = CGPoint.zero
        var rowHeight: CGFloat = 0
        for subview in subviews {
            let size = subview.sizeThatFits(ProposedViewSize(width: width.isFinite ? width : nil, height: nil))
            if origin.x > 0, origin.x + size.width > width {
                origin = CGPoint(x: 0, y: origin.y + rowHeight + spacing)
                rowHeight = 0
            }
            frames.append(CGRect(origin: origin, size: size))
            origin.x += size.width + spacing
            rowHeight = max(rowHeight, size.height)
        }
        return frames
    }
}

/// Natural height, scrolling only when the content exceeds the limit, so a
/// docked panel never pushes the conversation away. With less room on screen,
/// such as above the keyboard, it shrinks and scrolls rather than squeezing
/// the text beside it.
struct BoundedScroll<Content: View>: View {
    let maxHeight: CGFloat
    @ViewBuilder let content: Content
    @State private var height: CGFloat = 0

    var body: some View {
        ScrollView {
            content.onGeometryChange(for: CGFloat.self) { $0.size.height } action: { height = $0 }
        }
        .scrollBounceBehavior(.basedOnSize)
        .frame(maxHeight: min(height, maxHeight))
    }
}

extension View {
    /// The card shared by the panels docked above the composer.
    func dockedPanel() -> some View {
        padding(16)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Color(uiColor: .secondarySystemGroupedBackground))
            .clipShape(RoundedRectangle(cornerRadius: 24))
            .overlay(RoundedRectangle(cornerRadius: 24).strokeBorder(Color.primary.opacity(0.15), lineWidth: 1))
    }
}

/// The question the composer answers, docked above it as in T3 Code.
struct QuestionPanel: View {
    let prompt: QuestionPrompt
    let busy: Bool
    let error: ErrorText?
    let choose: (String) -> Void
    let skip: () -> Void
    /// Collapsing applies to one question; the next question opens again.
    @State private var collapsedQuestionId: String?

    var body: some View {
        let question = prompt.question
        let expanded = collapsedQuestionId != question.id
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 8) {
                Label(LocalizedStringKey(prompt.status ?? "質問"), systemImage: "questionmark.bubble")
                    .font(.subheadline).foregroundStyle(.secondary)
                    .accessibilityIdentifier("request.\(prompt.requestId)")
                if !question.header.isEmpty {
                    Text(question.header).font(.subheadline.weight(.semibold)).lineLimit(1)
                }
                Spacer(minLength: 0)
                if let progress = prompt.progress {
                    Text(progress).font(.caption.monospacedDigit()).foregroundStyle(.secondary)
                }
                if busy {
                    ProgressView()
                }
                Button {
                    collapsedQuestionId = expanded ? question.id : nil
                } label: {
                    Image(systemName: expanded ? "chevron.down" : "chevron.up")
                        .font(.body.weight(.medium))
                        .foregroundStyle(.secondary)
                        .frame(width: 44, height: 44)
                        .contentShape(Rectangle())
                }
                .accessibilityLabel(LocalizedStringKey(expanded ? "質問を閉じる" : "質問を開く"))
                .accessibilityIdentifier("request.toggle")
            }
            if expanded {
                // The question scrolls with its choices between the pinned header and actions, as in T3 Code.
                BoundedScroll(maxHeight: 360) {
                    VStack(alignment: .leading, spacing: 12) {
                        Text(question.prompt).textSelection(.enabled)
                        if question.multiple {
                            Text("複数選択できます").font(.caption).foregroundStyle(.secondary)
                        }
                        choices(question)
                    }
                }
                // Each question opens at its start.
                .id("\(prompt.requestId)/\(question.id)")
                HStack {
                    Spacer(minLength: 0)
                    Button("スキップする", action: skip)
                        .buttonStyle(.bordered)
                        .buttonBorderShape(.capsule)
                        .tint(.primary)
                        .controlSize(.large)
                        .accessibilityIdentifier("request.skip")
                }
            } else {
                Text(question.prompt).lineLimit(1).foregroundStyle(.secondary)
            }
            if let error {
                ErrorTextView(text: error).font(.caption).foregroundColor(.red)
            }
        }
        .disabled(busy || !prompt.canRespond)
        .buttonStyle(.plain)
        .dockedPanel()
    }

    private func choices(_ question: Question) -> some View {
        VStack(spacing: 0) {
            ForEach(question.choices, id: \.id) { choice in
                let chosen = prompt.chosenChoiceIds.contains(choice.id)
                Button { choose(choice.id) } label: {
                    choiceLabel(label: choice.label, description: choice.description, chosen: chosen)
                }
                .accessibilityLabel(choice.label)
                .accessibilityHint(choice.description)
                .accessibilityValue(chosen ? "選択済み" : "未選択")
                .accessibilityAddTraits(chosen ? [.isSelected] : [])
                .accessibilityIdentifier("request.choice.\(choice.id)")
                if choice.id != question.choices.last?.id {
                    Divider()
                }
            }
        }
    }

    private func choiceLabel(label: String, description: String, chosen: Bool) -> some View {
        HStack(alignment: .center, spacing: 12) {
            ZStack {
                Circle().strokeBorder(Color.primary.opacity(0.2), lineWidth: 2)
                if chosen {
                    Circle().fill(Color.primary)
                    Image(systemName: "checkmark")
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(Color(uiColor: .systemBackground))
                }
            }
            .frame(width: 24, height: 24)
            .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 4) {
                Text(label).font(.body)
                if !description.isEmpty {
                    Text(description).font(.caption).foregroundStyle(.secondary)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .padding(.vertical, 12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .contentShape(Rectangle())
    }
}

/// Composer controls while a question awaits an answer.
extension ConversationComposer {
    /// While a question awaits an answer, the primary action answers it.
    @ViewBuilder
    func questionActions(_ question: QuestionPrompt, runningTurnId: String?) -> some View {
        if let runningTurnId {
            ComposerStopButton(turnId: runningTurnId, interrupting: model.interruptingTurnId == runningTurnId,
                               stop: model.interrupt)
        }
        if question.canGoBack {
            Button("前へ") { model.answerQuestion(question.requestId, .back) }
                .font(.subheadline.weight(.semibold))
                .padding(.horizontal, 12)
                .frame(minHeight: 44)
                .accessibilityIdentifier("request.back")
        }
        Button {
            // Commit native text before the answer is read.
            UIApplication.shared.sendAction(#selector(UIResponder.resignFirstResponder), to: nil, from: nil, for: nil)
            composerFocused = false
            model.answerQuestion(question.requestId, .advance)
        } label: {
            AppText(question.action).font(.subheadline.weight(.semibold))
        }
        .buttonStyle(.borderedProminent)
        .buttonBorderShape(.capsule)
        .disabled(!question.canAdvance || !model.isConnected
            || model.composer.requestPhase(id: question.requestId) == .running)
        .accessibilityIdentifier("request.send")
    }

    /// The composer text is the active question's own answer.
    func answerField(_ question: QuestionPrompt) -> some View {
        // Read the published answer, not this render's copy, as the draft field does.
        BufferedTextInput(value: Binding(get: { model.questionPrompt?.text ?? "" }, set: {
            model.answerQuestion(question.requestId, .edit(text: $0))
        })) { input in
            Group {
                if question.question.secret {
                    SecureField(question.placeholder, text: input)
                } else {
                    TextField(question.placeholder, text: input, axis: .vertical)
                        .lineLimit(1 ... 6)
                }
            }
            .focused($composerFocused)
            .disabled(!question.question.allowFreeText || !question.canRespond)
            .accessibilityIdentifier("task.message")
        }
        .id("\(question.requestId)/\(question.question.id)")
    }
}
