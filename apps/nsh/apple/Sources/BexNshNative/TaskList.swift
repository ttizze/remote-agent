import Foundation
#if os(iOS)
    import SwiftUI
#endif
#if os(iOS)
    import UIKit
#else
    import AppKit
#endif

/// Core supplies ordered rows, expansion, actions and status priority.
/// This boundary only groups native list children and renders OS controls.
struct TaskListData: Decodable {
    let rows: [TaskRow]
    let menu: [TaskMenuItem]

    var groups: [TaskGroup] {
        var result: [TaskGroup] = []
        for row in rows {
            if row.level == 0 {
                result.append(TaskGroup(row: row, children: []))
            } else if !result.isEmpty {
                result[result.count - 1].children.append(row)
            }
        }
        return result
    }
}

struct TaskGroup: Identifiable {
    let row: TaskRow
    var children: [TaskRow]
    var id: String {
        row.id
    }
}

struct TaskRow: Decodable, Identifiable {
    struct Compose: Decodable {
        let id: String
        let enabled: Bool
    }

    struct Indicators: Decodable {
        let activity: String?
        let attention: String?
        let changes: Bool
    }

    let id: String
    let title: String
    let png: [UInt8]?
    let monogram: String?
    let color: UInt32?
    let level: UInt32
    let disclosure: Bool
    let expanded: Bool
    let group: Bool
    let menu: [TaskMenuItem]?
    let compose: Compose?
    let indicators: Indicators?
    let statusLabel: String?
    let loading: Bool
    let enabled: Bool
    let passive: Bool
}

struct TaskMenuItem: Decodable {
    let id: String?
    let title: String?
    let selected: Bool?
    let enabled: Bool?
    let separator: Bool?
    let children: [TaskMenuItem]?
}

#if os(iOS)
private struct TaskMenuEntries: View {
    let items: [TaskMenuItem]
    let send: ([String: String]) -> Void

    var body: some View {
        ForEach(Array(items.enumerated()), id: \.offset) { _, item in
            if item.separator == true {
                Divider()
            } else if let title = item.title {
                if let children = item.children {
                    Menu(title) { TaskMenuEntries(items: children, send: send) }
                } else if let id = item.id {
                    Button { send(["action": "menu", "id": id]) } label: {
                        if item.selected == true {
                            Label(title, systemImage: "checkmark")
                        } else {
                            Text(verbatim: title)
                        }
                    }.disabled(item.enabled == false)
                }
            }
        }
    }
}

struct TaskListView: View {
    let data: TaskListData
    let send: ([String: String]) -> Void

    var body: some View {
        List {
            Section {
                HStack {
                    Text("プロジェクト").font(.title2.weight(.bold))
                    Spacer()
                    if !data.menu.isEmpty {
                        Menu { TaskMenuEntries(items: data.menu, send: send) } label: {
                            Image(systemName: "folder.badge.plus")
                        }.accessibilityLabel("プロジェクトを管理")
                    }
                }.taskRow()
            }.listSectionSeparator(.hidden)

            ForEach(data.groups) { group in
                Section {
                    if group.row.disclosure {
                        DisclosureGroup(isExpanded: Binding(
                            get: { group.row.expanded },
                            set: { send(["action": "expand", "id": group.id, "expanded": String($0)]) }
                        )) {
                            ForEach(group.children) { row in
                                TaskListButton(row: row, send: send).taskRow(leading: 40)
                            }
                        } label: {
                            project(group.row)
                        }
                        .disclosureGroupStyle(TaskProjectDisclosureStyle())
                        .taskRow()
                    } else if group.row.group {
                        Text(verbatim: group.row.title).font(.title3.weight(.semibold)).taskRow()
                    } else {
                        TaskListButton(row: group.row, send: send).taskRow()
                    }
                }.listSectionSeparator(.hidden)
            }
        }
        .listStyle(.plain)
        .environment(\.defaultMinListRowHeight, 52)
        .accessibilityIdentifier("tasks.list")
    }

    private func project(_ row: TaskRow) -> some View {
        HStack(spacing: 16) {
            Button { send(["action": "expand", "id": row.id, "expanded": String(!row.expanded)]) } label: {
                HStack(spacing: 16) {
                    TaskProjectIcon(row: row)
                    Text(verbatim: row.title).font(.title3).lineLimit(1)
                    Spacer(minLength: 0)
                }.frame(minHeight: 44).contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier(row.id)
            .accessibilityValue(row.expanded ? "開いています" : "閉じています")
            if let compose = row.compose {
                Button { send(["action": "compose", "id": compose.id]) } label: {
                    Image(systemName: "square.and.pencil").font(.title3).foregroundStyle(Color.secondary)
                        .frame(width: 44, height: 44).contentShape(Rectangle())
                }
                .buttonStyle(.borderless)
                .accessibilityIdentifier(compose.id)
                .accessibilityLabel("\(row.title)で新しいタスク")
                .disabled(!compose.enabled)
            }
        }
        .contextMenu { TaskMenuEntries(items: row.menu ?? [], send: send) }
    }
}

private struct TaskProjectDisclosureStyle: DisclosureGroupStyle {
    func makeBody(configuration: Configuration) -> some View {
        Group {
            configuration.label
            if configuration.isExpanded {
                configuration.content
            }
        }
    }
}

private struct TaskListButton: View {
    let row: TaskRow
    let send: ([String: String]) -> Void

    var body: some View {
        Group {
            if row.passive {
                label.font(.subheadline).foregroundStyle(.secondary)
            } else {
                Button { send(["action": "select", "id": row.id]) } label: { label }
                    .font(.title3).foregroundStyle(.primary).buttonStyle(.plain).disabled(!row.enabled)
            }
        }
        .accessibilityIdentifier(row.id)
        .accessibilityValue(row.statusLabel ?? "")
    }

    private var label: some View {
        HStack(spacing: 10) {
            if row.loading {
                ProgressView().controlSize(.small)
            }
            Text(verbatim: row.title).lineLimit(row.passive ? nil : 1)
            Spacer()
            TaskStatus(indicators: row.indicators)
        }.contentShape(Rectangle())
    }
}

private struct TaskStatus: View {
    let indicators: TaskRow.Indicators?

    var body: some View {
        Group {
            // T3 Code's badge says what a waiting chat waits for.
            switch indicators?.attention {
            case "approval": Image(systemName: "shield").foregroundStyle(.yellow)
            case "input": Image(systemName: "questionmark.bubble").foregroundStyle(.blue)
            case "failed": Image(systemName: "exclamationmark.circle").foregroundStyle(.red)
            default: EmptyView()
            }
            switch indicators?.attention == nil ? indicators?.activity : nil {
            case "waiting": Image(systemName: "person.crop.circle.badge.questionmark").foregroundStyle(.orange)
            case "running": ProgressView().controlSize(.small)
            case "unread": Image(systemName: "circle.fill").resizable().frame(width: 8, height: 8)
            case "background":
                Image(systemName: "circle.fill").resizable().frame(width: 8, height: 8).foregroundStyle(.secondary)
            default: EmptyView()
            }
            if indicators?.changes == true {
                Image("GitDiff").resizable().frame(width: 18, height: 18).foregroundStyle(.orange)
            }
        }.accessibilityHidden(true)
    }
}

private struct TaskProjectIcon: View {
    let row: TaskRow

    var body: some View {
        let rgb = row.color ?? 0
        let color = Color(red: Double((rgb >> 16) & 255) / 255,
                          green: Double((rgb >> 8) & 255) / 255, blue: Double(rgb & 255) / 255)
        Group {
            if let bytes = row.png, let image = image(Data(bytes)) {
                image.resizable().scaledToFit()
            } else {
                Text(row.monogram ?? "").font(.system(size: 10, weight: .bold, design: .rounded))
                    .foregroundStyle(color).frame(maxWidth: .infinity, maxHeight: .infinity)
                    .background(color.opacity(0.15))
            }
        }
        .frame(width: 24, height: 24)
        .clipShape(RoundedRectangle(cornerRadius: 6))
        .accessibilityHidden(true)
    }

    private func image(_ data: Data) -> Image? {
        #if os(iOS)
            UIImage(data: data).map { Image(uiImage: $0) }
        #else
            NSImage(data: data).map { Image(nsImage: $0) }
        #endif
    }
}

private extension View {
    func taskRow(leading: CGFloat = 20) -> some View {
        listRowSeparator(.hidden)
            .listRowInsets(EdgeInsets(top: 0, leading: leading, bottom: 0, trailing: 20))
            .listRowBackground(Color.clear)
    }
}

#endif
