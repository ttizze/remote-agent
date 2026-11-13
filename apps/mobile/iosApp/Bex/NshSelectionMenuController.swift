import UIKit

final class NshSelectionMenuController: UIViewController {
    var perform: (String, String) -> Void = { _, _ in }
    var localize: (String) -> String = { $0 }
    var allowsSideChat = true
    private var selectedText = ""
    private var generation = 0

    func select(_ text: String) {
        guard selectedText != text else { return }
        selectedText = text
        UIMenuSystem.context.setNeedsRebuild()
    }

    func resetSelection() {
        generation += 1
        select("")
    }

    override func buildMenu(with builder: UIMenuBuilder) {
        super.buildMenu(with: builder)
        guard builder.system == UIMenuSystem.context,
              builder.menu(for: .standardEdit) != nil,
              let menu = selectionMenu() else { return }
        builder.insertSibling(menu, beforeMenu: .standardEdit)
    }

    func selectionMenu() -> UIMenu? {
        guard !selectedText.isEmpty else { return nil }
        let text = selectedText
        let currentGeneration = generation
        let items = allowsSideChat
            ? [("quote", "チャットに追加"), ("explain", "詳細を表示"), ("sideChat", "サイドチャットで質問")]
            : [("quote", "チャットに追加")]
        let actions = items.map { name, title in
            UIAction(title: localize(title), identifier: UIAction.Identifier(name)) { [weak self] _ in
                guard let self, self.generation == currentGeneration else { return }
                self.perform(name, text)
            }
        }
        return UIMenu(identifier: UIMenu.Identifier("bex.conversation.selection"),
                      options: .displayInline, children: actions)
    }
}
