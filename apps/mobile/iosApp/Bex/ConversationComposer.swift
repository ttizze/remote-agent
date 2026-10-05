import AgentCore
import SwiftUI

struct ConversationComposer: View {
    @ObservedObject var model: BexAppViewModel
    let showQueue: () -> Void
    let showAgents: () -> Void
    @FocusState private var focused: Bool
    @StateObject private var recorder = DictationRecorder()
    @State private var preparation: DictationPreparation?
    @State private var transcribing = false
    private var selectedModel: Model? {
        model.snapshot.modelChoices().first { $0.selected }?.model
    }

    var body: some View {
        let composer = model.conversation.composer
        let draft = model.snapshot.draft()
        VStack(spacing: 8) {
            if let label = model.conversation.agents.pillLabel {
                Button("Agents \(label)", action: showAgents).font(T3Theme.font(12))
                    .accessibilityLabel(model.conversation.agents.accessibilityLabel ?? "Agents")
            }
            if composer.queueCount > 0 {
                Button(action: showQueue) {
                    HStack {
                        Image(systemName: "text.badge.plus"); Text(
                            "\(composer.queueCount) queued\(composer.queueHeld ? " · paused" : "")"
                        ); Spacer(); Image(systemName: "chevron.up")
                    }
                    .font(T3Theme.font(12)).foregroundStyle(T3Theme.color("textMuted"))
                }.padding(.horizontal, 4)
            }
            if composer.editing {
                HStack {
                    Text("Editing queued message").font(T3Theme.font(12))
                    Spacer()
                    Button("Cancel") { model.perform(.queue(action: .cancelEdit)) }.font(T3Theme.font(12))
                }
            }
            ForEach(composer.pendingDeliveries, id: \.self) { id in
                Button("Delivery unconfirmed · Stop retrying") { model.perform(.discardPending(commandId: id)) }
            }
            if let notice = composer.notice {
                Text(notice).font(T3Theme.font(12)).foregroundStyle(T3Theme.color("textMuted"))
            }
            VStack(alignment: .leading, spacing: 12) {
                if !draft.attachments.isEmpty {
                    ConversationAttachmentStrip(
                        attachments: draft.attachments,
                        download: model.downloadAttachment,
                        perform: { model.perform($0) }
                    )
                }
                TextField(
                    composer.placeholder,
                    text: Binding(get: { model.composerText }, set: { model.editDraft($0) }),
                    axis: .vertical
                )
                .lineLimit(2 ... 8).font(T3Theme.font(16)).focused($focused).textInputAutocapitalization(.sentences)
                .accessibilityIdentifier("composer.text").disabled(!composer.canEdit)
                HStack(spacing: 12) {
                    ComposerAttachmentButton(model: model)
                    Menu {
                        ForEach(model.snapshot.modelChoices(), id: \.model.id) { choice in
                            let value = choice.model
                            Button(value.displayName) { select(
                                value,
                                instanceId: choice.instanceId,
                                effort: value.defaultReasoningEffort,
                                tier: value.defaultServiceTier
                            ) }
                        }
                        if let selectedModel {
                            Divider()
                            ForEach(selectedModel.supportedReasoningEfforts, id: \.reasoningEffort) { effort in
                                Button(effort.reasoningEffort) { select(
                                    selectedModel,
                                    instanceId: draft.instanceId,
                                    effort: effort.reasoningEffort,
                                    tier: draft.serviceTier
                                ) }
                            }
                            ForEach(selectedModel.serviceTiers ?? [], id: \.id) { tier in
                                Button(tier.name ?? tier.id) {
                                    select(
                                        selectedModel,
                                        instanceId: draft.instanceId,
                                        effort: draft.effort,
                                        tier: tier.id
                                    )
                                }
                            }
                        }
                    } label: {
                        HStack(spacing: 4) {
                            Image(composer.providerKind == .claude ? "claude" : "openai").resizable()
                                .scaledToFit().frame(
                                    width: 13,
                                    height: 13
                                )
                            Text(selectedModel?.displayName ?? draft.model).lineLimit(1)
                            Image(systemName: "chevron.down").font(.system(size: 8))
                        }.font(T3Theme.font(12))
                    }
                    Menu {
                        ForEach(runtimeModeChoices(), id: \.id) { mode in
                            Button { model.perform(.setRuntimeMode(mode: mode.id)) } label: { Label(
                                mode.label,
                                systemImage: draft.runtimeMode == mode.id ? "checkmark" : "circle"
                            ) }
                        }
                        Divider()
                        ForEach(interactionModeChoices(), id: \.id) { mode in
                            Button(mode.label) { model.perform(.setInteractionMode(mode: mode.id)) }
                        }
                    } label: {
                        Image(systemName: draft
                            .interactionMode == "plan" ? "list.bullet.clipboard" : "slider.horizontal.3")
                    }
                    Spacer(minLength: 0)
                    if recorder.isRecording {
                        Button { recorder.cancel(); preparation = nil } label: { Image(systemName: "xmark") }
                        Button { recorder.finish() } label: { Image(systemName: "checkmark.circle.fill") }
                    } else if transcribing || recorder.requestingPermission {
                        ProgressView().controlSize(.small)
                    } else {
                        Button(action: startRecording) { Image(systemName: "mic") }.disabled(!model.isConnected)
                    }
                    if composer
                        .canStop {
                        Button { model.perform(.stop) } label: { Image(systemName: "stop.fill") }
                            .accessibilityLabel("Stop")
                    }
                    if composer.canSteer || composer.canRestart {
                        Menu {
                            if composer.canSteer {
                                Button("Steer") { model.send(.steer) }
                            }
                            if composer.canRestart {
                                Button("Restart with message") { model.send(.restart) }
                            }
                        } label: { Image(systemName: "arrow.triangle.branch") }.disabled(!composer.enabled)
                    }
                    Button {
                        if composer.editing {
                            model.perform(.queue(action: .saveEdit))
                        } else {
                            model.send()
                        }
                    } label: { Image(systemName: "arrow.up").font(.system(size: 16, weight: .semibold)).frame(
                        width: 32,
                        height: 32
                    ) }
                    .background(T3Theme.color("text"), in: Circle()).foregroundStyle(T3Theme.color("canvas"))
                    .disabled(!composer.enabled).opacity(composer.enabled ? 1 : 0.35)
                    .accessibilityLabel(composer.sendLabel)
                }.foregroundStyle(T3Theme.color("textMuted"))
            }
            .padding(14).background(
                T3Theme.color("mobileComposer").opacity(0.9),
                in: RoundedRectangle(cornerRadius: 18)
            )
            .overlay(RoundedRectangle(cornerRadius: 18).stroke(T3Theme.color("border").opacity(0.8)))
        }
        .padding(.horizontal, 14).padding(.vertical, 10).background(T3Theme.color("canvas"))
        .onChange(of: model.selectedThreadId) { _, _ in recorder.cancel(); preparation = nil }
        .onDisappear { recorder.cancel(); preparation = nil }
    }

    private func select(_ value: Model, instanceId: String, effort: String?, tier: String?) {
        model.perform(.setModel(
            instanceId: instanceId,
            model: value.model.id,
            effort: effort,
            serviceTier: tier
        ))
    }

    private func startRecording() {
        let key = model.snapshot.currentDraftKey()
        let host = model.selectedProfileId
        recorder.start(started: { preparation = model.store?.prepareDictation() }, completion: { result in
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
}

struct QueueSheet: View {
    @ObservedObject var model: BexAppViewModel
    @Environment(\.dismiss) private var dismiss
    var body: some View {
        NavigationStack {
            List {
                if model.conversation.composer
                    .queueHeld {
                    Button("Resume queue") { model.perform(.queue(action: .resume)) }
                }
                ForEach(model.conversation.queue, id: \.runId) { row in
                    VStack(alignment: .leading, spacing: 8) {
                        Text(row.text).font(T3Theme.font(14)).lineLimit(5)
                        Text(row.model).font(T3Theme.font(11)).foregroundStyle(T3Theme.color("textMuted"))
                        HStack {
                            if row
                                .canEdit {
                                Button("Edit") { model.perform(.queue(action: .edit(runId: row.runId))); dismiss() }
                            }
                            if row
                                .canSteer {
                                Button("Steer") { model.perform(.queue(action: .steer(runId: row.runId))) }
                            }
                            Button("Cancel", role: .destructive) {
                                model.perform(.queue(action: .cancel(runId: row.runId)))
                            }
                        }.font(T3Theme.font(12)).buttonStyle(.borderless)
                    }
                }.onMove { offsets, destination in
                    var ids = model.conversation.queue.map(\.runId)
                    ids.move(fromOffsets: offsets, toOffset: destination)
                    model.perform(.queue(action: .reorder(runIds: ids)))
                }
            }
            .scrollContentBackground(.hidden).background(T3Theme.color("canvas"))
            .navigationTitle("Queue").navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) { EditButton() }; ToolbarItem(placement: .topBarTrailing) {
                    Button("Done") { dismiss() }
                }
            }
        }.presentationDetents([.medium, .large])
    }
}
