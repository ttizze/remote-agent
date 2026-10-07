import AgentCore
import SwiftUI

struct ThreadListRowView: View {
    let row: ThreadRow
    let icon: UIImage?
    let sidebar: Bool
    let open: () -> Void

    var body: some View {
        Button(action: open) {
            Group {
                if row.variant == .card {
                    card
                } else {
                    slim
                }
            }
            .padding(.horizontal, sidebar ? 12 : 17.5)
            .padding(.vertical, row.variant == .card ? (sidebar ? 10 : 8.75) : 7)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(background)
            .overlay(alignment: .bottom) {
                if row.showTrailingDivider, !sidebar {
                    Rectangle().fill(AppTheme.borderSubtle).frame(height: 1).padding(.leading, 17.5)
                }
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .padding(.horizontal, sidebar ? 8 : 0)
        .accessibilityHint("Opens the thread. Swipe left for settle and snooze actions.")
        .accessibilityLabel(row.hasQueuedMessages ? "\(row.title), messages queued to send" : row.title)
    }

    @ViewBuilder
    private var background: some View {
        if sidebar {
            RoundedRectangle(cornerRadius: 12)
                .fill(row.selected ? AppTheme.color("mobileSelected") : Color.clear)
        } else {
            AppTheme.screen
        }
    }

    private var card: some View {
        VStack(alignment: .leading, spacing: 3.5) {
            HStack(spacing: 5.25) {
                ProjectGlyph(name: row.projectTitle ?? "", icon: icon)
                Text(row.projectTitle ?? "").font(AppTheme.font(14, weight: .medium))
                    .foregroundStyle(AppTheme.muted).lineLimit(1)
                    .frame(maxWidth: .infinity, alignment: .leading)
                if row.hasQueuedMessages {
                    Image(systemName: "tray.and.arrow.up").font(.system(size: 12)).foregroundStyle(AppTheme.muted)
                }
                if row.pinned {
                    Image(systemName: "pin").font(.system(size: 11)).foregroundStyle(AppTheme.muted)
                }
                trailing
            }
            Text(row.title).font(AppTheme.font(16, weight: .medium)).foregroundStyle(AppTheme.text)
                .lineLimit(2).multilineTextAlignment(.leading)
            if let snippet = row.searchSnippet {
                Text(snippet).font(AppTheme.font(13)).foregroundStyle(AppTheme.muted).lineLimit(2)
            }
            if row.branch != nil || !row.providerInstances.isEmpty {
                HStack(spacing: 7) {
                    if let branch = row.branch {
                        Text(branch).font(AppTheme.mono(13)).foregroundStyle(AppTheme.muted).lineLimit(1)
                            .truncationMode(.middle)
                    }
                    Spacer(minLength: 0)
                    ProviderStack(instances: row.providerInstances)
                }
            }
        }
    }

    private var slim: some View {
        HStack(spacing: 8.75) {
            ProjectGlyph(name: row.projectTitle ?? "", icon: icon).opacity(0.4)
            VStack(alignment: .leading, spacing: 2) {
                Text(row.title).font(AppTheme.font(16)).foregroundStyle(AppTheme.muted).lineLimit(1)
                if let snippet = row.searchSnippet {
                    Text(snippet).font(AppTheme.font(13)).foregroundStyle(AppTheme.muted).lineLimit(1)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            if row.hasQueuedMessages {
                Image(systemName: "tray.and.arrow.up").font(.system(size: 12)).foregroundStyle(AppTheme.muted)
            }
            Text(row.snoozeWakeLabel ?? row.timeLabel).font(AppTheme.mono(14)).monospacedDigit()
                .foregroundStyle(row.snoozed ? AppTheme.muted : AppTheme.tertiary)
        }
        .frame(minHeight: 30)
    }

    @ViewBuilder
    private var trailing: some View {
        if let label = row.statusLabel {
            Text(label).font(AppTheme.font(13)).monospacedDigit().foregroundStyle(row.status.color)
        } else {
            Text(row.timeLabel).font(AppTheme.font(13)).monospacedDigit().foregroundStyle(AppTheme.tertiary)
        }
    }
}

extension ThreadListStatus {
    var color: Color {
        switch self {
        case .approval, .limited: AppTheme.warningForeground
        case .input: AppTheme.indigo
        case .working, .waiting: AppTheme.sky
        case .failed: AppTheme.dangerForeground
        case .ready: AppTheme.emerald
        }
    }
}

/// A project's icon from the Host, or its initial when it has none.
struct ProjectGlyph: View {
    let name: String
    let icon: UIImage?
    var size: CGFloat = 15

    var body: some View {
        if let icon {
            Image(uiImage: icon).resizable().scaledToFit()
                .frame(width: size, height: size)
                .clipShape(RoundedRectangle(cornerRadius: size * 0.16))
                .accessibilityLabel("\(name) favicon")
        } else {
            Text(name.first.map { String($0).uppercased() } ?? "·")
                .font(AppTheme.font(size * 0.6, weight: .bold))
                .foregroundStyle(AppTheme.muted)
                .frame(width: size, height: size)
                .background(AppTheme.subtle, in: RoundedRectangle(cornerRadius: size * 0.23))
        }
    }
}

/// Decoded project icons by content hash.
@MainActor
enum ProjectIconImages {
    private static var decoded: [String: UIImage] = [:]

    static func image(_ snapshot: AgentCore.Snapshot, _ projectId: String) -> UIImage? {
        guard let hash = snapshot.projectIconHash(projectId: projectId) else { return nil }
        if let image = decoded[hash] {
            return image
        }
        guard let icon = snapshot.projectIcon(projectId: projectId), let image = UIImage(data: icon.data) else {
            return nil
        }
        decoded[icon.hash] = image
        return image
    }
}

/// Earlier providers at 30%, overlapping, then the current one.
struct ProviderStack: View {
    let instances: [String]

    var body: some View {
        HStack(spacing: -3.5) {
            ForEach(Array(instances.enumerated()), id: \.offset) { index, instance in
                let current = index == instances.count - 1
                ProviderIcon(instance: instance, size: current ? 14 : 12).opacity(current ? 1 : 0.3)
            }
        }
    }
}

struct ProviderIcon: View {
    let instance: String
    var size: CGFloat = 14

    var body: some View {
        Image(instance.lowercased().contains("claude") ? "claude" : "openai")
            .resizable().scaledToFit().frame(width: size, height: size)
    }
}

extension Driver {
    var iconName: String {
        self == .claude ? "claude" : "openai"
    }
}

struct PendingTaskRowView: View {
    let task: PendingTaskRow
    let icon: UIImage?
    let sidebar: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if task.showPendingDivider {
                SectionRule(label: "Unsent", sidebar: sidebar)
            }
            VStack(alignment: .leading, spacing: 3.5) {
                HStack(spacing: 5.25) {
                    ProjectGlyph(name: task.projectTitle, icon: icon)
                    Text(task.projectTitle).font(AppTheme.font(14, weight: .medium)).foregroundStyle(AppTheme.muted)
                        .lineLimit(1).frame(maxWidth: .infinity, alignment: .leading)
                    Text("Sends on reconnect").font(AppTheme.font(13)).foregroundStyle(AppTheme.tertiary)
                }
                Text(task.title).font(AppTheme.font(16, weight: .medium)).lineLimit(1)
                if let branch = task.branch {
                    Text(branch).font(AppTheme.mono(13)).foregroundStyle(AppTheme.muted).lineLimit(1)
                }
            }
            .padding(.horizontal, 17.5).padding(.vertical, 8.75)
        }
    }
}

struct SectionRule: View {
    let label: String
    var sidebar = false
    var snoozed = false

    var body: some View {
        HStack(spacing: 8.75) {
            Text(label).font(AppTheme.font(13, weight: .medium))
                .foregroundStyle(snoozed ? AppTheme.muted : AppTheme.tertiary)
            Rectangle().fill(snoozed ? AppTheme.primary.opacity(0.2) : AppTheme.border).frame(height: 1)
        }
        .padding(.horizontal, sidebar ? 10.5 : 17.5)
        .padding(.top, 14).padding(.bottom, 5.25)
    }
}

struct ShelfHeaderView: View {
    let label: String
    let shelf: ShelfHeader
    let sidebar: Bool
    let toggle: () -> Void

    var body: some View {
        Button(action: toggle) {
            HStack(spacing: 8.75) {
                Text(shelf.expanded ? label : "\(label) (\(shelf.count))")
                    .font(AppTheme.font(13, weight: .medium))
                    .foregroundStyle(label == "Snoozed" ? AppTheme.muted : AppTheme.tertiary)
                Rectangle().fill(label == "Snoozed" ? AppTheme.primary.opacity(0.2) : AppTheme.border)
                    .frame(height: 1)
                Image(systemName: "chevron.down").font(.system(size: 10))
                    .rotationEffect(.degrees(shelf.expanded ? 180 : 0))
                    .foregroundStyle(AppTheme.tertiary)
            }
            .padding(.horizontal, sidebar ? 10.5 : 17.5)
            .padding(.top, 14).padding(.bottom, 5.25)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(shelf.disabled)
        .accessibilityLabel("\(shelf.count) \(label.lowercased()) thread\(shelf.count == 1 ? "" : "s")")
    }
}

struct ShowMoreSettled: View {
    let count: UInt32
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Text("Show more (\(count) settled hidden)")
                .font(AppTheme.font(13, weight: .medium)).foregroundStyle(AppTheme.muted)
                .frame(maxWidth: .infinity).padding(.vertical, 8.75)
                .overlay(RoundedRectangle(cornerRadius: 7).strokeBorder(
                    AppTheme.border, style: StrokeStyle(lineWidth: 1, dash: [4, 3])
                ))
        }
        .buttonStyle(.plain)
        .padding(.horizontal, 14).padding(.top, 7)
    }
}

/// A thread's menu records as SwiftUI menu entries.
struct ThreadMenuItems: View {
    let items: [ThreadMenuItem]
    let run: (ThreadMenuAction) -> Void

    var body: some View {
        ForEach(Array(items.enumerated()), id: \.offset) { _, item in
            if item.separatorBefore {
                Divider()
            }
            if item.children.isEmpty {
                Button(role: item.destructive ? .destructive : nil) {
                    if let action = item.action {
                        run(action)
                    }
                } label: {
                    Label(item.label, systemImage: item.id.symbol)
                }
                .disabled(!item.enabled)
            } else {
                Menu {
                    ForEach(Array(item.children.enumerated()), id: \.offset) { _, child in
                        Button { run(child.action) } label: {
                            if child.checked == true {
                                Label(child.label, systemImage: "checkmark")
                            } else {
                                Text(child.label)
                            }
                            if let detail = child.detail {
                                Text(detail)
                            }
                        }
                    }
                } label: {
                    Label(item.label, systemImage: item.id.symbol)
                }
            }
        }
    }
}

extension ThreadMenuItemId {
    var symbol: String {
        switch self {
        case .newThreadOnBranch, .rename: "square.and.pencil"
        case .copyThreadId, .copy, .copyPath, .copyBranch: "doc.on.doc"
        case .settle: "checkmark"
        case .unsettle: "arrow.uturn.backward"
        case .snooze, .snoozePreset, .snoozeCustom, .unsnooze: "clock"
        case .arrange: "line.3.horizontal"
        case .moveUp: "arrow.up"
        case .moveDown: "arrow.down"
        case .pin: "pin"
        case .unpin: "pin.slash"
        case .regenerateTitle: "arrow.clockwise"
        case .autoSettle, .autoSettleEnabled, .autoSettleDisabled: "timer"
        case .markUnread: "circle.fill"
        case .archive: "archivebox"
        case .delete: "trash"
        case .filterByProject: "line.3.horizontal.decrease"
        case .projectSettings: "gearshape"
        }
    }
}
