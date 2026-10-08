import AgentCore
import SwiftUI

/// The thread composer: a capsule while idle, a card while editing.
struct Composer: View {
    @ObservedObject var model: BexAppViewModel
    let composer: ComposerView
    /// The new-task draft keeps the card open.
    var alwaysExpanded = false
    /// Controls between the command menu and the card (the new task's workspace and branch).
    var accessory: AnyView?
    /// Why the message cannot be sent yet, beyond the composer's own state.
    var sendBlockedReason: String?
    let openSettings: () -> Void
    @FocusState private var focused: Bool
    @State private var selection: TextSelection?
    @StateObject private var dictation = DictationRecorder()
    @State private var preparation: DictationPreparation?
    @State private var transcribing = false
    @AppStorage(ComposerEnterBehavior.storageKey) private var enterBehavior = ComposerEnterBehavior.send

    private var expanded: Bool {
        alwaysExpanded || focused
    }

    /// The Liquid Glass card's drop shadow, deeper in dark mode.
    private static let shadow = Color(uiColor: UIColor {
        UIColor.black.withAlphaComponent($0.userInterfaceStyle == .dark ? 0.35 : 0.15)
    })

    var body: some View {
        VStack(spacing: 7) {
            if composer.editingQueuedRun != nil {
                QueuedEditBanner { model.perform(.queue(action: .cancelEdit)) }
            }
            if let message = composer.attachmentError ?? composer.validationMessage {
                Text(message).font(AppTheme.font(13)).foregroundStyle(AppTheme.muted)
                    .frame(maxWidth: .infinity, alignment: .leading).padding(.horizontal, 14)
            }
            CommandPopover(model: model, text: model.composerText, cursor: cursor)
            if let accessory {
                accessory
            }
            Group {
                if expanded {
                    expandedCard
                } else {
                    collapsedCapsule
                }
            }
            .glassEffect(.regular, in: RoundedRectangle(cornerRadius: expanded ? 26 : 27))
            .shadow(color: Self.shadow, radius: 14, y: 6)
            .animation(.easeInOut(duration: 0.22), value: expanded)
        }
        .padding(.horizontal, 12)
        .padding(.top, expanded ? 8 : 6)
        .padding(.bottom, expanded ? 8 : 6)
        .background(alignment: .top) {
            LinearGradient(colors: [AppTheme.screen.opacity(0), AppTheme.screen.opacity(0.9)],
                           startPoint: .top, endPoint: .center)
                .ignoresSafeArea()
        }
        .onChange(of: model.selectedThreadId) { _, _ in cancelDictation() }
        .onChange(of: model.composerText) { _, _ in updateMenu() }
        .onChange(of: model.composerFocusRequests) { _, _ in
            focused = true
            selection = TextSelection(insertionPoint: model.composerText.endIndex)
        }
        .onChange(of: selection) { _, _ in updateMenu() }
        .onAppear(perform: updateMenu)
        .onDisappear(perform: cancelDictation)
    }

    /// Return on a hardware keyboard: the plainer chord sends as configured and
    /// adding Command sends the other way; text being composed is left alone.
    private func returnKey(_ press: KeyPress) -> KeyPress.Result {
        guard !UIResponder.isComposingText else { return .ignored }
        let command = press.modifiers.contains(.command)
        let shift = press.modifiers.contains(.shift)
        switch enterBehavior {
        case .send:
            if shift, !command {
                return .ignored
            }
            send(alternate: command)
        case .newline:
            guard command else { return .ignored }
            send(alternate: shift)
        }
        return .handled
    }

    private var editor: some View {
        TextField(
            composer.editor.placeholder,
            text: Binding(get: { model.composerText }, set: { model.editDraft($0) }),
            selection: $selection,
            axis: .vertical
        )
        .font(AppTheme.font(16))
        .focused($focused)
        .onKeyPress(.return, phases: .down, action: returnKey)
        .disabled(composer.editor.disabled)
        .accessibilityIdentifier("composer.text")
    }

    private var collapsedCapsule: some View {
        HStack(spacing: 0) {
            AttachmentButton(model: model, draftKey: composer.draftKey)
            editor.lineLimit(1).frame(height: 36).padding(.horizontal, 4)
            if !composer.attachments.isEmpty {
                HStack(spacing: 3.5) {
                    ForEach(composer.attachments.prefix(3), id: \.id) { attachment in
                        DraftAttachmentTile(attachment: attachment, size: 30, radius: 8)
                    }
                    if composer.attachments.count > 3 {
                        Text("+\(composer.attachments.count - 3)").font(AppTheme.font(12, weight: .bold))
                            .foregroundStyle(AppTheme.muted)
                            .frame(width: 30, height: 30)
                            .background(AppTheme.subtleStrong, in: RoundedRectangle(cornerRadius: 7))
                    }
                }
                .padding(.leading, 3.5)
            }
            trailingControls
        }
        .padding(.vertical, 2)
    }

    private var expandedCard: some View {
        VStack(alignment: .leading, spacing: 0) {
            if !composer.attachments.isEmpty {
                ComposerAttachmentStrip(attachments: composer.attachments, draftKey: nil, model: model)
                    .padding(.horizontal, 14).padding(.bottom, 8.75)
            }
            editor.lineLimit(3 ... 7)
                .frame(minHeight: 72, maxHeight: 160, alignment: .topLeading)
                .padding(.horizontal, 14).padding(.vertical, 4)
            Spacer().frame(height: 3.5)
            HStack(spacing: 0) {
                AttachmentButton(model: model, draftKey: composer.draftKey)
                Button(action: openSettings) {
                    HStack(spacing: 7) {
                        if let instance = composer.modelTrigger.instance {
                            Image(instance.driver.iconName).resizable().scaledToFit().frame(width: 15, height: 15)
                        }
                        Text(composer.modelTrigger.label).font(AppTheme.font(14, weight: .medium)).lineLimit(1)
                        Image(systemName: "chevron.down").font(.system(size: 10))
                    }
                    .foregroundStyle(AppTheme.text)
                    .padding(.horizontal, 7).frame(height: 38.5).frame(maxWidth: 190)
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Model and reasoning settings")
                if let limits = composerUsageLimits {
                    Button(action: openSettings) {
                        Text(limits.windows.first.map { "\($0.remainingPercent)% left" } ?? "Limits")
                            .font(AppTheme.font(12, weight: .medium))
                            .foregroundStyle(AppTheme.muted)
                            .padding(.horizontal, 5)
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel(limits.label)
                }
                if let toggle = composer.controls.interactionToggle, alwaysExpanded {
                    Button { model.perform(.setInteractionMode(mode: toggle.toggled)) } label: {
                        Label(toggle.label, systemImage: toggle.mode == .plan ? "list.bullet.clipboard" : "hammer")
                            .font(AppTheme.font(14, weight: .medium)).foregroundStyle(AppTheme.text)
                            .padding(.horizontal, 7).frame(height: 38.5)
                    }
                    .buttonStyle(.plain)
                    .accessibilityHint(toggle.tooltip)
                }
                Spacer(minLength: 0)
                trailingControls
            }
        }
        .padding(.top, 14).padding(.bottom, 6)
        .frame(minHeight: 140)
    }

    private var composerUsageLimits: ComposerUsageLimits? {
        guard let driver = composer.controls.model?.driver else { return nil }
        let provider: ProviderKind = driver == .codex ? .codex : .claude
        return model.snapshot.composerUsageLimits(provider: provider)
    }

    @ViewBuilder
    private var trailingControls: some View {
        if dictation.isRecording || transcribing {
            DictationControls(recorder: dictation, transcribing: transcribing, cancel: cancelDictation)
        } else {
            Button(action: startDictation) {
                Image(systemName: "mic").font(.system(size: 20)).frame(width: 44, height: 44)
                    .foregroundStyle(AppTheme.text)
            }
            .buttonStyle(.plain)
            .disabled(!model.isConnected || dictation.requestingPermission)
            .accessibilityLabel("Start dictation")
            if composer.mobileShowsStop {
                RoundAction(symbol: "stop.fill", danger: true, enabled: true) { model.perform(.stop) }
                    .accessibilityLabel("Stop agent")
            } else {
                sendButton
            }
        }
    }

    @ViewBuilder
    private var sendButton: some View {
        let send = composer.mobileSend
        let button = RoundAction(symbol: send.icon.symbol, danger: false, enabled: sendEnabled) {
            self.send(alternate: false)
        }
        .accessibilityLabel(send.label)
        .accessibilityHint(sendBlockedReason ?? "")
        if send.offersFollowUpChoice, let action = send.action, sendEnabled {
            button.contextMenu {
                Button { self.send(alternate: false) } label: {
                    Label(action.menuLabel, systemImage: "checkmark")
                    Text(action.menuSubtitle)
                }
                if let alternate = send.alternate {
                    Button { self.send(alternate: true) } label: {
                        Text(alternate.menuLabel)
                        Text(alternate.menuSubtitle)
                    }
                }
            }
            .accessibilityHint("Choose how to send this message")
        } else {
            button
        }
    }

    private var sendEnabled: Bool {
        if sendBlockedReason != nil {
            return false
        }
        return switch composer.primaryAction {
        case let .send(button): !button.disabled
        case let .implement(_, disabled, _): !disabled
        case let .refine(_, disabled): !disabled
        case let .answer(_, _, _, disabled): !disabled
        case .stop: false
        }
    }

    private func send(alternate: Bool) {
        guard sendEnabled else { return }
        if composer.editingQueuedRun != nil {
            model.perform(.queue(action: .saveEdit))
            return
        }
        switch composer.primaryAction {
        case .implement, .refine: model.perform(.planFollowUp(newThread: false))
        default: model.send(alternate: alternate)
        }
    }

    /// The caret as a UTF-16 offset; the end of the text when there is no selection.
    private var cursor: UInt32 {
        let text = model.composerText
        let length = text.utf16.count
        guard case let .selection(range) = selection?.indices,
              range.upperBound <= text.endIndex else { return UInt32(length) }
        return UInt32(min(length, range.upperBound.utf16Offset(in: text)))
    }

    private func updateMenu() {
        model.perform(.updateComposerMenu(text: model.composerText, cursor: cursor, layout: .mobile))
    }
}

/// Recording a dictation and handing its audio to the Host.
private extension Composer {
    func startDictation() {
        let key = composer.draftKey
        let host = model.selectedProfileId
        dictation.start(started: { preparation = model.store?.prepareDictation() }, completion: { result in
            let retained = preparation
            preparation = nil
            switch result {
            case let .success(audio):
                guard model.selectedProfileId == host else { return }
                transcribing = true
                model.perform(.transcribe(draftKey: key, preparation: retained?.id(), audio: audio)) { _ in
                    withExtendedLifetime(retained) { transcribing = false }
                }
            case let .failure(error): model.notice = error.localizedDescription
            }
        })
    }

    func cancelDictation() {
        dictation.cancel()
        preparation = nil
    }
}

extension MobileSendIcon {
    var symbol: String {
        switch self {
        case .arrowUp: "arrow.up"
        case .checkmark: "checkmark"
        case .listNumber: "list.number"
        case .arrowTurnLeftUp: "arrow.turn.left.up"
        }
    }
}

extension FollowUpBehavior {
    var menuLabel: String {
        switch self {
        case .queue: "Queue"
        case .steer: "Steer now"
        case .restart: "Restart turn"
        }
    }

    var menuSubtitle: String {
        switch self {
        case .queue: "Run after the current turn"
        case .steer: "Interrupt what the agent is doing"
        case .restart: "Start the turn over with this message"
        }
    }
}

/// A 30pt circle inside a 44pt hit area.
struct RoundAction: View {
    let symbol: String
    let danger: Bool
    let enabled: Bool
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Image(systemName: symbol).font(.system(size: 16, weight: .semibold))
                .foregroundStyle(danger ? AppTheme.dangerForeground : .white)
                .frame(width: 30, height: 30)
                .background(danger ? AppTheme.danger : AppTheme.primary.opacity(enabled ? 1 : 0.15), in: Circle())
                .frame(width: 44, height: 44)
        }
        .buttonStyle(.plain)
        .disabled(!enabled)
    }
}

private struct DictationControls: View {
    @ObservedObject var recorder: DictationRecorder
    let transcribing: Bool
    let cancel: () -> Void

    var body: some View {
        HStack(spacing: 4) {
            Button(action: cancel) { Image(systemName: "xmark").frame(width: 44, height: 44) }
                .buttonStyle(.plain)
                .accessibilityLabel("Cancel dictation")
            Text(transcribing ? "Transcribing" : "Recording").font(AppTheme.font(13)).foregroundStyle(AppTheme.muted)
            if transcribing {
                ProgressView().frame(width: 44, height: 44)
            } else {
                RoundAction(symbol: "checkmark", danger: false, enabled: true) { recorder.finish() }
                    .accessibilityLabel("Finish dictation")
            }
        }
    }
}

private struct QueuedEditBanner: View {
    let cancel: () -> Void

    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: "pencil").font(.system(size: 12))
            Text("Editing queued message").font(AppTheme.font(13))
            Spacer()
            Button("Cancel", action: cancel).font(AppTheme.font(13, weight: .medium))
                .foregroundStyle(AppTheme.primary)
                .accessibilityLabel("Cancel editing queued message")
        }
        .foregroundStyle(AppTheme.muted)
        .padding(.horizontal, 14)
    }
}
