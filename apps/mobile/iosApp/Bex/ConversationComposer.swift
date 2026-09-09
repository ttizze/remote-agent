import AVFoundation
import RemoteAgentMobile
import SwiftUI
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
                    .disabled(!state.isConnected || (!state.isNewThread && conversation.thread == nil) || model
                        .transcribing || dictation.requestingPermission || model.sending || model
                        .transferring || preparingMedia)
                    .accessibilityLabel(dictation.isRecording ? "録音を終了して文字起こし" : "音声をCodexで文字起こし")
                    .accessibilityIdentifier("dictation.toggle")
                    if let running = conversation.thread?.turns.last(where: { $0.isInProgress }),
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
                            if dictation.isRecording {
                                sendRecordedText = true
                                dictation.finish()
                            } else {
                                model.send()
                            }
                            composerFocused = false
                        } label: {
                            Image(systemName: "arrow.up").font(.title2.weight(.semibold))
                        }
                        .buttonStyle(.borderedProminent)
                        .buttonBorderShape(.capsule)
                        .controlSize(.large)
                        .accessibilityLabel(dictation.isRecording ? "文字起こしして送信" : "送信")
                        .disabled(!state.isConnected || (!state.isNewThread && conversation.thread == nil) || model
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

    @ViewBuilder var messageField: some View {
        let binding = Binding(get: { model.draft }, set: { model.draft = $0 })
        TextField(state.isNewThread ? "メッセージを入力" : "追加の指示を入力", text: binding, axis: .vertical).lineLimit(1 ... 6)
    }

    func refreshReview() {
        let directory = model.cwd
        guard !directory.isEmpty, let threadId = conversation.thread?.id else { review = nil; return }
        model.workspace.reviewWorkspace(cwd: directory) { result, _ in
            guard model.cwd == directory, model.state.selectedThreadId == threadId else { return }
            if let result {
                review = WorkspaceReviewSummary(
                    files: result.files.count,
                    additions: Int(result.additions),
                    deletions: Int(result.deletions)
                )
            } else {
                review = nil
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
        case user(IosItemView), response(IosItemView, String?), queued(IosItemView)
        case activityHeader(IosTurnView), activity(IosItemView, String)
        case request(IosTurnRequestView), error(IosTurnErrorView)
    }
}
