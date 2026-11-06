import UIKit

/// Native layout owns row measurement; the controller owns the reading position.
final class ConversationListLayout: UICollectionViewCompositionalLayout {
    private var alignment: CGFloat = 0

    /// Rows span the safe area's width minus their margins; UIKit resizes them with the viewport.
    static func make() -> ConversationListLayout {
        let item = NSCollectionLayoutItem(layoutSize: NSCollectionLayoutSize(
            widthDimension: .fractionalWidth(1), heightDimension: .estimated(100)
        ))
        let group = NSCollectionLayoutGroup.vertical(layoutSize: NSCollectionLayoutSize(
            widthDimension: .fractionalWidth(1), heightDimension: .estimated(100)
        ), subitems: [item])
        let section = NSCollectionLayoutSection(group: group)
        section.interGroupSpacing = 12
        section.contentInsets = NSDirectionalEdgeInsets(top: 6, leading: 16, bottom: 6, trailing: 16)
        return ConversationListLayout(section: section)
    }

    override func prepare() {
        super.prepare()
        guard let collectionView else { return }
        let insets = collectionView.adjustedContentInset
        let viewport = collectionView.bounds.height - insets.top - insets.bottom
        // Changing section metrics here discards self-sizing measurements and creates a layout loop.
        // Alignment translates the measured geometry without invalidating those measurements.
        alignment = max(0, viewport - super.collectionViewContentSize.height)
    }

    override func shouldInvalidateLayout(forBoundsChange newBounds: CGRect) -> Bool {
        // Short histories align to the viewport's bottom, so its height matters as well as its width.
        super.shouldInvalidateLayout(forBoundsChange: newBounds)
            || newBounds.height != collectionView?.bounds.height
    }

    override var collectionViewContentSize: CGSize {
        var size = super.collectionViewContentSize
        size.height += alignment
        return size
    }

    override func layoutAttributesForItem(at indexPath: IndexPath) -> UICollectionViewLayoutAttributes? {
        super.layoutAttributesForItem(at: indexPath).map { translated($0, by: alignment) }
    }

    override func layoutAttributesForElements(in rect: CGRect) -> [UICollectionViewLayoutAttributes]? {
        super.layoutAttributesForElements(in: rect.offsetBy(dx: 0, dy: -alignment))?
            .map { translated($0, by: alignment) }
    }

    private func translated(_ attributes: UICollectionViewLayoutAttributes,
                            by offset: CGFloat) -> UICollectionViewLayoutAttributes {
        guard let copy = attributes.copy() as? UICollectionViewLayoutAttributes else { return attributes }
        copy.frame.origin.y += offset
        return copy
    }

    override func invalidationContext(
        forPreferredLayoutAttributes preferredAttributes: UICollectionViewLayoutAttributes,
        withOriginalAttributes originalAttributes: UICollectionViewLayoutAttributes
    )
        -> UICollectionViewLayoutInvalidationContext {
        super.invalidationContext(
            forPreferredLayoutAttributes: translated(preferredAttributes, by: -alignment),
            withOriginalAttributes: translated(originalAttributes, by: -alignment)
        )
    }

    func bottomOffset(viewportHeight: CGFloat, insets: UIEdgeInsets) -> CGFloat {
        let height = collectionViewContentSize.height - alignment
        return max(-insets.top, height - viewportHeight + insets.bottom)
    }
}

/// Selectable message text takes focus only after its double-tap selection fails (about 350ms),
/// and empty space never takes it; dismiss the keyboard on the tap itself.
final class KeyboardDismissingTap: UITapGestureRecognizer, UIGestureRecognizerDelegate {
    private var keyboardShown = false

    init() {
        super.init(target: nil, action: nil)
        addTarget(self, action: #selector(dismissKeyboard))
        cancelsTouchesInView = false
        delegate = self
        NotificationCenter.default.addObserver(self, selector: #selector(keyboardWillShow),
                                               name: UIResponder.keyboardWillShowNotification, object: nil)
        NotificationCenter.default.addObserver(self, selector: #selector(keyboardWillHide),
                                               name: UIResponder.keyboardWillHideNotification, object: nil)
    }

    @objc private func keyboardWillShow() {
        keyboardShown = true
    }

    @objc private func keyboardWillHide() {
        keyboardShown = false
    }

    @objc private func dismissKeyboard() {
        UIApplication.shared.sendAction(#selector(UIResponder.resignFirstResponder), to: nil, from: nil, for: nil)
    }

    /// Without a keyboard, the first responder is selected message text, whose taps must keep their selection.
    func gestureRecognizerShouldBegin(_: UIGestureRecognizer) -> Bool {
        keyboardShown
    }

    /// The tapped content still receives the tap, so links, buttons and text selection keep working.
    func gestureRecognizer(_: UIGestureRecognizer,
                           shouldRecognizeSimultaneouslyWith _: UIGestureRecognizer) -> Bool {
        true
    }
}
