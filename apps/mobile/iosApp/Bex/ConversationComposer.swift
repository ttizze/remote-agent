import AgentCore
import AVFoundation
import SwiftUI
import UIKit
import UniformTypeIdentifiers

/// Composer and dictation
extension ThreadScreen {
    var composer: some View {
        let attachments = model.snapshot.draft(key: model.coreDraftKey).attachments
        return VStack(spacing: 12) {
            if model.isNewThread {
                newThreadContext
            }
            if !model.isNewThread, let review, review.files > 0 {
                Button { showingDiff = true; panel = .files; showingPanel = true } label: {
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
            VStack(alignment: .leading, spacing: 8) {
                if !attachments.isEmpty {
                    ScrollView(.horizontal) {
                        HStack(spacing: 8) {
                            ForEach(Array(attachments.enumerated()), id: \.element.path) { index, attachment in
                                Group {
                                    if attachment.isImage {
                                        ConversationImage(source: SessionImage(reference: attachment.path),
                                                          label: attachment.name,
                                                          identifier: "composer.attachment.\(index)",
                                                          media: model.mediaAccess)
                                            .frame(width: 104, height: 104).clipped()
                                    } else {
                                        Label(attachment.name, systemImage: "doc")
                                            .lineLimit(1).padding(12).padding(.trailing, 24)
                                    }
                                }
                                .background(Color(UIColor.secondarySystemBackground),
                                            in: RoundedRectangle(cornerRadius: 12))
                                .overlay(alignment: .topTrailing) {
                                    Button { model.removeAttachment(index) } label: {
                                        Image(systemName: "xmark.circle.fill")
                                            .symbolRenderingMode(.palette)
                                            .foregroundStyle(.white, .black.opacity(0.75))
                                            .font(.system(size: 24))
                                            .frame(width: 44, height: 44)
                                    }
                                    .accessibilityLabel("\(attachment.name)を外す")
                                }
                            }
                        }
                    }
                    .frame(height: attachments.contains(where: { $0.isImage }) ? 104 : 44)
                }
                Group {
                    if dictation.isRecording {
                        Canvas { context, size in
                            let spacing = size.width / CGFloat(dictation.levels.count)
                            for (index, level) in dictation.levels.enumerated() {
                                let height = 3 + CGFloat(level.squareRoot()) * (size.height - 3)
                                let bar = CGRect(x: CGFloat(index) * spacing, y: (size.height - height) / 2,
                                                 width: 3, height: height)
                                context.fill(Path(roundedRect: bar, cornerRadius: 1.5), with: .foreground)
                            }
                        }
                        .frame(height: 48)
                        .accessibilityElement(children: .ignore)
                        .accessibilityLabel("録音中")
                        .accessibilityIdentifier("dictation.recording")
                    } else if dictation.requestingPermission {
                        ProgressView("マイクの許可を確認中…")
                    } else if model.transcribing {
                        ProgressView(sendRecordedText ? "文字起こしして送信中…" : "文字起こし中…")
                            .accessibilityIdentifier("dictation.processing")
                    } else {
                        messageField
                            .font(.system(size: 18))
                            .focused($composerFocused)
                            .accessibilityIdentifier("task.message")
                    }
                }
                .padding(.horizontal, 8)
                .padding(.top, 10)
                .padding(.bottom, 4)
                HStack(spacing: 8) {
                    if dictation.isRecording || dictation.requestingPermission {
                        Button { dictation.cancel() } label: {
                            Image(systemName: "xmark").frame(width: 40, height: 40)
                        }
                        .accessibilityLabel("録音を取り消す")
                        .accessibilityIdentifier("dictation.cancel")
                    } else {
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
                        .disabled((!model.isNewThread && conversation == nil) || model
                            .transferring || preparingMedia || model.sending || model.transcribing)
                        .accessibilityLabel("添付").accessibilityIdentifier("task.attach")
                    }
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
                            Image(systemName: dictation.isRecording ? "stop.fill" : "mic")
                                .font(.system(size: dictation.isRecording ? 13 : 23))
                                .foregroundColor(dictation.isRecording ? .white : .primary)
                                .frame(width: 34, height: 34)
                                .background(dictation.isRecording ? Color(white: 0.2) : .clear, in: Circle())
                                .frame(width: 44, height: 44)
                        }
                    }
                    .disabled(!model.isConnected || (!model.isNewThread && conversation == nil) || model
                        .transcribing || dictation.requestingPermission || model.sending || model
                        .transferring || preparingMedia)
                    .accessibilityLabel(dictation.isRecording ? "録音を終了して文字起こし" : "音声をCodexで文字起こし")
                    .accessibilityIdentifier("dictation.toggle")
                    if let threadId = model.selectedThreadId,
                       let runningTurnId = model.snapshot.conversation(id: threadId)?.activeTurnId(),
                       !dictation.isRecording, !dictation.requestingPermission, !model.transcribing,
                       model.draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty, attachments
                       .isEmpty {
                        Button { model.interrupt(runningTurnId) } label: {
                            Image(systemName: "stop.fill").font(.system(size: 15))
                        }
                        .buttonStyle(ComposerSendButtonStyle())
                        .disabled(model.interruptingTurnId == runningTurnId)
                        .accessibilityLabel(model.interruptingTurnId == runningTurnId ? "停止中" : "停止")
                        .accessibilityIdentifier("turn.interrupt.\(runningTurnId)")
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
                            Image(systemName: "arrow.up").font(.system(size: 20, weight: .semibold))
                        }
                        .buttonStyle(ComposerSendButtonStyle())
                        .accessibilityLabel(dictation.isRecording ? "文字起こしして送信" : "送信")
                        .disabled(!model.isConnected || (!model.isNewThread && conversation == nil) || model
                            .sending || model.transferring || preparingMedia || dictation.requestingPermission || model
                            .transcribing ||
                            (!dictation.isRecording && model.draft.trimmingCharacters(in: .whitespacesAndNewlines)
                                .isEmpty && attachments.isEmpty))
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
        model.notice = nil
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
        BufferedTextInput(value: Binding(get: { model.draft }, set: { model.draft = $0 })) { input in
            TextField(model.isNewThread ? "メッセージを入力" : "追加の指示を入力", text: input, axis: .vertical)
                .lineLimit(1 ... 6)
        }
        .id(model.draftKey)
    }
}

private struct ComposerSendButtonStyle: ButtonStyle {
    @Environment(\.isEnabled) private var isEnabled

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .foregroundColor(.black)
            .frame(width: 36, height: 36)
            .background(.white, in: Circle())
            .opacity(isEnabled ? (configuration.isPressed ? 0.7 : 1) : 0.3)
            .frame(width: 44, height: 44)
            .contentShape(Rectangle())
    }
}

struct ThreadConversationRow: Identifiable, Sendable {
    let id: String
    let content: Content
    enum Content: Sendable {
        case historyNotice(String)
        case olderTurns, native(ConversationRow, ConversationItem?), queued(ConversationItem)
    }
}
