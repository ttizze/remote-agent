import AgentCore
import SwiftUI

/// The `/`, `$` and `@` menu above the composer.
struct CommandPopover: View {
    @ObservedObject var model: BexAppViewModel
    let text: String

    var body: some View {
        let cursor = UInt32(text.utf16.count)
        let menu = model.snapshot.composerMenu(text: text, cursor: cursor)
        if let trigger = menu.trigger {
            VStack(alignment: .leading, spacing: 0) {
                Text(trigger.kind.header.uppercased()).font(AppTheme.font(11, weight: .bold)).tracking(0.8)
                    .foregroundStyle(AppTheme.muted)
                    .padding(.horizontal, 12.25).padding(.top, 8.75).padding(.bottom, 3.5)
                if menu.items.isEmpty {
                    Text(trigger.kind.emptyLabel).font(AppTheme.font(13)).foregroundStyle(AppTheme.tertiary)
                        .padding(.horizontal, 12.25).padding(.vertical, 8.75)
                } else {
                    ScrollView {
                        VStack(spacing: 0) {
                            ForEach(menu.items, id: \.id) { item in
                                Button {
                                    model.perform(.selectComposerItem(text: text, cursor: cursor, itemId: item.id))
                                } label: {
                                    CommandRow(item: item)
                                }
                                .buttonStyle(.plain)
                                Divider()
                            }
                        }
                    }
                    .frame(maxHeight: 180)
                }
            }
            .glassEffect(.clear, in: RoundedRectangle(cornerRadius: 16))
        }
    }
}

private struct CommandRow: View {
    let item: ComposerCommandItem

    var body: some View {
        HStack(spacing: 8.75) {
            Image(systemName: item.target.symbol).font(.system(size: 14)).foregroundStyle(AppTheme.muted)
            VStack(alignment: .leading, spacing: 1) {
                Text(item.label).font(AppTheme.font(16, weight: .medium)).foregroundStyle(AppTheme.text)
                if !item.description.isEmpty {
                    Text(item.description).font(AppTheme.font(13)).foregroundStyle(AppTheme.muted).lineLimit(1)
                }
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 12.25).padding(.vertical, 8.75)
        .contentShape(Rectangle())
    }
}

extension ComposerTriggerKind {
    var header: String {
        switch self {
        case .slashCommand, .slashModel: "Commands"
        case .skill: "Skills"
        case .path: "Files"
        case .pullRequest: "Pull requests"
        }
    }

    var emptyLabel: String {
        switch self {
        case .slashCommand, .slashModel: "No matching commands."
        case .skill: "No skills found."
        case .path: "No matching files or folders."
        case .pullRequest: "No matching pull requests."
        }
    }
}

extension ComposerCommandTarget {
    var symbol: String {
        switch self {
        case .builtIn, .providerCommand: "terminal"
        case .skill: "square.grid.2x2"
        case .path(_, directory: true): "folder"
        case .path: "doc"
        case .thread: "text.bubble"
        }
    }
}
