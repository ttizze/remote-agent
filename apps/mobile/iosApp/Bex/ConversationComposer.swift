import AgentCore
import SwiftUI
import UIKit

/// Owns native input, dictation and attachment presentation independently of history.
struct ConversationComposer: View {
    let model: ConversationModel
    let mediaPresentation: ConversationMediaPresentation
    let hasConversation: Bool
    let isSideChat: Bool
    let openTools: ((PanelKind?) -> Void)?
    @StateObject var dictation = DictationRecorder()
    @State var sendRecordedText = false
    @State var mediaPicker: ComposerMediaPicker.Kind?
    @State var preparingMedia = false
    @State var showingModelSettings = false
    @State var openedDirectory: WorkspaceDestination?
    @FocusState var composerFocused: Bool

    var body: some View {
        composer
            .onDisappear { dictation.cancel() }
            .onChange(of: model.composerFocusRequest) { _ in
                composerFocused = true
            }
            .onChange(of: model.draftKey) { dictation.cancel(); openedDirectory = nil }
            .onChange(of: model.isConnected) {
                if !$0 {
                    dictation.cancel()
                }
            }
            .background(ComposerMediaPicker(model: model, selection: $mediaPicker,
                                            preparingMedia: $preparingMedia))
            .sheet(isPresented: $showingModelSettings) { ModelSettingsSheet(model: model) }
            .sheet(item: $openedDirectory) { destination in
                ConversationDirectorySheet(model: model, destination: destination) { openedDirectory = nil }
            }
            .onAppear {
                if model.isNewThread {
                    composerFocused = true
                }
            }
            .onChange(of: model.isNewThread) {
                if $0 {
                    composerFocused = true
                }
            }
    }

    var mediaAccess: ConversationMediaAccess {
        model.mediaAccess(presentation: mediaPresentation) { openedDirectory = $0 }
    }

    var composer: some View {
        let controls = model.composerControls
        let attachments = model.composer.draft().attachments
        let question = model.questionPrompt
        let runningTurnId = controls.runningTurn
        // Dictation in progress finishes into the draft before the composer answers.
        let answering = dictation.isRecording || dictation.requestingPermission || model.transcribing ? nil : question
        // An idle reply collapses to one row; focus expands it without recreating the field.
        let collapsed = !model.isNewThread && !composerFocused && model.draft.isEmpty && attachments.isEmpty
            && answering == nil && !dictation.isRecording && !dictation.requestingPermission && !model.transcribing
            && !model.transferring && !preparingMedia
        return VStack(spacing: 12) {
            if !isSideChat, !model.isNewThread, let review = model.files.review(), review.fileCount() > 0 {
                Button { openTools?(.diff) } label: {
                    HStack(spacing: 10) {
                        Text("\(review.fileCount())件のファイル").foregroundStyle(.tint)
                        Text("+\(review.additions())").foregroundColor(.green)
                        Text("−\(review.deletions())").foregroundColor(.red)
                    }
                    .font(.subheadline.monospacedDigit())
                }
                .buttonStyle(.glass)
                .buttonBorderShape(.capsule)
                .accessibilityIdentifier("task.diff")
            }
            if let error = model.transferError {
                AppNotice(text: error)
            }
            if let request = model.dockedRequest {
                RequestPanel(
                    request: request,
                    busy: model.composer.requestPhase(id: request.id) == .running,
                    error: model.composer.requestError(id: request.id),
                    respond: { model.respond(request.id, $0) }
                )
                // Typed responses belong to one request.
                .id(request.id)
            }
            if let question {
                QuestionPanel(
                    prompt: question,
                    busy: model.composer.requestPhase(id: question.requestId) == .running,
                    error: model.composer.requestError(id: question.requestId),
                    choose: { model.answerQuestion(question.requestId, .choose(choiceId: $0)) },
                    skip: { model.respond(question.requestId, .skipQuestions) }
                )
            }
            VStack(alignment: .leading, spacing: 4) {
                // A question's answer has no attachments; the draft keeps them.
                if answering == nil, !attachments.isEmpty {
                    ComposerAttachmentStrip(attachments: attachments, media: mediaAccess,
                                            remove: model.removeAttachment)
                }
                HStack(spacing: 0) {
                    if collapsed {
                        attachMenu
                    }
                    Group {
                        if dictation.isRecording {
                            DictationWaveform(levels: dictation.levels)
                        } else if dictation.requestingPermission {
                            ProgressView("マイクの許可を確認中…")
                        } else if model.transcribing {
                            ProgressView(sendRecordedText ? "文字起こしして送信中…" : "文字起こし中…")
                                .accessibilityIdentifier("dictation.processing")
                        } else if let answering {
                            answerField(answering)
                                .font(.body)
                        } else {
                            ComposerMessageField(model: model, composerFocused: $composerFocused)
                                .font(.body)
                        }
                    }
                    .padding(.horizontal, 8)
                    .padding(.top, collapsed ? 0 : 6)
                    if collapsed {
                        trailingActions(answering: nil, runningTurnId: runningTurnId, hasAttachments: false)
                    }
                }
                if !collapsed {
                    controlRow(answering: answering, runningTurnId: runningTurnId,
                               hasAttachments: !attachments.isEmpty)
                }
            }
            .buttonStyle(.plain)
            .padding(7)
            .glassEffect(.regular, in: .rect(cornerRadius: 26))
        }
        .padding(.horizontal, 12)
    }

    /// The expanded composer's attachment, model, and primary actions.
    func controlRow(answering: QuestionPrompt?, runningTurnId: String?, hasAttachments: Bool) -> some View {
        HStack(spacing: 0) {
            if dictation.isRecording || dictation.requestingPermission {
                Button { dictation.cancel() } label: {
                    Image(systemName: "xmark").frame(width: 40, height: 40)
                }
                .accessibilityLabel("録音を取り消す")
                .accessibilityIdentifier("dictation.cancel")
            } else if answering == nil {
                attachMenu
            }
            if model.transferring || preparingMedia {
                ProgressView().frame(height: 40)
            }
            Spacer(minLength: 0)
            ComposerModelControls(model: model, composerFocused: $composerFocused,
                                  showingModelSettings: $showingModelSettings)
            trailingActions(answering: answering, runningTurnId: runningTurnId, hasAttachments: hasAttachments)
        }
    }

    var attachMenu: some View {
        ComposerAttachMenu(
            disabled: (!model.isNewThread && !hasConversation) || model.transferring
                || preparingMedia || model.sending || model.transcribing,
            choose: { composerFocused = false; mediaPicker = $0 },
            camera: openCamera
        )
    }

    /// Dictation and the primary action, which answers an awaiting question.
    @ViewBuilder
    func trailingActions(answering: QuestionPrompt?, runningTurnId: String?, hasAttachments: Bool) -> some View {
        if let answering {
            questionActions(answering, runningTurnId: runningTurnId)
        } else {
            dictationButton
            if let runningTurnId,
               !dictation.isRecording, !dictation.requestingPermission, !model.transcribing,
               model.draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty, !hasAttachments {
                ComposerStopButton(turnId: runningTurnId, interrupting: model.interruptingTurnId == runningTurnId,
                                   stop: model.interrupt)
            } else {
                sendButton(hasAttachments: hasAttachments)
            }
        }
    }

    var dictationButton: some View {
        Button {
            if dictation.isRecording {
                dictation.finish()
            } else {
                startDictation()
            }
        } label: {
            if dictation.requestingPermission {
                ProgressView().frame(width: 40, height: 40)
            } else {
                Image(systemName: dictation.isRecording ? "stop.fill" : "mic")
                    .font(dictation.isRecording ? .body : .title2)
                    .foregroundStyle(dictation.isRecording ? .red : .primary)
                    .frame(width: 34, height: 34)
                    .background(dictation.isRecording ? Color.red.opacity(0.12) : .clear, in: Circle())
                    .frame(width: 44, height: 44)
            }
        }
        .disabled(!model.isConnected || (!model.isNewThread && !hasConversation) || model
            .transcribing || dictation.requestingPermission || model.sending || model
            .transferring || preparingMedia)
        .accessibilityLabel(LocalizedStringKey(dictation.isRecording ? "録音を終了して文字起こし" : "音声をCodexで文字起こし"))
        .accessibilityIdentifier("dictation.toggle")
    }

    func sendButton(hasAttachments: Bool) -> some View {
        Button {
            // Commit native text before Store can clear the accepted draft.
            UIApplication.shared.sendAction(#selector(UIResponder.resignFirstResponder), to: nil, from: nil, for: nil)
            composerFocused = false
            if dictation.isRecording {
                sendRecordedText = true
                dictation.finish()
            } else {
                model.send()
            }
        } label: {
            Image(systemName: "arrow.up").font(.system(size: 16, weight: .semibold))
        }
        .buttonStyle(.borderedProminent)
        .buttonBorderShape(.circle)
        .accessibilityLabel(LocalizedStringKey(dictation.isRecording ? "文字起こしして送信" : "送信"))
        .disabled(!model.isConnected || (!model.isNewThread && !hasConversation) || model
            .composerControls.provider == nil || model
            .sending || model.transferring || preparingMedia || dictation.requestingPermission || model
            .transcribing ||
            (!dictation.isRecording && model.draft.trimmingCharacters(in: .whitespacesAndNewlines)
                .isEmpty && !hasAttachments))
        .accessibilityIdentifier("task.send")
    }

    func startDictation() {
        composerFocused = false
        model.transferError = nil
        model.notice = nil
        sendRecordedText = false
        let sendIntent = $sendRecordedText
        let key = model.draftKey
        var preparation: DictationPreparation?
        dictation.start(started: { [weak model] in
            preparation = model?.prepareDictation()
        }, completion: { [weak model] result in
            guard let model, model.draftKey == key else { return }
            switch result {
            case let .success(audio): model.transcribe(audio, draftKey: key, sendImmediately: sendIntent.wrappedValue,
                                                       preparation: preparation)
            case let .failure(error): model.transferError = error.localizedDescription
            }
        })
    }
}
