import UIKit

/// The taps that confirm actions, as the mobile app plays them.
@MainActor
enum Haptics {
    /// A thread list action or a copy.
    static func light() {
        UIImpactFeedbackGenerator(style: .light).impactOccurred()
    }

    /// A swipe reaching the action a full swipe commits.
    static func medium() {
        UIImpactFeedbackGenerator(style: .medium).impactOccurred()
    }

    /// A choice, a disclosure, a navigation or streaming text.
    static func selection() {
        UISelectionFeedbackGenerator().selectionChanged()
    }

    /// Copies `text` to the clipboard with the light tap.
    static func copy(_ text: String) {
        UIPasteboard.general.string = text
        light()
    }
}

extension UIResponder {
    private nonisolated(unsafe) weak static var found: UIResponder?

    /// The focused text input is still composing (marked) text, such as kana
    /// awaiting conversion, so Return confirms it rather than sending.
    @MainActor static var isComposingText: Bool {
        found = nil
        UIApplication.shared.sendAction(#selector(UIResponder.captureFirstResponder), to: nil, from: nil, for: nil)
        return (found as? UITextInput)?.markedTextRange != nil
    }

    @objc private func captureFirstResponder() {
        UIResponder.found = self
    }
}
