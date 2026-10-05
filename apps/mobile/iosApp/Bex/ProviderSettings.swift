import AgentCore
import SwiftUI

struct ProviderSettingsScreen: View {
    @ObservedObject var model: BexAppViewModel
    @State private var editor: ProviderEditor?
    @State private var driver = "codex"
    @State private var busy = false
    @State private var error: String?

    var body: some View {
        List {
            if let settings = model.snapshot.providerSettings() {
                Section("Providers") {
                    ForEach(settings.instances, id: \.instanceId) { instance in
                        Button {
                            editor = model.snapshot.providerEditor(instanceId: instance.instanceId, driver: "")
                        } label: {
                            HStack {
                                VStack(alignment: .leading) {
                                    Text(instance.config.displayName ?? instance.instanceId)
                                    Text("\(instance.instanceId) · \(instance.config.driver)")
                                        .font(.caption).foregroundStyle(.secondary)
                                }
                                Spacer()
                                if !instance.config.enabled {
                                    Text("無効").font(.caption)
                                }
                                Image(systemName: "chevron.right").font(.caption)
                            }
                        }
                        .accessibilityIdentifier("provider.edit.\(instance.instanceId)")
                    }
                }
                Section("Providerを追加") {
                    Picker("Driver", selection: $driver) {
                        Text("Codex").tag("codex")
                        Text("Claude").tag("claudeAgent")
                        if driver != "codex", driver != "claudeAgent" {
                            Text(driver).tag(driver)
                        }
                    }
                    TextField("Driver ID", text: $driver)
                        .textInputAutocapitalization(.never).autocorrectionDisabled()
                    Button("追加") {
                        editor = model.snapshot.providerEditor(instanceId: nil, driver: driver)
                        if editor == nil {
                            error = "Driver IDを確認してください。"
                        }
                    }.accessibilityIdentifier("provider.add")
                }
            }
            if busy {
                ProgressView()
            }
            if let error {
                Text(error).foregroundStyle(.red)
            }
            Button("再読み込み", action: load).disabled(busy)
        }
        .disabled(!model.isConnected)
        .safeAreaInset(edge: .top, spacing: 0) {
            SettingsScopeBar { Text("すべてのプロジェクト") } environment: {
                EnvironmentScopeMenu(model: model).disabled(busy || editor != nil)
            }
        }
        .navigationTitle("Providers")
        .task { load() }
        .sheet(isPresented: Binding(get: { editor != nil }, set: {
            if !$0 {
                editor = nil
            }
        })) {
            if let editor {
                NavigationStack { ProviderEditorScreen(model: model, editor: editor) }
            }
        }
    }

    private func load() {
        busy = true
        error = nil
        model.perform(.readProviderSettings(ReadProviderSettings())) { result in
            busy = false
            if case let .failure(failure) = result {
                error = failure.localizedDescription
            }
        }
    }
}

private struct ProviderEnvironmentInput: Identifiable {
    let id = UUID()
    var variable: EnvironmentVariable
}

private struct ProviderEditorScreen: View {
    @ObservedObject var model: BexAppViewModel
    @Environment(\.dismiss) private var dismiss
    @State var editor: ProviderEditor
    @State private var environment: [ProviderEnvironmentInput]
    @State private var busy = false
    @State private var error: String?
    @State private var confirmRemoval = false

    init(model: BexAppViewModel, editor: ProviderEditor) {
        self.model = model
        _environment = State(initialValue: editor.environment.map { ProviderEnvironmentInput(variable: $0) })
        var fields = editor
        fields.environment = []
        _editor = State(initialValue: fields)
    }

    var body: some View {
        Form {
            Section("Provider") {
                Text(model.selectedProfileName ?? "選択した環境")
                    .font(.caption).foregroundStyle(.secondary)
                TextField("ID", text: $editor.instanceId).disabled(editor.existingId != nil)
                    .accessibilityIdentifier("provider.id")
                LabeledContent("Driver", value: editor.driver)
                TextField("表示名", text: $editor.displayName).accessibilityIdentifier("provider.name")
                TextField("アクセントカラー", text: $editor.accentColor)
                Toggle("有効", isOn: $editor.enabled).accessibilityIdentifier("provider.enabled")
            }
            Section("実行設定") {
                ForEach(editor.fields.indices, id: \.self) { index in
                    let field = editor.fields[index]
                    if field.kind == .json {
                        Text(field.label).font(.caption)
                        TextEditor(text: $editor.fields[index].value).frame(minHeight: 88)
                            .font(.system(.caption, design: .monospaced))
                            .accessibilityIdentifier("provider.config.\(field.key)")
                    } else {
                        TextField(field.label, text: $editor.fields[index].value)
                            .accessibilityIdentifier("provider.config.\(field.key)")
                    }
                }
                if editor.fields.isEmpty {
                    TextEditor(text: $editor.configJson).frame(minHeight: 120)
                        .font(.system(.caption, design: .monospaced))
                        .accessibilityLabel("Driver設定のJSON")
                }
            }
            Section("環境変数") {
                ForEach($environment) { $input in
                    let id = input.id
                    VStack(alignment: .leading) {
                        TextField("名前", text: $input.variable.name)
                        let value = Binding(
                            get: { input.variable.value },
                            set: {
                                input.variable.value = $0
                                input.variable.valueRedacted = false
                            }
                        )
                        if input.variable.sensitive {
                            SecureField(input.variable.valueRedacted ? "保存済み（変更する場合だけ入力）" : "値", text: value)
                        } else {
                            TextField("値", text: value)
                        }
                        Toggle("秘密値", isOn: $input.variable.sensitive)
                        Button("変数を削除", role: .destructive) { environment.removeAll { $0.id == id } }
                    }
                }
                Button("環境変数を追加") {
                    environment.append(ProviderEnvironmentInput(variable: EnvironmentVariable(
                        name: "",
                        value: "",
                        sensitive: false,
                        valueRedacted: false
                    )))
                }
            }
            if let message = error {
                Section {
                    Text(message).foregroundStyle(.red).accessibilityIdentifier("provider.error")
                    Button("設定を再読み込み") {
                        busy = true
                        model.perform(.readProviderSettings(ReadProviderSettings())) { result in
                            busy = false
                            if case let .failure(failure) = result {
                                error = failure.localizedDescription
                            } else if let refreshed = model.snapshot.providerEditor(
                                instanceId: editor.existingId,
                                driver: editor.driver
                            ) {
                                environment = refreshed.environment.map { ProviderEnvironmentInput(variable: $0) }
                                var fields = refreshed
                                fields.environment = []
                                editor = fields
                                error = nil
                            } else {
                                error = "providerが削除されました。"
                            }
                        }
                    }
                }
            }
            if editor.existingId != nil {
                Button("Providerを削除", role: .destructive) { confirmRemoval = true }
                    .accessibilityIdentifier("provider.remove")
            }
        }
        .textInputAutocapitalization(.never).autocorrectionDisabled()
        .disabled(busy || !model.isConnected)
        .navigationTitle(editor.existingId == nil ? "Providerを追加" : "Provider設定")
        .toolbar {
            ToolbarItem(placement: .cancellationAction) { Button("キャンセル") { dismiss() }.disabled(busy) }
            ToolbarItem(placement: .confirmationAction) {
                Button("保存") { save(remove: false) }.disabled(busy || !model.isConnected)
                    .accessibilityIdentifier("provider.save")
            }
        }
        .confirmationDialog("このproviderを削除しますか？", isPresented: $confirmRemoval) {
            Button("削除", role: .destructive) { save(remove: true) }
        }
    }

    private func save(remove: Bool) {
        busy = true
        error = nil
        var request = editor
        request.environment = environment.map(\.variable)
        model.perform(.applyProviderEdit(ApplyProviderEdit(editor: request, remove: remove))) { result in
            busy = false
            if case let .failure(failure) = result {
                error = failure.localizedDescription
            } else {
                dismiss()
            }
        }
    }
}
