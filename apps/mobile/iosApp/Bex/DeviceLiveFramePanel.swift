import AgentCore
import SwiftUI
import UIKit

struct DeviceLiveFramePanel: View {
    let view: DeviceView
    let frames: [DeviceFrameStore.RenderedFrame]
    let onTouch: (DeviceTouchEvent) -> Void

    var body: some View {
        if frames.isEmpty {
            Text("Open a device to see its live frame").foregroundStyle(AppTheme.muted)
        } else {
            HStack(spacing: 8) {
                ForEach(frames) { frame in
                    DeviceFramePreview(frame: frame, view: view, onTouch: onTouch)
                }
            }
            .frame(maxWidth: .infinity)
            .contentShape(Rectangle())
        }
    }
}

private struct DeviceFramePreview: View {
    let frame: DeviceFrameStore.RenderedFrame
    let view: DeviceView
    let onTouch: (DeviceTouchEvent) -> Void

    var body: some View {
        ZStack {
            Image(uiImage: frame.image).resizable().scaledToFit().frame(maxWidth: .infinity)
            DeviceAccessibilityOverlay(view: view, deviceKey: "\(frame.hostId):\(frame.deviceId)")
        }
        .overlay {
            GeometryReader { proxy in
                Color.clear
                    .contentShape(Rectangle())
                    .gesture(
                        DragGesture(minimumDistance: 0)
                            .onChanged { value in
                                onTouch(DeviceTouchEvent(
                                    hostId: frame.hostId,
                                    deviceId: frame.deviceId,
                                    sessionEpoch: frame.sessionEpoch,
                                    viewSize: proxy.size,
                                    frameSize: CGSize(width: CGFloat(frame.width), height: CGFloat(frame.height)),
                                    phase: .changed(value.location)
                                ))
                            }
                            .onEnded { value in
                                onTouch(DeviceTouchEvent(
                                    hostId: frame.hostId,
                                    deviceId: frame.deviceId,
                                    sessionEpoch: frame.sessionEpoch,
                                    viewSize: proxy.size,
                                    frameSize: CGSize(width: CGFloat(frame.width), height: CGFloat(frame.height)),
                                    phase: .ended(value.location)
                                ))
                            }
                    )
            }
        }
    }
}

private struct DeviceAccessibilityOverlay: View {
    let view: DeviceView
    let deviceKey: String

    var body: some View {
        GeometryReader { proxy in
            ForEach(view.accessibility.filter { deviceKey == "\($0.hostId):\($0.deviceId)" }
                .flatMap(\.elements).filter { !$0.label.isEmpty }, id: \.id) { element in
                    RoundedRectangle(cornerRadius: 3)
                        .stroke(AppTheme.primary, lineWidth: 1)
                        .frame(
                            width: proxy.size.width * CGFloat(element.width),
                            height: proxy.size.height * CGFloat(element.height)
                        )
                        .position(
                            x: proxy.size.width * CGFloat(element.x + element.width / 2),
                            y: proxy.size.height * CGFloat(element.y + element.height / 2)
                        )
                        .accessibilityLabel(element.label)
                }
        }
        .allowsHitTesting(false)
    }
}
