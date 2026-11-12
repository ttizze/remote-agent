import Foundation
import SwiftUI

/// Native text editing preserves whitespace until Core normalizes a submitted form.
public struct WorktreeSettingsInputs: Codable, Equatable, Sendable {
    public var createOnNewSession: Bool
    public var copyOnCreate: Bool
    public var copyPathsText: String
    public var worktreeDirectory: String
    public var deleteUnchanged: Bool
    public var projectDeleteUnchanged: [String: Bool]
    public var copyPaths: [String] {
        copyPathsText.components(separatedBy: .newlines)
    }

    public init(createOnNewSession: Bool = false, copyOnCreate: Bool = false, copyPaths: [String] = [],
                worktreeDirectory: String = "", deleteUnchanged: Bool = false,
                projectDeleteUnchanged: [String: Bool] = [:]) {
        self.createOnNewSession = createOnNewSession
        self.copyOnCreate = copyOnCreate
        copyPathsText = copyPaths.joined(separator: "\n")
        self.worktreeDirectory = worktreeDirectory
        self.deleteUnchanged = deleteUnchanged
        self.projectDeleteUnchanged = projectDeleteUnchanged
    }

    private enum CodingKeys: String, CodingKey {
        case createOnNewSession, copyOnCreate, copyPaths, worktreeDirectory, deleteUnchanged, projectDeleteUnchanged
    }

    public init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        try self.init(createOnNewSession: values.decode(Bool.self, forKey: .createOnNewSession),
                      copyOnCreate: values.decode(Bool.self, forKey: .copyOnCreate),
                      copyPaths: values.decode([String].self, forKey: .copyPaths),
                      worktreeDirectory: values.decode(String.self, forKey: .worktreeDirectory),
                      deleteUnchanged: values.decode(Bool.self, forKey: .deleteUnchanged),
                      projectDeleteUnchanged: values.decode([String: Bool].self, forKey: .projectDeleteUnchanged))
    }

    public func encode(to encoder: Encoder) throws {
        var values = encoder.container(keyedBy: CodingKeys.self)
        try values.encode(createOnNewSession, forKey: .createOnNewSession)
        try values.encode(copyOnCreate, forKey: .copyOnCreate)
        try values.encode(copyPaths, forKey: .copyPaths)
        try values.encode(worktreeDirectory, forKey: .worktreeDirectory)
        try values.encode(deleteUnchanged, forKey: .deleteUnchanged)
        try values.encode(projectDeleteUnchanged, forKey: .projectDeleteUnchanged)
    }
}

public struct WorktreeSettingsFields: View {
    @Binding var inputs: WorktreeSettingsInputs
    let project: String?
    let enabled: Bool

    public init(inputs: Binding<WorktreeSettingsInputs>, project: String?, enabled: Bool) {
        _inputs = inputs
        self.project = project
        self.enabled = enabled
    }

    public var body: some View {
        Group {
            Section {
                Text(LocalizedStringKey(project == nil ? "この環境に保存し、新しい会話に適用します。" : "このプロジェクトの設定を変更します。"))
                    .font(.footnote).foregroundStyle(.secondary)
            }
            if let project {
                projectCleanup(project)
            } else {
                sharedSettings
            }
        }.disabled(!enabled)
    }

    @ViewBuilder
    private var sharedSettings: some View {
        Section {
            Toggle("新規セッションをワークツリーで開始", isOn: $inputs.createOnNewSession)
                .accessibilityIdentifier("worktree.create")
            TextField("この環境の絶対パス（空欄で既定）", text: $inputs.worktreeDirectory)
                .hostPathInput()
                .accessibilityLabel("ワークツリーの保存先").accessibilityIdentifier("worktree.directory")
        } header: { Text("作成と保存先") }
            footer: { Text("保存先に「セッション名/リポジトリ名」の構成で作ります。空欄なら元のリポジトリ内の .worktree に保存します。既存のワークツリーは移動しません。") }
        Section {
            Toggle("作成時にファイルをコピー", isOn: $inputs.copyOnCreate)
                .accessibilityIdentifier("worktree.copy")
            TextEditor(text: $inputs.copyPathsText).frame(minHeight: 88).hostPathInput()
                .accessibilityLabel("コピー対象").accessibilityIdentifier("worktree.paths")
        } header: { Text("コピー対象") }
            footer: { Text("リポジトリからの相対パスを1行に1つ指定します（例: .env、config/local）。存在しないパスはスキップします。") }
        Section {
            Toggle("取り込み済みを自動削除", isOn: $inputs.deleteUnchanged)
                .accessibilityIdentifier("worktree.deleteUnchanged")
        } header: { Text("自動削除") }
            footer: { Text("既定ブランチより先のコミットがない作業場所を1時間ごとに削除します。実行中・回答待ち・ターミナル使用中・ローカル変更ありの場合は保留します。ブランチと会話履歴は残ります。")
            }
    }

    private func projectCleanup(_ project: String) -> some View {
        Section {
            Picker("取り込み済みを自動削除", selection: Binding<Bool?>(
                get: { inputs.projectDeleteUnchanged[project] },
                set: { inputs.projectDeleteUnchanged[project] = $0 }
            )) {
                Text(LocalizedStringKey(inputs.deleteUnchanged ? "すべてのプロジェクトと同じ（オン）" : "すべてのプロジェクトと同じ（オフ）"))
                    .tag(Bool?.none)
                Text("オン").tag(Optional(true))
                Text("オフ").tag(Optional(false))
            }.accessibilityIdentifier("worktree.projectDeleteUnchanged")
        } header: { Text("自動削除") }
            footer: { Text("このプロジェクトの作業場所に適用します。") }
    }
}

private extension View {
    func hostPathInput() -> some View {
        autocorrectionDisabled()
        #if os(iOS)
            .textInputAutocapitalization(.never)
        #endif
    }
}
