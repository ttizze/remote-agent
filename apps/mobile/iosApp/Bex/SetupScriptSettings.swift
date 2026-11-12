import AgentCore
import SwiftUI

struct SetupScriptSettings: View {
    @Binding var shared: SetupScript?
    @Binding var projects: [String: SetupScript]
    let project: String?

    private var script: SetupScript {
        project.flatMap { projects[$0] } ?? shared ?? SetupScript(name: "", command: "", asynchronous: false)
    }

    private var inherited: Bool {
        project.map { projects[$0] == nil } ?? false
    }

    var body: some View {
        Section("設定スクリプト") {
            if let project {
                Toggle("すべてのプロジェクトと同じ設定", isOn: Binding(
                    get: { projects[project] == nil },
                    set: { projects[project] = $0 ? nil : script }
                ))
            }
            Group {
                TextField("スクリプト名", text: Binding(get: { script.name }, set: { value in update { $0.name = value } }))
                TextField(
                    "実行するコマンド",
                    text: Binding(get: { script.command }, set: { value in update { $0.command = value } }),
                    axis: .vertical
                )
                .textInputAutocapitalization(.never).autocorrectionDisabled().font(.body.monospaced())
                Toggle(
                    "エージェントと並行して実行",
                    isOn: Binding(get: { script.asynchronous }, set: { value in update { $0.asynchronous = value } })
                )
            }.disabled(inherited)
            Text("オフの場合、スクリプトの成功を待ってからエージェントを開始します。")
                .font(.footnote).foregroundStyle(.secondary)
        }
    }

    private func update(_ change: (inout SetupScript) -> Void) {
        var value = script
        change(&value)
        if let project {
            projects[project] = value
        } else {
            shared = value
        }
    }
}
