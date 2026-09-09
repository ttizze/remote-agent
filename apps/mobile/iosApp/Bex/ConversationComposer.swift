import AgentCore
import AVFoundation
import SwiftUI
import UIKit
import UniformTypeIdentifiers

/// Composer and dictation
extension ThreadScreen {
    var composer: some View {
        VStack(spacing: 12) {
            if state.isNewThread {
                newThreadContext
            }
            if !state.isNewThread, let review, review.files > 0 {
                Button { opensDiff = true; showingFiles = true } label: {
                    HStack(spacing: 10) {
                        Text("\(review.files)件のファイル")
                        Text("+\(review.additions)").foregroundColor(.green)
                        Text("−\(review.deletions)").foregroundColor(.red)
                    }
                    .font(.subheadline.monospacedDigit())
                }
                .buttonStyle(.bordered)
                .buttonBorderShape(.capsule)
                .accessibilityIdentifier("task.diff")
            }
            if let error = model.transferError {
                BexNotice(text: error)
            }
            if dictation.isRecording {
                Text("録音中").font(.caption).foregroundColor(.red)
                    .accessibilityIdentifier("dictation.recording")
            }
            if model.transcribing {
                ProgressView(sendRecordedText ? "文字起こしして送信中…" : "文字起こし中…").font(.caption)
                    .accessibilityIdentifier("dictation.processing")
            }
            ForEach(model.attachments) { attachment in
                HStack {
                    Label(attachment.name, systemImage: attachment.isImage ? "photo" : "doc")
                        .lineLimit(1)
                    Button { model.removeAttachment(attachment.id) } label: { Image(systemName: "xmark.circle.fill") }
                        .accessibilityLabel("\(attachment.name)を外す")
                }
                .font(.subheadline).padding(10)
                .background(Color(UIColor.secondarySystemBackground), in: Capsule())
            }
            VStack(alignment: .leading, spacing: 8) {
                messageField
                    .font(.system(size: 18))
                    .focused($composerFocused)
                    .padding(.horizontal, 8)
                    .padding(.top, 10)
                    .padding(.bottom, 4)
                    .accessibilityIdentifier("task.message")
                HStack(spacing: 8) {
                    Menu {
                        Button { composerFocused = false; showingPhotos = true } label: {
                            Label("写真・動画", systemImage: "photo.on.rectangle")
                        }.accessibilityIdentifier("task.attach.photos")
                        Button { openCamera() } label: {
                            Label("カメラ", systemImage: "camera")
                        }.accessibilityIdentifier("task.attach.camera")
                        Button { composerFocused = false; importing = true } label: {
                            Label("ファイル", systemImage: "doc")
                        }.accessibilityIdentifier("task.attach.file")
                    } label: {
                        Image(systemName: "plus").font(.title2.weight(.regular)).frame(width: 40, height: 40)
                    }
                    .disabled(model.transferring || preparingMedia || model.sending || dictation
                        .isRecording || dictation.requestingPermission || model.transcribing)
                    .accessibilityLabel("添付").accessibilityIdentifier("task.attach")
                    if model.transferring || preparingMedia {
                        ProgressView().frame(height: 40)
                    }
                    Spacer(minLength: 0)
                    Button { showingModelSettings = true } label: {
                        Image(systemName: "speedometer")
                            .font(.system(size: 23, weight: .regular))
                            .frame(width: 44, height: 44)
                    }
                    .accessibilityLabel("モデル設定")
                    .accessibilityIdentifier("model.settings")
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
                            Image(systemName: dictation.isRecording ? "stop.circle.fill" : "mic")
                                .font(.system(size: 23)).foregroundColor(dictation.isRecording ? .red : .primary)
                                .frame(width: 40, height: 40)
                        }
                    }
                    .disabled(!state.isConnected || (!state.isNewThread && conversation == nil) || model
                        .transcribing || dictation.requestingPermission || model.sending || model
                        .transferring || preparingMedia)
                    .accessibilityLabel(dictation.isRecording ? "録音を終了して文字起こし" : "音声をCodexで文字起こし")
                    .accessibilityIdentifier("dictation.toggle")
                    if let running = conversation?.turns.last(where: { $0.isInProgress }),
                       !dictation.isRecording, !dictation.requestingPermission, !model.transcribing,
                       model.draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty, model.attachments
                       .isEmpty {
                        Button { model.interrupt(running.turnId) } label: {
                            Image(systemName: "stop.fill").font(.system(size: 15))
                        }
                        .buttonStyle(.borderedProminent)
                        .buttonBorderShape(.capsule)
                        .controlSize(.large)
                        .disabled(state.interruptingTurnId == running.turnId)
                        .accessibilityLabel(state.interruptingTurnId == running.turnId ? "停止中" : "停止")
                        .accessibilityIdentifier("turn.interrupt.\(running.id)")
                    } else {
                        Button {
                            // Commit native text before Store can clear the accepted draft.
                            UIApplication.shared.sendAction(
                                #selector(UIResponder.resignFirstResponder),
                                to: nil,
                                from: nil,
                                for: nil
                            )
                            composerFocused = false
                            if dictation.isRecording {
                                sendRecordedText = true
                                dictation.finish()
                            } else {
                                model.send()
                            }
                        } label: {
                            Image(systemName: "arrow.up").font(.title2.weight(.semibold))
                        }
                        .buttonStyle(.borderedProminent)
                        .buttonBorderShape(.capsule)
                        .controlSize(.large)
                        .accessibilityLabel(dictation.isRecording ? "文字起こしして送信" : "送信")
                        .disabled(!state.isConnected || (!state.isNewThread && conversation == nil) || model
                            .sending || model.transferring || preparingMedia || dictation.requestingPermission || model
                            .transcribing ||
                            (!dictation.isRecording && model.draft.trimmingCharacters(in: .whitespacesAndNewlines)
                                .isEmpty && model.attachments.isEmpty))
                        .accessibilityIdentifier("task.send")
                    }
                }
            }
            .buttonStyle(.plain)
            .padding(7)
            .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 30))
            .overlay(RoundedRectangle(cornerRadius: 30).stroke(Color.white.opacity(0.12)))
        }
        .padding(.horizontal, 20).padding(.top, 8).padding(.bottom, 8)
        .background(LinearGradient(
            colors: [.clear, Color(UIColor.systemBackground)],
            startPoint: .top,
            endPoint: .bottom
        ))
    }

    func startDictation() {
        composerFocused = false
        model.transferError = nil
        sendRecordedText = false
        let sendIntent = $sendRecordedText
        let key = model.draftKey
        dictation.start { [weak model] result in
            guard let model, model.draftKey == key else { return }
            switch result {
            case let .success(audio): model.transcribe(audio, draftKey: key, sendImmediately: sendIntent.wrappedValue)
            case let .failure(error): model.transferError = error.localizedDescription
            }
        }
    }

    var messageField: some View {
        ConversationMessageField(
            placeholder: state.isNewThread ? "メッセージを入力" : "追加の指示を入力",
            draft: Binding(get: { model.draft }, set: { model.draft = $0 })
        )
        .id(model.draftKey)
    }

    func refreshReview() async {
        let directory = model.cwd
        let host = model.state.selectedProfileId
        guard model.state.isConnected, !directory.isEmpty,
              let threadId = conversation?.id else { review = nil; return }
        let result: Result<Outcome, Error> = await withCheckedContinuation { continuation in
            model.perform(.reviewWorkspace(cwd: directory)) { continuation.resume(returning: $0) }
        }
        guard !Task.isCancelled, model.cwd == directory, model.state.selectedThreadId == threadId,
              model.state.selectedProfileId == host else { return }
        if case .success = result, let value = model.snapshot.review() {
            review = WorkspaceReviewSummary(
                files: value.files.count,
                additions: Int(value.additions),
                deletions: Int(value.deletions)
            )
        } else {
            review = nil
        }
    }
}

/// Keep native editing ahead of the Store notification triggered by each keystroke.
/// The buffer belongs to this control; every edit still dispatches synchronously.
private struct ConversationMessageField: View {
    let placeholder: String
    @Binding private var draft: String
    @State private var text: String

    init(placeholder: String, draft: Binding<String>) {
        self.placeholder = placeholder
        _draft = draft
        _text = State(initialValue: draft.wrappedValue)
    }

    var body: some View {
        let input = Binding(get: { text }, set: {
            guard text != $0 else { return }
            text = $0
            draft = $0
        })
        Group {
            if #available(iOS 16.0, *) {
                TextField(placeholder, text: input, axis: .vertical).lineLimit(1 ... 6)
            } else {
                TextField(placeholder, text: input)
            }
        }
        .onChange(of: draft) { _ in
            // Read the latest Store value, including send clears and transcription.
            if text != draft {
                text = draft
            }
        }
    }
}

struct ScrollViewportPreferenceKey: PreferenceKey {
    static var defaultValue: CGFloat = 0
    static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) {
        value = max(value, nextValue())
    }
}

struct ThreadConversationRow: Identifiable {
    let id: String
    let content: Content
    enum Content {
        case olderTurns, olderItems(String)
        case user(ConversationItem), response(ConversationItem, String?), queued(ConversationItem)
        case activityHeader(TurnPresentation), activity(ConversationItem, String)
        case request(RequestPresentation), error(TurnErrorPresentation)
    }
}
