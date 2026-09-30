import AgentCore
import SwiftUI

struct ThreadRequestRow: View {
    let request: Request
    let respond: (Answer, @escaping (String?) -> Void) -> Void
    @State private var answers: [String: String] = [:]
    @State private var selections: [String: [String]] = [:]
    @State private var response = ""
    @State private var busy = false
    @State private var error: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(request.title).font(.subheadline.weight(.semibold))
                .accessibilityIdentifier("request.\(request.id)")
            Text(request.body).textSelection(.enabled)
            if !request.details.isEmpty {
                DisclosureGroup("詳細") { Text(request.details).font(.caption.monospaced()).textSelection(.enabled) }
            }
            switch request.requestBody {
            case let .approval(_, _, _, choices): choiceButtons(choices, permission: false)
            case let .permission(_, _, choices): choiceButtons(choices, permission: true)
            case let .question(questions):
                ForEach(questions, id: \.id) { question in questionView(question) }
                Button("回答を送信") {
                    let values = Dictionary(uniqueKeysWithValues: questions.map { question in
                        (
                            question.id,
                            buildQuestionAnswer(
                                multiple: question.multiple,
                                text: answers[question.id] ?? "",
                                choiceIds: selections[question.id] ?? []
                            )
                        )
                    })
                    submit(.questions(answers: values))
                }
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
                Button("確認して送信") { submitResponse() }
                HStack {
                    Button("辞退") { submit(.elicitation(action: .decline)) }
                    Button("キャンセル") { submit(.elicitation(action: .cancel)) }
                }
            case .toolExecution:
                responseEditor
                Button("実行結果を送信") { submitResponse() }
            }
            if busy {
                ProgressView()
            }
            if let error {
                Text(error).foregroundColor(.red)
            }
        }
        .onAppear { response = requestInputDefault(body: request.requestBody) }
        .disabled(busy || !request.canRespond)
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.orange.opacity(0.14))
        .clipShape(RoundedRectangle(cornerRadius: 10))
    }

    private var responseEditor: some View {
        TextEditor(text: $response).font(.body.monospaced()).frame(minHeight: 100)
    }

    private func choiceButtons(_ choices: [Choice], permission: Bool) -> some View {
        ForEach(choices, id: \.id) { choice in
            Button(choice.label) {
                submit(permission ? .permission(choiceId: choice.id) : .approval(choiceId: choice.id))
            }
            .accessibilityIdentifier(choice.id == choices.first?.id ? "request.accept" : "request.choice.\(choice.id)")
            if !choice.description.isEmpty {
                Text(choice.description).font(.caption)
            }
        }
    }

    @ViewBuilder private func questionView(_ question: Question) -> some View {
        let id = question.id
        let binding = Binding<String>(get: { answers[id] ?? "" }, set: { answers[id] = $0 })
        Text(question.prompt)
        ForEach(question.choices, id: \.id) { choice in
            Button("\((selections[id] ?? []).contains(choice.id) ? "✓ " : "")\(choice.label)") {
                var selected = selections[id] ?? []
                if question.multiple {
                    if selected.contains(choice.id) {
                        selected.removeAll { $0 == choice.id }
                    } else {
                        selected.append(choice.id)
                    }
                } else {
                    selected = [choice.id]
                }
                selections[id] = selected
                answers[id] = ""
            }.buttonStyle(.bordered)
            if !choice.description.isEmpty {
                Text(choice.description).font(.caption)
            }
        }
        if question.allowFreeText {
            if question.secret {
                SecureField("回答", text: binding)
            } else {
                TextField("回答", text: binding).textFieldStyle(.roundedBorder).accessibilityIdentifier("request.answer")
            }
        }
    }

    private func submitResponse() {
        do {
            try submit(requestAnswerFromJson(body: request.requestBody, text: response))
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func submit(_ answer: Answer) {
        busy = true
        error = nil
        respond(answer) { message in busy = false; error = message }
    }
}
