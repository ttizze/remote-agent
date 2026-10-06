import AgentCore
import SwiftUI

struct ConversationRequest: View {
    @ObservedObject var model: BexAppViewModel
    let row: TimelineRow
    @State private var selected: [String: Set<String>] = [:]
    @State private var custom: [String: String] = [:]
    private var answers: [QuestionAnswer] {
        row.questions.map { question in
            let values = questionAnswerValues(
                selected: Array(selected[question.id] ?? []).sorted(),
                custom: custom[question.id] ?? "",
                multi: question.multiSelect
            )
            return QuestionAnswer(questionId: question.id, values: values)
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(row.title).font(AppTheme.font(14, weight: .medium))
            if !row.text.isEmpty {
                Text(row.text).font(AppTheme.font(13)).textSelection(.enabled)
            }
            if let request = row.requestId {
                ForEach(row.choices, id: \.decision) { choice in
                    VStack(alignment: .leading, spacing: 5) {
                        if let warning = choice
                            .warning {
                            Text(warning).font(AppTheme.font(12)).foregroundStyle(AppTheme.color("warningForeground"))
                        }
                        Button(choice.label) { model.perform(.respondApproval(
                            requestId: request,
                            decision: choice.decision
                        )) }
                        .buttonStyle(.bordered).disabled(!row.actionable)
                    }
                }
                ForEach(row.questions, id: \.id) { question in
                    VStack(alignment: .leading, spacing: 7) {
                        Text(question.header).font(AppTheme.font(13, weight: .medium))
                        Text(question.question).font(AppTheme.font(14))
                        ForEach(question.options, id: \.value) { option in
                            Button {
                                if question.multiSelect {
                                    var values = selected[question.id] ?? []
                                    if values.remove(option.value) == nil {
                                        values.insert(option.value)
                                    }
                                    selected[question.id] = values
                                } else {
                                    selected[question.id] = [option.value]; custom[question.id] = ""
                                }
                            } label: {
                                HStack(alignment: .top) {
                                    Image(systemName: questionOptionSelected(
                                        selected: Array(selected[question.id] ?? []),
                                        custom: custom[question.id] ?? "",
                                        multi: question.multiSelect,
                                        value: option.value
                                    ) ? "checkmark.circle.fill" : "circle")
                                    VStack(alignment: .leading, spacing: 3) {
                                        Text(option.label).font(AppTheme.font(14))
                                        if !option.description
                                            .isEmpty {
                                            Text(option.description).font(AppTheme.font(12))
                                                .foregroundStyle(AppTheme.color("textMuted"))
                                        }
                                    }
                                }.frame(maxWidth: .infinity, alignment: .leading)
                            }.buttonStyle(.bordered).disabled(!row.actionable)
                        }
                        if question.allowCustomAnswer || question.options.isEmpty {
                            TextField(
                                "Your answer",
                                text: Binding(get: { custom[question.id] ?? "" }, set: { custom[question.id] = $0 }),
                                axis: .vertical
                            )
                            .font(AppTheme.font(14)).textFieldStyle(.roundedBorder).disabled(!row.actionable)
                        }
                    }
                }
                if row.responseModeMessage {
                    Text("Reply in the composer").font(AppTheme.font(12)).foregroundStyle(AppTheme.color("textMuted"))
                } else if !row.questions.isEmpty {
                    let error = questionError(questions: row.questions, answers: answers)
                    Button("Submit answers") { model.perform(.respondQuestions(requestId: request, answers: answers)) }
                        .buttonStyle(.borderedProminent).disabled(!row.actionable || error != nil)
                    if let error {
                        Text(error).font(AppTheme.font(12)).foregroundStyle(AppTheme.color("textMuted"))
                    }
                }
                if row
                    .responseModeMessage {
                    Button("Dismiss without answering") { model.perform(.dismissInput(requestId: request)) }
                        .font(AppTheme.font(12)).disabled(!row.actionable)
                }
            } else {
                Text(row.status).font(AppTheme.font(12)).foregroundStyle(AppTheme.color("textMuted"))
            }
        }
        .padding(14).frame(maxWidth: .infinity, alignment: .leading)
        .background(AppTheme.color("mobileGroupedCard"), in: RoundedRectangle(cornerRadius: 12))
        .overlay(RoundedRectangle(cornerRadius: 12).stroke(AppTheme.color("border")))
    }
}
