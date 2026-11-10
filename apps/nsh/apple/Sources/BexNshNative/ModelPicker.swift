import Foundation
#if os(iOS)
    import SwiftUI
#endif

struct ModelPickerData: Decodable {
    struct Choice: Decodable, Identifiable {
        let id: String
        let title: String
    }

    struct Quick: Decodable {
        let efforts: [String]
        let effort: String
        let speeds: [Choice]
        let speed: String?
    }

    struct Permissions: Decodable {
        let selected: String?
        let choices: [Choice]
    }

    struct Notice: Decodable, Identifiable {
        let id: String
        let title: String
        let text: String
        let recovery: String?
    }

    struct Usage: Decodable {
        let label: String
        let remainingPercent: UInt32
    }

    struct Account: Decodable {
        let title: String
        let asset: String
        let usage: [Usage]
    }

    let id: String
    let provider: String?
    let query: String
    let disabled: Bool
    let providers: [Choice]
    let models: [Choice]
    let selectedModel: String?
    let loading: Bool
    let errors: [String]
    let notices: [Notice]
    let customModel: String?
    let quick: Quick?
    let permissions: Permissions?
    let account: Account?
}

#if os(iOS)
struct ModelPickerPresenter: View {
    let data: ModelPickerData?
    let send: ([String: String]) -> Void

    var body: some View {
        Color.clear.sheet(isPresented: Binding(get: { data != nil }, set: {
            if !$0, let data {
                send(["token": data.id, "action": "close"])
            }
        })) {
            if let data {
                ModelPickerForm(data: data, send: send).id(data.id)
            }
        }
    }
}

private struct ModelPickerForm: View {
    let data: ModelPickerData
    let send: ([String: String]) -> Void

    var body: some View {
        NavigationStack {
            Form {
                ForEach(data.notices) { notice in
                    Section {
                        Text(verbatim: notice.text).textSelection(.enabled)
                        if let recovery = notice.recovery {
                            Button(recovery) { emit("recover", value: notice.id) }
                        }
                        Button("閉じる") { emit("dismiss", value: notice.id) }
                    } header: { Text(verbatim: notice.title) }
                }
                Section("エージェント") {
                    Picker("エージェント", selection: selection(data.provider, action: "provider")) {
                        ForEach(data.providers) { choice in
                            Text(verbatim: choice.title).tag(Optional(choice.id))
                        }
                    }
                    .pickerStyle(.segmented).disabled(data.disabled)
                    .accessibilityIdentifier("model.provider")
                }
                Section("モデル") {
                    if data.models.isEmpty {
                        Text(data.loading ? "モデルを読み込み中…" : "利用可能なモデルがありません")
                            .foregroundStyle(.secondary)
                    }
                    Picker("モデル", selection: selection(data.selectedModel, action: "model")) {
                        ForEach(data.models) { choice in
                            Text(verbatim: choice.title).tag(Optional(choice.id))
                                .accessibilityIdentifier("model.choice." + choice.id)
                        }
                    }.pickerStyle(.inline).labelsHidden().disabled(data.disabled)
                    if let custom = data.customModel {
                        Button("「\(custom)」を使う") { emit("custom") }.disabled(data.disabled)
                    }
                    ForEach(data.errors, id: \.self) { error in
                        Text(verbatim: error).font(.caption).foregroundStyle(.red)
                            .textSelection(.enabled).accessibilityIdentifier("model.error")
                    }
                }
                if let quick = data.quick, !quick.efforts.isEmpty || quick.speeds.count > 1 {
                    Section("応答") {
                        if !quick.efforts.isEmpty {
                            Picker(
                                "思考の深さ",
                                selection: Binding(get: { quick.effort }, set: { emit("effort", value: $0) })
                            ) {
                                ForEach(quick.efforts, id: \.self) { Text(verbatim: $0).tag($0) }
                            }.accessibilityIdentifier("model.sheet.effort")
                        }
                        if quick.speeds.count > 1 {
                            Picker("速度", selection: selection(quick.speed, action: "speed")) {
                                ForEach(quick.speeds) { Text(verbatim: $0.title).tag(Optional($0.id)) }
                            }.accessibilityIdentifier("model.sheet.fast")
                        }
                    }.disabled(data.disabled)
                }
                if let permissions = data.permissions {
                    Section("アクセス権限") {
                        Picker("アクセス権限", selection: selection(permissions.selected, action: "permission")) {
                            ForEach(permissions.choices) { Text(verbatim: $0.title).tag(Optional($0.id)) }
                        }
                    }.disabled(data.disabled)
                }
                Section {
                    Button { emit("accounts") } label: {
                        if data.provider == "deepseek" {
                            Label("APIキー", systemImage: "key")
                        } else if let account = data.account {
                            HStack(spacing: 4) {
                                Image(account.asset).resizable().scaledToFit().frame(width: 12, height: 12)
                                    .accessibilityHidden(true)
                                Text(verbatim: account.title).lineLimit(1)
                            }
                        } else {
                            Text("未選択")
                        }
                    }.disabled(data.provider == nil).accessibilityIdentifier("model.accounts.manage")
                    if let account = data.account {
                        if account.usage.isEmpty {
                            Text("残量未取得").font(.caption).foregroundStyle(.secondary)
                        }
                        ForEach(account.usage.indices, id: \.self) { index in
                            let usage = account.usage[index]
                            HStack {
                                Text(verbatim: usage.label)
                                ProgressView(value: Double(usage.remainingPercent), total: 100)
                                    .tint(usage.remainingPercent <= 20 ? .orange : .accentColor)
                                Text("\(usage.remainingPercent)%").monospacedDigit()
                            }.font(.caption).accessibilityElement(children: .combine)
                        }
                    }
                } header: {
                    Text(data.provider == "deepseek" ? "接続" : "アカウント")
                } footer: {
                    Text("選んだモデルと応答設定は、次の新規チャットにも引き継ぎます。")
                }
            }
            .formStyle(.grouped).accessibilityIdentifier("model.picker")
            .navigationTitle("モデル設定")
                .navigationBarTitleDisplayMode(.inline)
                .searchable(text: search, placement: .navigationBarDrawer(displayMode: .always), prompt: "モデルを検索")
                .toolbar {
                    ToolbarItem(placement: .confirmationAction) {
                        Button("完了") { emit("close") }.keyboardShortcut(.cancelAction)
                            .accessibilityIdentifier("model.close")
                    }
                }
        }
        .presentationDetents([.large]).presentationDragIndicator(.visible)
    }

    private var search: Binding<String> {
        Binding(get: { data.query }, set: { emit("search", value: $0) })
    }

    private func selection(_ value: String?, action: String) -> Binding<String?> {
        Binding(get: { value }, set: {
            if let value = $0 {
                emit(action, value: value)
            }
        })
    }

    private func emit(_ action: String, value: String = "") {
        send(["token": data.id, "action": action, "value": value])
    }
}

#endif
