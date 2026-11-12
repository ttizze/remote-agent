import UIKit

@main @MainActor
final class NshSelectionMenuTests: UIResponder, UIApplicationDelegate {
    func application(_: UIApplication,
                     didFinishLaunchingWithOptions _: [UIApplication.LaunchOptionsKey: Any]?) -> Bool {
        let controller = NshSelectionMenuController()
        var received: [(String, String)] = []
        controller.perform = { received.append(($0, $1)) }
        check(controller.selectionMenu() == nil, "No selection must omit conversation actions")
        let selection = "選択した本文 👋\nsecond line"
        controller.select(selection)
        let actions = controller.selectionMenu()!.children.compactMap { $0 as? UIAction }
        check(actions.map(\.identifier.rawValue) == ["quote", "explain", "sideChat"],
              "Main conversation must expose all three actions")
        check(actions.map(\.title) == ["チャットに追加", "詳細を表示", "サイドチャットで質問"],
              "Menu must label each action")
        for action in actions { invoke(action) }
        check(received.map { $0.0 } == ["quote", "explain", "sideChat"], "Actions must dispatch their identity")
        check(received.allSatisfy { $0.1 == selection }, "Actions must preserve the exact selected text")
        controller.select("Changed selection")
        invoke(controller.selectionMenu()!.children[0] as! UIAction)
        check(received.last?.1 == "Changed selection", "Changing selection must refresh action payloads")
        controller.select("")
        check(controller.selectionMenu() == nil, "Cleared selection must omit actions")
        invoke(actions[0])
        check(received.last?.1 == selection, "Dismissal must not erase the invoked menu's text")
        let count = received.count
        controller.resetSelection()
        invoke(actions[0])
        check(received.count == count, "Switching conversations must invalidate old actions")
        controller.allowsSideChat = false
        controller.localize = { "Localized: " + $0 }
        controller.select("Side chat text")
        let sideActions = controller.selectionMenu()!.children.compactMap { $0 as? UIAction }
        check(sideActions.map(\.identifier.rawValue) == ["quote"], "Side chats must not spawn nested side chats")
        check(sideActions[0].title == "Localized: チャットに追加", "Menu labels must use the active locale")
        invoke(sideActions[0])
        check(received.last?.1 == "Side chat text", "Side chat must still quote the current selection")
        print("NSH selection menu tests passed")
        exit(0)
    }

    private func invoke(_ action: UIAction) {
        let button = UIButton(type: .system)
        button.addAction(action, for: .touchUpInside)
        button.sendActions(for: .touchUpInside)
    }

    private func check(_ condition: @autoclosure () -> Bool, _ message: String) {
        guard condition() else {
            fputs("FAIL: \(message)\n", stderr)
            exit(1)
        }
    }
}
