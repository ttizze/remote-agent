import AgentCore
import SwiftUI
import UIKit
import UniformTypeIdentifiers

struct DeviceScreen: View {
    @ObservedObject var model: BexAppViewModel
    let threadId: String
    @StateObject private var deviceFrames = DeviceFrameStore()
    @State private var recordingDocument: DeviceRecordingDocument?
    @State private var exportingRecording = false
    @State private var recordingFileName = "device-recording"
    @State private var recordingContentType: UTType = .data
    @State private var touchingHostId: String?
    @State private var touchingDeviceId: String?
    @State private var touchingSessionEpoch: String?
    @State private var touchingPoint: CGPoint?
    @State private var focusedDeviceKey: String?
    @State private var keyboardHostId: String?
    @State private var keyboardDeviceId: String?
    @State private var keyboardSessionEpoch: String?

    var body: some View {
        let view = model.snapshot.device()
        let sessions = view.sessions.filter { $0.threadId == threadId }
        let sessionKey = sessions.map(sessionIdentity).joined(separator: ",")
        DeviceScreenContent(
            model: model,
            threadId: threadId,
            view: view,
            sessions: sessions,
            frames: deviceFrames.frames(for: threadId),
            releaseInput: { sessions, hostId, deviceId in
                releaseDeviceInput(sessions: sessions, hostId: hostId, deviceId: deviceId)
            },
            onTouch: handleTouch,
            recordingDocument: $recordingDocument,
            exportingRecording: $exportingRecording,
            recordingFileName: $recordingFileName,
            recordingContentType: $recordingContentType
        )
        .background(AppTheme.screen.ignoresSafeArea())
        .navigationTitle("Device")
        .focusable()
        .onKeyPress(phases: .down) { press in
            sendDeviceKey(press, down: true, sessions: sessions)
        }
        .onKeyPress(phases: .up) { press in
            sendDeviceKey(press, down: false, sessions: sessions)
        }
        .onAppear {
            openDeviceScreen(view: view, sessions: sessions)
        }
        .onChange(of: view.frameRevision) { _, _ in
            deviceFrames.consume(model.snapshot.device().videoEvents, threadId: threadId)
        }
        .onChange(of: sessionKey) { _, _ in
            resetDeviceScreen()
        }
        .onDisappear {
            closeDeviceScreen(sessions: sessions)
        }
        .fileExporter(
            isPresented: $exportingRecording,
            document: recordingDocument,
            contentType: recordingContentType,
            defaultFilename: recordingFileName
        ) { result in
            if case let .failure(error) = result {
                model.notice = error.localizedDescription
            }
        }
    }

    private func sessionIdentity(_ session: DeviceSessionView) -> String {
        "\(session.hostId):\(session.deviceId):\(session.sessionEpoch)"
    }

    private func openDeviceScreen(view: DeviceView, sessions: [DeviceSessionView]) {
        model.perform(.openThread(threadId: threadId))
        model.perform(.loadDevices)
        model.perform(.subscribeDevice)
        deviceFrames.consume(view.videoEvents, threadId: threadId)
        loadDeviceData(sessions)
    }

    private func loadDeviceData(_ sessions: [DeviceSessionView]) {
        for session in sessions {
            model.perform(.loadDeviceDetail(hostId: session.hostId, deviceId: session.deviceId))
            model.perform(.loadDeviceAccessibility(hostId: session.hostId, deviceId: session.deviceId))
            model.perform(.loadDeviceEventLog(hostId: session.hostId, deviceId: session.deviceId, limit: 100))
        }
    }

    private func resetDeviceScreen() {
        finishActiveTouch()
        let sessions = model.snapshot.device().sessions.filter { $0.threadId == threadId }
        releaseAllDeviceInput(sessions)
        focusedDeviceKey = nil
        deviceFrames.reset(threadId: threadId)
        let view = model.snapshot.device()
        deviceFrames.consume(view.videoEvents, threadId: threadId)
        loadDeviceData(sessions)
    }

    private func closeDeviceScreen(sessions: [DeviceSessionView]) {
        finishActiveTouch()
        releaseAllDeviceInput(sessions)
        focusedDeviceKey = nil
        deviceFrames.reset(threadId: threadId)
        model.perform(.unsubscribeDevice)
    }

    private func handleTouch(_ event: DeviceTouchEvent) {
        switch event.phase {
        case let .changed(location):
            handleTouchChange(event, location: location)
        case let .ended(location):
            handleTouchEnd(event, location: location)
        }
    }

    private func handleTouchChange(_ event: DeviceTouchEvent, location: CGPoint) {
        let ownsGesture = touchingHostId == event.hostId
            && touchingDeviceId == event.deviceId
            && touchingSessionEpoch == event.sessionEpoch
        if !ownsGesture {
            finishActiveTouch()
        }
        guard let point = projectTouchPoint(location, viewSize: event.viewSize, frameSize: event.frameSize) else {
            return
        }
        touchingHostId = event.hostId
        touchingDeviceId = event.deviceId
        touchingSessionEpoch = event.sessionEpoch
        touchingPoint = point
        focusedDeviceKey = "\(event.hostId):\(event.deviceId)"
        let phase = ownsGesture ? "move" : "begin"
        sendTouch(event, phase: phase, point: point)
    }

    private func handleTouchEnd(_ event: DeviceTouchEvent, location: CGPoint) {
        guard touchingHostId == event.hostId,
              touchingDeviceId == event.deviceId,
              touchingSessionEpoch == event.sessionEpoch else {
            if touchingHostId == event.hostId, touchingDeviceId == event.deviceId {
                finishActiveTouch()
            }
            return
        }
        guard let point = projectTouchPoint(location, viewSize: event.viewSize, frameSize: event.frameSize) else {
            finishActiveTouch()
            return
        }
        sendTouch(event, phase: "end", point: point)
        clearTouch()
    }

    private func sendTouch(_ event: DeviceTouchEvent, phase: String, point: CGPoint) {
        model.perform(.deviceAction(
            hostId: event.hostId,
            deviceId: event.deviceId,
            action: .touch(phase: phase, x: Float(point.x), y: Float(point.y))
        ))
    }

    private func finishActiveTouch() {
        guard let hostId = touchingHostId,
              let deviceId = touchingDeviceId,
              let sessionEpoch = touchingSessionEpoch,
              let point = touchingPoint else { return }
        let currentEpoch = model.snapshot.device().sessions.first {
            $0.threadId == threadId && $0.hostId == hostId && $0.deviceId == deviceId
        }?.sessionEpoch
        if currentEpoch == sessionEpoch {
            model.perform(.deviceAction(
                hostId: hostId,
                deviceId: deviceId,
                action: .touch(phase: "end", x: Float(point.x), y: Float(point.y))
            ))
        }
        clearTouch()
    }

    private func clearTouch() {
        touchingHostId = nil
        touchingDeviceId = nil
        touchingSessionEpoch = nil
        touchingPoint = nil
    }

    private func sendDeviceKey(_ press: KeyPress, down: Bool, sessions: [DeviceSessionView]) -> KeyPress.Result {
        let target = sessions.first { focusedDeviceKey == "\($0.hostId):\($0.deviceId)" } ?? sessions.first
        guard let target, let facts = deviceKeyFacts(press) else { return .ignored }
        if keyboardHostId != target.hostId
            || keyboardDeviceId != target.deviceId
            || keyboardSessionEpoch != target.sessionEpoch {
            releaseDeviceInput(sessions: sessions)
            keyboardHostId = target.hostId
            keyboardDeviceId = target.deviceId
            keyboardSessionEpoch = target.sessionEpoch
        }
        model.perform(.deviceAction(
            hostId: target.hostId,
            deviceId: target.deviceId,
            action: .key(
                code: facts.code,
                key: facts.key,
                sessionEpoch: target.sessionEpoch,
                down: down,
                meta: press.modifiers.contains(.command),
                ctrl: press.modifiers.contains(.control),
                shift: press.modifiers.contains(.shift),
                alt: press.modifiers.contains(.option)
            )
        ))
        return .handled
    }

    private func releaseAllDeviceInput(_ sessions: [DeviceSessionView]) {
        for session in sessions {
            releaseDeviceInput(sessions: sessions, hostId: session.hostId, deviceId: session.deviceId)
        }
    }

    private func releaseDeviceInput(
        sessions: [DeviceSessionView],
        hostId: String? = nil,
        deviceId: String? = nil,
        sessionEpoch: String? = nil
    ) {
        let identity: DeviceInputIdentity? = if let hostId, let deviceId {
            DeviceInputIdentity(hostId: hostId, deviceId: deviceId, sessionEpoch: sessionEpoch)
        } else if let keyboardHostId, let keyboardDeviceId {
            DeviceInputIdentity(
                hostId: keyboardHostId,
                deviceId: keyboardDeviceId,
                sessionEpoch: keyboardSessionEpoch
            )
        } else {
            sessions.first { focusedDeviceKey == "\($0.hostId):\($0.deviceId)" }
                .map { DeviceInputIdentity(hostId: $0.hostId, deviceId: $0.deviceId, sessionEpoch: $0.sessionEpoch) }
                ?? sessions.first.map { DeviceInputIdentity(
                    hostId: $0.hostId,
                    deviceId: $0.deviceId,
                    sessionEpoch: $0.sessionEpoch
                ) }
        }
        guard let identity else { return }
        let epoch = identity.sessionEpoch ?? sessions.first {
            $0.hostId == identity.hostId && $0.deviceId == identity.deviceId
        }?.sessionEpoch
        guard let epoch else { return }
        model.perform(.releaseDeviceInput(hostId: identity.hostId, deviceId: identity.deviceId, sessionEpoch: epoch))
        if keyboardHostId == identity.hostId,
           keyboardDeviceId == identity.deviceId,
           keyboardSessionEpoch == epoch {
            keyboardHostId = nil
            keyboardDeviceId = nil
            keyboardSessionEpoch = nil
        }
    }
}
