import AgentCore
import SwiftUI

/// The `/`, `$` and `@` menu above the composer.
struct CommandPopover: View {
    @ObservedObject var model: BexAppViewModel
    let text: String
    /// UTF-16 offset of the caret.
    let cursor: UInt32

    var body: some View {
        let menu = model.snapshot.composerMenu(text: text, cursor: cursor)
        if let trigger = menu.trigger, !menu.items.isEmpty || trigger.kind == .pullRequest {
            VStack(alignment: .leading, spacing: 0) {
                if let header = trigger.kind.header {
                    Text(header.uppercased()).font(AppTheme.font(11, weight: .bold)).tracking(0.8)
                        .foregroundStyle(AppTheme.muted)
                        .padding(.horizontal, 14).padding(.top, 10).padding(.bottom, 4)
                }
                if menu.items.isEmpty {
                    Text(menu.emptyLabel ?? "").font(AppTheme.font(12)).foregroundStyle(AppTheme.tertiary)
                        .padding(.horizontal, 14).padding(.vertical, 10)
                } else {
                    ScrollView {
                        VStack(spacing: 0) {
                            ForEach(Array(menu.items.enumerated()), id: \.element.id) { index, item in
                                Button {
                                    model.perform(.selectComposerItem(text: text, cursor: cursor, itemId: item.id))
                                } label: {
                                    CommandRow(item: item, skillPrefix: trigger.kind == .slashCommand)
                                }
                                .buttonStyle(.plain)
                                if index < menu.items.count - 1 {
                                    Rectangle().fill(AppTheme.border).frame(height: 0.5)
                                }
                            }
                        }
                    }
                    .scrollIndicators(.hidden)
                    .frame(maxHeight: 180)
                    .fixedSize(horizontal: false, vertical: true)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .glassEffect(.clear, in: RoundedRectangle(cornerRadius: 16))
        }
    }
}

private struct CommandRow: View {
    let item: ComposerCommandItem
    /// A skill offered under `/` reads `skill:name`.
    let skillPrefix: Bool

    var body: some View {
        HStack(spacing: 10) {
            icon
            label.font(AppTheme.font(16, weight: .medium)).foregroundStyle(AppTheme.text).lineLimit(1)
                .layoutPriority(1)
            if !item.description.isEmpty {
                Text(item.description).font(AppTheme.font(12)).foregroundStyle(AppTheme.muted).lineLimit(1)
                    .frame(maxWidth: .infinity, alignment: .leading)
            } else {
                Spacer(minLength: 0)
            }
        }
        .padding(.horizontal, 14).padding(.vertical, 10)
        .contentShape(Rectangle())
    }

    @ViewBuilder
    private var icon: some View {
        if let fileIcon = item.fileIcon {
            FileIconImage(icon: fileIcon, size: 16)
        } else {
            Image(systemName: item.skillSource?.symbol ?? item.target.symbol)
                .font(.system(size: item.target.isPath ? 16 : 14))
                .foregroundStyle(AppTheme.tertiary)
        }
    }

    private var label: Text {
        if skillPrefix, case let .skill(name) = item.target {
            return Text("\(Text("skill:").foregroundStyle(AppTheme.muted))\(name)")
        }
        return Text(item.label)
    }
}

extension ComposerTriggerKind {
    var header: String? {
        switch self {
        case .slashCommand: "Commands"
        case .skill: "Skills"
        case .path: "Files"
        case .pullRequest: "Pull requests"
        case .slashModel: nil
        }
    }
}

extension SkillSourceKind {
    var symbol: String {
        switch self {
        case .app: "square.grid.2x2"
        case .repo, .project: "folder"
        case .personal: "person.crop.circle"
        case .system: "gearshape"
        case .other: "cube"
        }
    }
}

extension ComposerCommandTarget {
    var symbol: String {
        switch self {
        case .builtIn, .providerCommand: "terminal"
        case .skill: "square.grid.2x2"
        case .path: "folder"
        case .thread: "text.bubble"
        }
    }

    var isPath: Bool {
        if case .path = self {
            return true
        }
        return false
    }
}

/// A file type's icon from the bundled Pierre set.
struct FileIconImage: View {
    let icon: MarkdownFileIcon
    var size: CGFloat = 16

    var body: some View {
        // The generated cases read as the asset names (`typescript`, `cpp`, …).
        Image("pierre_\(String(describing: icon))").resizable().scaledToFit()
            .frame(width: size, height: size)
            .accessibilityHidden(true)
    }
}
