import AgentCore
import CoreGraphics
import Foundation
import SwiftUI

struct DeviceTouchEvent {
    enum Phase {
        case changed(CGPoint)
        case ended(CGPoint)
    }

    let hostId: String
    let deviceId: String
    let sessionEpoch: String
    let viewSize: CGSize
    let frameSize: CGSize
    let phase: Phase
}

struct DeviceInputIdentity {
    let hostId: String
    let deviceId: String
    let sessionEpoch: String?
}

func projectTouchPoint(_ point: CGPoint, viewSize: CGSize, frameSize: CGSize) -> CGPoint? {
    guard let projected = AgentCore.projectTouchPoint(
        x: Float(point.x),
        y: Float(point.y),
        viewportWidth: Float(viewSize.width),
        viewportHeight: Float(viewSize.height),
        contentWidth: Float(frameSize.width),
        contentHeight: Float(frameSize.height)
    ) else {
        return nil
    }
    return CGPoint(x: CGFloat(projected.x), y: CGFloat(projected.y))
}

func deviceKeyFacts(_ press: KeyPress) -> DeviceKeyFacts? {
    if let source = editingKeySource(press.key) ?? navigationKeySource(press.key) {
        return AgentCore.canonicalDeviceKey(code: source.code, key: source.key)
    }
    guard !press.characters.isEmpty else { return nil }
    return AgentCore.canonicalDeviceKey(code: "", key: press.characters)
}

private func editingKeySource(_ key: KeyEquivalent) -> (code: String, key: String)? {
    switch key {
    case .return: ("Enter", "Enter")
    case .tab: ("Tab", "Tab")
    case .delete: ("Backspace", "Backspace")
    case .deleteForward: ("Delete", "Delete")
    case .escape: ("Escape", "Escape")
    case .space: ("Space", " ")
    default: nil
    }
}

private func navigationKeySource(_ key: KeyEquivalent) -> (code: String, key: String)? {
    switch key {
    case .upArrow: ("ArrowUp", "ArrowUp")
    case .downArrow: ("ArrowDown", "ArrowDown")
    case .leftArrow: ("ArrowLeft", "ArrowLeft")
    case .rightArrow: ("ArrowRight", "ArrowRight")
    case .home: ("Home", "Home")
    case .end: ("End", "End")
    case .pageUp: ("PageUp", "PageUp")
    case .pageDown: ("PageDown", "PageDown")
    default: nil
    }
}
