import AgentCore
import SwiftUI
import UniformTypeIdentifiers

struct DeviceScreenContent: View {
    @ObservedObject var model: BexAppViewModel
    let threadId: String
    let view: DeviceView
    let sessions: [DeviceSessionView]
    let frames: [DeviceFrameStore.RenderedFrame]
    let releaseInput: ([DeviceSessionView], String?, String?) -> Void
    let onTouch: (DeviceTouchEvent) -> Void
    @Binding var recordingDocument: DeviceRecordingDocument?
    @Binding var exportingRecording: Bool
    @Binding var recordingFileName: String
    @Binding var recordingContentType: UTType

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                if !view.enabled {
                    DeviceSupportDisabled(model: model)
                } else {
                    DeviceHostStatus(model: model, view: view)
                    DeviceInventory(model: model, view: view, threadSessions: sessions, releaseInput: releaseInput)
                    ForEach(Array(sessions.enumerated()), id: \.offset) { _, session in
                        DeviceSessionControls(
                            model: model,
                            session: session,
                            foregroundApp: view.details.first {
                                $0.hostId == session.hostId && $0.deviceId == session.deviceId
                            }?.foregroundApp,
                            duoStatus: duoStatus(for: session),
                            screenSummaries: screenSummaries(for: session),
                            releaseInput: {
                                releaseInput(sessions, session.hostId, session.deviceId)
                            },
                            stopRecording: stopRecording(for: session)
                        )
                    }
                    DeviceLiveFramePanel(view: view, frames: frames, onTouch: onTouch)
                    DeviceEventLogList(view: view, sessions: sessions)
                    if let recording = view.lastRecording, recording.threadId == threadId {
                        DeviceRecordingCard(
                            model: model,
                            recording: recording,
                            document: $recordingDocument,
                            exporting: $exportingRecording,
                            fileName: $recordingFileName,
                            contentType: $recordingContentType
                        )
                    }
                }
            }
            .padding(16)
        }
    }

    private func duoStatus(for session: DeviceSessionView) -> String? {
        guard let duo = view.duoControls.first(where: {
            $0.threadId == threadId && $0.hostId == session.hostId && $0.deviceId == session.deviceId
        }), duo.pending || duo.error != nil else { return nil }
        return duo.error ?? "Duo control pending"
    }

    private func screenSummaries(for session: DeviceSessionView) -> [String] {
        view.screens.filter {
            $0.threadId == threadId && $0.hostId == session.hostId && $0.deviceId == session.deviceId
        }.map {
            "Screen \($0.screenId.map(String.init) ?? "main") · \($0.width)×\($0.height) · \($0.orientation)"
        }
    }

    private func stopRecording(for session: DeviceSessionView) -> () -> Void {
        {
            guard let recording = view.recordings.first(where: {
                $0.threadId == threadId && $0.hostId == session.hostId && $0.deviceId == session.deviceId
            }) else { return }
            model.perform(.stopDeviceRecording(
                hostId: session.hostId,
                deviceId: session.deviceId,
                recordingId: recording.recordingId,
                sessionEpoch: recording.sessionEpoch
            ))
        }
    }
}

private struct DeviceSupportDisabled: View {
    @ObservedObject var model: BexAppViewModel

    var body: some View {
        Text("Device support is off").font(AppTheme.font(17, weight: .semibold))
        Text("Enable it to discover simulators and emulators on the Host.")
            .foregroundStyle(AppTheme.muted)
        Button("Enable device support") {
            model.perform(.configureDevices(enabled: true, agentAccessEnabled: nil, onboardingCompleted: false))
        }
    }
}

private struct DeviceHostStatus: View {
    @ObservedObject var model: BexAppViewModel
    let view: DeviceView

    var body: some View {
        Text("Host status: \(view.status)").foregroundStyle(AppTheme.muted)
        HStack(spacing: 8) {
            Button("Inspect tools") { model.perform(.inspectDevices(hostId: nil)) }
            Button("Update hub") { model.perform(.updateDeviceTool(hostId: nil, tool: "hub")) }
            Button("Update agent") { model.perform(.updateDeviceTool(hostId: nil, tool: "agent")) }
        }
        if let detail = view.statusDetail {
            Text(detail).font(AppTheme.font(13)).foregroundStyle(AppTheme.muted)
        }
        if !view.agentAccessEnabled {
            Button("Enable agent device access") {
                model.perform(.configureDevices(enabled: nil, agentAccessEnabled: true, onboardingCompleted: true))
            }
        }
        ForEach(view.hosts, id: \.id) { host in
            HStack {
                Text("\(host.label) · \(host.kind)").font(AppTheme.font(13)).foregroundStyle(AppTheme.muted)
                Spacer()
                Button("Retry") { model.perform(.retryDeviceHost(hostId: host.id)) }
            }
            ForEach(host.unavailableReasons, id: \.self) { reason in
                Text(reason).font(AppTheme.font(12)).foregroundStyle(AppTheme.muted)
            }
        }
    }
}

private struct DeviceInventory: View {
    @ObservedObject var model: BexAppViewModel
    let view: DeviceView
    let threadSessions: [DeviceSessionView]
    let releaseInput: ([DeviceSessionView], String?, String?) -> Void

    var body: some View {
        ForEach(Array(view.devices.enumerated()), id: \.offset) { _, device in
            let opened = threadSessions.contains {
                $0.hostId == device.hostId && $0.deviceId == device.id
            }
            HStack {
                VStack(alignment: .leading) {
                    Text(device.name).font(AppTheme.font(16, weight: .semibold))
                    Text("\(device.platform) · \(device.version)").foregroundStyle(AppTheme.muted)
                }
                Spacer()
                Button(opened ? "Close" : "Open") {
                    if opened {
                        releaseInput(threadSessions, device.hostId, device.id)
                        model.perform(.closeDevice(hostId: device.hostId, deviceId: device.id, shutdown: false))
                    } else {
                        model.perform(.openDevice(
                            hostId: device.hostId,
                            deviceId: device.id,
                            platform: device.platform,
                            boot: true
                        ))
                    }
                }
            }
            .padding(12)
            .background(AppTheme.card, in: RoundedRectangle(cornerRadius: 14))
        }
    }
}

private struct DeviceEventLogList: View {
    let view: DeviceView
    let sessions: [DeviceSessionView]

    var body: some View {
        ForEach(Array(view.accessibility.filter { tree in
            sessions.contains { $0.hostId == tree.hostId && $0.deviceId == tree.deviceId }
        }.enumerated()), id: \.offset) { _, tree in
            VStack(alignment: .leading, spacing: 4) {
                Text("Accessibility overlay").font(AppTheme.font(13, weight: .semibold))
                ForEach(tree.elements.filter { !$0.label.isEmpty }.prefix(20), id: \.id) { element in
                    Text("\(element.role) · \(element.label)")
                        .font(AppTheme.font(12)).foregroundStyle(AppTheme.muted)
                }
            }
        }
        ForEach(Array(view.eventLog.filter { entry in
            sessions.contains { $0.hostId == entry.hostId && $0.deviceId == entry.deviceId }
        }.suffix(20).enumerated()), id: \.offset) { _, entry in
            Text("\(entry.kind) · \(entry.summary)")
                .font(AppTheme.font(12)).foregroundStyle(AppTheme.muted)
        }
    }
}
