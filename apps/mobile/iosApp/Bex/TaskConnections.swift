import SwiftUI

struct TaskConnectionEmptyState: View {
    let connect: () -> Void

    var body: some View {
        ContentUnavailableView {
            Label("作業する環境を接続", systemImage: "laptopcomputer")
        } description: {
            Text("接続すると、プロジェクトとタスクがここに表示されます")
        } actions: {
            Button("環境に接続", action: connect)
                .buttonStyle(.borderedProminent)
                .accessibilityIdentifier("tasks.connect")
            Text("QRコード・接続情報を使う").font(.footnote).foregroundStyle(.secondary)
        }
    }
}

struct TaskConnectionsMenu: View {
    let profiles: [HostProfile]
    let selected: String?
    let all: Bool
    let connecting: Bool
    let loading: Bool
    let connected: Bool
    let select: (String?) -> Void

    var body: some View {
        Menu {
            // Profile identifiers are never empty, so the empty tag stands for every connection.
            Picker("接続先", selection: Binding<String?>(get: { all ? "" : selected }, set: {
                select($0.flatMap { $0.isEmpty ? nil : $0 })
            })) {
                Section {
                    Label("すべての接続先", systemImage: "square.stack").tag(Optional(""))
                        .accessibilityIdentifier("tasks.connections.all")
                }
                Section {
                    ForEach(profiles) { profile in
                        Text(profile.name).tag(Optional(profile.id))
                            .accessibilityIdentifier("tasks.connections.\(profile.id)")
                    }
                }
            }
            .pickerStyle(.inline)
            .labelsHidden()
        } label: {
            HStack(spacing: 5) {
                if connecting || loading {
                    ProgressView().controlSize(.mini)
                        .tint(connecting ? .red : .green)
                        .accessibilityLabel(LocalizedStringKey(connecting ? "接続中" : "読み込み中"))
                        .accessibilityIdentifier("connection.progress")
                } else {
                    Circle().fill(connected ? Color.blue : Color.red).frame(width: 6, height: 6)
                }
                Image(systemName: all ? "square.stack" : "laptopcomputer")
                if all {
                    Text("すべての接続先").lineLimit(1)
                } else {
                    Text(profiles.first { $0.id == selected }?.name ?? "PC Host").lineLimit(1)
                }
                Image(systemName: "chevron.down").font(.caption)
            }
            .font(.caption)
            .foregroundStyle(.secondary)
            .accessibilityElement(children: .combine)
        }
        .accessibilityIdentifier("tasks.connections")
        .accessibilityValue(Text(LocalizedStringKey(connecting ? "接続中" : connected ? "接続済み" : "未接続")))
    }
}
