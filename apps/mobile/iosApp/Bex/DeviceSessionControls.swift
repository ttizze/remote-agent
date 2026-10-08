import AgentCore
import SwiftUI

struct DeviceSessionControls: View {
    @ObservedObject var model: BexAppViewModel
    let session: DeviceSessionView
    let foregroundApp: String?
    let duoStatus: String?
    let screenSummaries: [String]
    let releaseInput: () -> Void
    let stopRecording: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            ScrollView(.horizontal) {
                HStack(spacing: 8) {
                    appearanceControls
                    platformControls
                    hardwareControls
                    Button("Enter") { sendEnter() }
                    Button("Rotate") { send(.rotate) }
                    Button("Book fold") { send(.duo(command: .pose(value: .book))) }
                    Button("Table") { send(.duo(command: .table(value: true))) }
                    Button("Record") {
                        model.perform(.startDeviceRecording(
                            hostId: session.hostId,
                            deviceId: session.deviceId,
                            format: "mp4"
                        ))
                    }
                    Button("Stop record", action: stopRecording)
                    Button("Power off") {
                        releaseInput()
                        model.perform(.closeDevice(hostId: session.hostId, deviceId: session.deviceId, shutdown: true))
                    }
                }
            }
            if let foregroundApp {
                Text("Foreground: \(foregroundApp)").font(AppTheme.font(13)).foregroundStyle(AppTheme.muted)
            }
            if let duoStatus {
                Text(duoStatus).font(AppTheme.font(12)).foregroundStyle(AppTheme.muted)
            }
            ForEach(screenSummaries, id: \.self) { summary in
                Text(summary).font(AppTheme.font(12)).foregroundStyle(AppTheme.muted)
            }
        }
    }

    @ViewBuilder
    private var appearanceControls: some View {
        Button("Dark") { send(.setAppearance(dark: true)) }
        Button("Light") { send(.setAppearance(dark: false)) }
        Button("Text +") { send(.setTextSize(size: "large")) }
    }

    @ViewBuilder
    private var platformControls: some View {
        if session.platform == "android" {
            Button("Portrait") { send(.setOrientation(orientation: "portrait")) }
            Button("Fold closed") { send(.fold(command: .closed)) }
            Button("Fold open") { send(.fold(command: .opened)) }
        } else {
            ForEach(duoCommands) { command in
                Button(command.title) { send(.duo(command: command.intent)) }
            }
        }
    }

    @ViewBuilder
    private var hardwareControls: some View {
        Button("Home") { send(.hardwareButton(button: "home")) }
        Button("Back") { send(.hardwareButton(button: "back")) }
        Button("Recents") { send(.hardwareButton(button: "recents")) }
        Button("Power") { send(.hardwareButton(button: "power")) }
    }

    private var duoCommands: [DeviceDuoCommand] {
        [
            DeviceDuoCommand("Closed", .pose(value: .closed)),
            DeviceDuoCommand("Book", .pose(value: .book)),
            DeviceDuoCommand("Open", .pose(value: .open)),
            DeviceDuoCommand("Laptop", .pose(value: .laptop)),
            DeviceDuoCommand("Tent", .pose(value: .tent)),
            DeviceDuoCommand("Table on", .table(value: true)),
            DeviceDuoCommand("Table off", .table(value: false)),
            DeviceDuoCommand("Face up", .physical(value: .faceup)),
            DeviceDuoCommand("Face down", .physical(value: .facedown)),
            DeviceDuoCommand("0°", .angle(value: 0)),
            DeviceDuoCommand("45°", .angle(value: 45)),
            DeviceDuoCommand("90°", .angle(value: 90)),
            DeviceDuoCommand("135°", .angle(value: 135)),
            DeviceDuoCommand("180°", .angle(value: 180)),
            DeviceDuoCommand("Portrait", .orientation(value: .portrait)),
            DeviceDuoCommand("Landscape left", .orientation(value: .landscapeLeft)),
            DeviceDuoCommand("Upside down", .orientation(value: .portraitUpsideDown)),
            DeviceDuoCommand("Landscape right", .orientation(value: .landscapeRight))
        ]
    }

    private func send(_ action: DeviceActionIntent) {
        model.perform(.deviceAction(hostId: session.hostId, deviceId: session.deviceId, action: action))
    }

    private func sendEnter() {
        send(.key(
            code: "Enter",
            key: "Enter",
            sessionEpoch: session.sessionEpoch,
            down: true,
            meta: false,
            ctrl: false,
            shift: false,
            alt: false
        ))
        send(.key(
            code: "Enter",
            key: "Enter",
            sessionEpoch: session.sessionEpoch,
            down: false,
            meta: false,
            ctrl: false,
            shift: false,
            alt: false
        ))
    }
}

private struct DeviceDuoCommand: Identifiable {
    let title: String
    let intent: DeviceDuoCommandIntent
    var id: String {
        title
    }

    init(_ title: String, _ intent: DeviceDuoCommandIntent) {
        self.title = title
        self.intent = intent
    }
}
