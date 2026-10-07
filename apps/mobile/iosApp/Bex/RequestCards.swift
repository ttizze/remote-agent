import AgentCore
import SwiftUI

struct RequestEyebrow: View {
    let text: String
    var color = AppTheme.muted

    var body: some View {
        Text(text.uppercased()).font(AppTheme.font(12, weight: .bold)).tracking(1.1).foregroundStyle(color)
    }
}

struct RequestActionButton: View {
    let label: String
    let tone: ApprovalTone
    var large = false
    var enabled = true
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Text(label)
                .font(AppTheme.font(14, weight: .bold))
                .foregroundStyle(foreground)
                .padding(.horizontal, large ? 14 : 12.25).padding(.vertical, large ? 12.25 : 10.5)
                .frame(maxWidth: large ? .infinity : nil)
                .background(background, in: RoundedRectangle(cornerRadius: 14))
        }
        .buttonStyle(.plain)
        .disabled(!enabled)
        .opacity(enabled ? 1 : 0.5)
    }

    private var background: Color {
        switch tone {
        case .primary: AppTheme.primary
        case .secondary: AppTheme.subtleStrong
        case .danger: AppTheme.danger
        }
    }

    private var foreground: Color {
        switch tone {
        case .primary: .white
        case .secondary: AppTheme.text
        case .danger: AppTheme.dangerForeground
        }
    }
}

struct ApprovalCard: View {
    let approval: ApprovalView
    let respond: (String) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 8.75) {
            RequestEyebrow(text: "Approval needed")
            Text(approval.cardTitle).font(AppTheme.font(18, weight: .bold)).foregroundStyle(AppTheme.text)
            if let detail = approval.cardDetail {
                Text(detail)
                    .font(approval.detailMonospace ? AppTheme.mono(13) : AppTheme.font(14))
                    .foregroundStyle(AppTheme.muted).textSelection(.enabled)
            }
            if let notice = approval.unavailableNotice {
                Text(notice).font(AppTheme.font(14)).foregroundStyle(AppTheme.muted)
            }
            FlowButtons(actions: approval.cardActions, responding: approval.responding || !approval.canRespond,
                        respond: respond)
        }
        .requestCard()
    }
}

private struct FlowButtons: View {
    let actions: [ApprovalAction]
    let responding: Bool
    let respond: (String) -> Void

    var body: some View {
        HStack(spacing: 8.75) {
            ForEach(actions, id: \.decision) { action in
                RequestActionButton(label: action.label, tone: action.tone, enabled: action.enabled && !responding) {
                    respond(action.decision)
                }
            }
        }
    }
}

extension View {
    func requestCard() -> some View {
        padding(14)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(AppTheme.cardAlt, in: RoundedRectangle(cornerRadius: 20))
            .overlay(RoundedRectangle(cornerRadius: 20).stroke(AppTheme.border))
    }
}

struct QuestionCard: View {
    @ObservedObject var model: BexAppViewModel
    let questions: QuestionsView
    @State private var expanded = true

    var body: some View {
        Group {
            if expanded {
                expandedCard
            } else {
                collapsedBar
            }
        }
        .onChange(of: questions.requestId) { _, _ in expanded = true }
    }

    private var collapsedBar: some View {
        HStack(spacing: 8) {
            Button { expanded = true } label: {
                HStack(spacing: 8) {
                    RequestEyebrow(text: "User input needed")
                    Text(questions.countLabel).font(AppTheme.font(13)).foregroundStyle(AppTheme.muted)
                    Spacer()
                    Image(systemName: "chevron.up").font(.system(size: 12)).foregroundStyle(AppTheme.muted)
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel(questions.expandAccessibilityLabel)
            Button { model.perform(.stop) } label: {
                Image(systemName: "stop.fill").font(.system(size: 12))
                    .frame(width: 31.5, height: 31.5)
                    .background(AppTheme.danger, in: Circle())
                    .foregroundStyle(AppTheme.dangerForeground)
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Stop")
        }
        .padding(.leading, 14).padding(.trailing, 5.25).padding(.vertical, 5.25)
        .background(AppTheme.cardAlt, in: Capsule())
        .overlay(Capsule().stroke(AppTheme.border))
    }

    private var expandedCard: some View {
        VStack(alignment: .leading, spacing: 8.75) {
            Button {
                UIApplication.shared.sendAction(
                    #selector(UIResponder.resignFirstResponder),
                    to: nil,
                    from: nil,
                    for: nil
                )
                expanded = false
            } label: {
                HStack(alignment: .top) {
                    VStack(alignment: .leading, spacing: 4) {
                        RequestEyebrow(text: "User input needed")
                        Text("Fill in the pending answers").font(AppTheme.font(18, weight: .bold))
                            .foregroundStyle(AppTheme.text)
                    }
                    Spacer()
                    Image(systemName: "chevron.down").font(.system(size: 13))
                        .frame(width: 28, height: 28).background(AppTheme.subtleStrong, in: Circle())
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    ForEach(questions.questions, id: \.id) { question in
                        QuestionForm(model: model, requestId: questions.requestId, question: question,
                                     enabled: questions.canRespond && !questions.responding)
                    }
                    if let notice = questions.unavailableNotice {
                        Text(notice).font(AppTheme.font(14)).foregroundStyle(AppTheme.muted)
                    }
                }
            }
            .frame(maxHeight: 420)
            .fixedSize(horizontal: false, vertical: true)
            RequestActionButton(
                label: "Submit answers", tone: questions.submitEnabled ? .primary : .secondary, large: true,
                enabled: questions.submitEnabled && !questions.responding
            ) { model.perform(.submitAnswers(requestId: questions.requestId)) }
            if questions.dismissible {
                Button("Dismiss without answering") { model.perform(.dismissInput(requestId: questions.requestId)) }
                    .buttonStyle(.plain)
                    .font(AppTheme.font(14, weight: .bold)).foregroundStyle(AppTheme.muted)
                    .frame(maxWidth: .infinity)
            }
        }
        .requestCard()
    }
}

private struct QuestionForm: View {
    @ObservedObject var model: BexAppViewModel
    let requestId: String
    let question: QuestionView
    let enabled: Bool
    @State private var custom = ""
    /// Answers this field sent since it last took core's; their echoes do not
    /// overwrite newer typing.
    @State private var sent: Set<String> = []

    var body: some View {
        let draftKey = answerDraftKey(requestId: requestId, questionId: question.id)
        VStack(alignment: .leading, spacing: 8) {
            Text(question.header.uppercased()).font(AppTheme.font(13, weight: .bold)).tracking(1)
                .foregroundStyle(AppTheme.muted)
            Text(question.question).font(AppTheme.font(16)).foregroundStyle(AppTheme.text)
            if let hint = question.hint {
                Text(hint).font(AppTheme.font(13)).foregroundStyle(AppTheme.muted)
            }
            ForEach(question.options, id: \.value) { option in
                Button {
                    // Choosing an option may clear the typed answer; show what core keeps.
                    sent.removeAll()
                    model.perform(.editAnswer(requestId: requestId, questionId: question.id,
                                              edit: .toggleOption(value: option.value)))
                } label: {
                    HStack(alignment: .top, spacing: 10) {
                        Image(systemName: symbol(option.selected)).foregroundStyle(
                            option.selected ? AppTheme.primary : AppTheme.muted
                        )
                        VStack(alignment: .leading, spacing: 2) {
                            Text(option.label).font(AppTheme.font(14, weight: .bold)).foregroundStyle(AppTheme.text)
                            if let description = option.description {
                                Text(description).font(AppTheme.font(14)).foregroundStyle(AppTheme.muted)
                            }
                        }
                        Spacer(minLength: 0)
                    }
                    .padding(.horizontal, 12.25).padding(.vertical, 10.5)
                    .frame(minHeight: 42)
                    .background(
                        option.selected ? AppTheme.primary.opacity(0.1) : AppTheme.color("mobileInputBorder")
                            .opacity(0.4),
                        in: RoundedRectangle(cornerRadius: 14)
                    )
                    .overlay(RoundedRectangle(cornerRadius: 14).stroke(
                        option.selected ? AppTheme.primary : AppTheme.border
                    ))
                }
                .buttonStyle(.plain)
                .disabled(!enabled)
            }
            let attachments = model.snapshot.draftAttachments(draftKey: draftKey)
            if !attachments.isEmpty {
                ComposerAttachmentStrip(attachments: attachments, draftKey: draftKey, model: model)
            }
            HStack(alignment: .top, spacing: 8) {
                AttachmentButton(model: model, draftKey: draftKey)
                TextField("Or type a custom answer", text: $custom, axis: .vertical)
                    .font(AppTheme.font(16))
                    .padding(12).frame(minHeight: 54, alignment: .topLeading)
                    .background(
                        AppTheme.color("mobileInputBorder").opacity(0.4),
                        in: RoundedRectangle(cornerRadius: 14)
                    )
                    .overlay(RoundedRectangle(cornerRadius: 14).stroke(AppTheme.color("mobileInputBorder")))
                    .disabled(!enabled)
                    .onChange(of: custom) { _, text in
                        guard text != question.customAnswer else { return }
                        sent.insert(text)
                        model.perform(.editAnswer(requestId: requestId, questionId: question.id,
                                                  edit: .custom(text: text)))
                    }
            }
        }
        .onAppear { custom = question.customAnswer }
        .onChange(of: question.customAnswer) { _, answer in
            guard answer != custom, !sent.contains(answer) else { return }
            sent = [answer]
            custom = answer
        }
    }

    private func symbol(_ selected: Bool) -> String {
        if question.multiple {
            return selected ? "checkmark.square.fill" : "square"
        }
        return selected ? "largecircle.fill.circle" : "circle"
    }
}
