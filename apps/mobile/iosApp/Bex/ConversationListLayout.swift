import SwiftUI
import UIKit

/// Native layout owns row measurement; the controller owns the reading position.
final class ConversationListLayout: UICollectionViewCompositionalLayout {
    private var alignment: CGFloat = 0

    static func make() -> ConversationListLayout {
        let configuration = UICollectionViewCompositionalLayoutConfiguration()
        configuration.contentInsetsReference = .none
        // The compositional container omits UIScrollView's explicit horizontal insets.
        // Read the actual viewport when UIKit requests a section, including after a resize.
        weak var owner: ConversationListLayout?
        let layout = ConversationListLayout(sectionProvider: { _, environment in
            let width: CGFloat
            if let view = owner?.collectionView {
                let insets = view.adjustedContentInset
                width = view.bounds.width - insets.left - insets.right
            } else {
                width = environment.container.effectiveContentSize.width
            }
            let item = NSCollectionLayoutItem(layoutSize: NSCollectionLayoutSize(
                widthDimension: .fractionalWidth(1), heightDimension: .estimated(100)
            ))
            let group = NSCollectionLayoutGroup.vertical(layoutSize: NSCollectionLayoutSize(
                widthDimension: .absolute(max(1, width - 32)), heightDimension: .estimated(100)
            ), subitems: [item])
            let section = NSCollectionLayoutSection(group: group)
            section.interGroupSpacing = 12
            section.contentInsets = NSDirectionalEdgeInsets(top: 6, leading: 16, bottom: 6, trailing: 16)
            return section
        }, configuration: configuration)
        owner = layout
        return layout
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
        // Even a fixed-width section depends on viewport height because short histories align to its bottom.
        super.shouldInvalidateLayout(forBoundsChange: newBounds)
            || newBounds.height != collectionView?.bounds.height
    }

    override var collectionViewContentSize: CGSize {
        var size = super.collectionViewContentSize
        if let view = collectionView {
            let insets = view.adjustedContentInset
            size.width = max(0, view.bounds.width - insets.left - insets.right)
        }
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

/// A real child controller preserves native selection and controller-backed SwiftUI content in recycled cells.
final class ConversationListCell: UICollectionViewCell {
    private var host: UIHostingController<AnyView>?

    var isMounted: Bool {
        host?.parent != nil
    }

    func show(_ content: AnyView, parent: UIViewController) {
        let row = AnyView(content.fixedSize(horizontal: false, vertical: true))
        let host: UIHostingController<AnyView>
        if let retained = self.host {
            host = retained
            host.rootView = row
        } else {
            host = UIHostingController(rootView: row)
            self.host = host
        }
        if host.parent == nil {
            host.safeAreaRegions = []
            host.sizingOptions = .intrinsicContentSize
            parent.addChild(host)
            host.view.backgroundColor = .clear
            host.view.translatesAutoresizingMaskIntoConstraints = false
            contentView.addSubview(host.view)
            NSLayoutConstraint.activate([
                host.view.leadingAnchor.constraint(equalTo: contentView.leadingAnchor),
                host.view.trailingAnchor.constraint(equalTo: contentView.trailingAnchor),
                host.view.topAnchor.constraint(equalTo: contentView.topAnchor),
                host.view.bottomAnchor.constraint(equalTo: contentView.bottomAnchor)
            ])
            host.didMove(toParent: parent)
        }
        // UIKit observes invalidation of the cell's content view, not the child controller's view.
        contentView.invalidateIntrinsicContentSize()
    }

    func unmount() {
        guard let host, host.parent != nil else { return }
        host.willMove(toParent: nil)
        host.view.removeFromSuperview()
        host.removeFromParent()
        host.rootView = AnyView(EmptyView())
    }

    override func prepareForReuse() {
        super.prepareForReuse()
        unmount()
    }

    override func preferredLayoutAttributesFitting(_ layoutAttributes: UICollectionViewLayoutAttributes)
        -> UICollectionViewLayoutAttributes {
        guard let attributes = layoutAttributes.copy() as? UICollectionViewLayoutAttributes else {
            return layoutAttributes
        }
        // Recycling must not replace a measured row's size with the empty hosting view's size.
        guard let host, host.parent != nil else { return attributes }
        let proposal = CGSize(width: attributes.size.width, height: .greatestFiniteMagnitude)
        attributes.size.height = max(1, ceil(host.sizeThatFits(in: proposal).height))
        return attributes
    }
}
