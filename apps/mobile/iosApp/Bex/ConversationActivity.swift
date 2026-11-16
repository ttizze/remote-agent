import AgentCore
import SwiftUI
import UIKit
import UniformTypeIdentifiers

struct ConversationPlanRequest: Identifiable {
    enum Destination: Equatable { case export, workspace }
    let id = UUID()
    let destination: Destination
    let cwd: String
    let filename: String
    let markdown: String
}

struct ConversationEditRequest {
    let thread: SessionRef
    let turn: String
}

struct ThreadDecoratedActivity: View {
    let values: [ActivityDecoration]
    let blocks: [ConversationMarkdownContent.Part]
    let media: ConversationMediaAccess
    let selection: ConversationSelectionActions
    let canSave: Bool
    let open: (SessionRef) -> Void
    let plan: (ConversationPlanRequest.Destination, String, String) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            ForEach(values.indices, id: \.self) { index in
                switch values[index] {
                case let .plan(title, markdown, filename):
                    ConversationPlan(title: title, badge: String(localized: "計画")) {
                        ConversationMarkdown(blocks: blocks, media: media, selection: selection)
                    } actions: {
                        Menu {
                            Button { UIPasteboard.general.string = markdown } label: {
                                Label("クリップボードにコピー", systemImage: "doc.on.doc")
                            }
                            Button { plan(.export, filename, markdown) } label: {
                                Label("Markdownとして保存…", systemImage: "square.and.arrow.down")
                            }
                            Button { plan(.workspace, filename, markdown) } label: {
                                Label("ワークスペースに保存…", systemImage: "folder")
                            }.disabled(!canSave)
                        } label: { Image(systemName: "ellipsis") }
                            .accessibilityLabel("計画のメニュー")
                    }
                case let .subagent(id, target, name, message, status, elapsed):
                    ConversationSubagent(name: name, status: status, elapsed: elapsed, enabled: target != nil) {
                        if let target {
                            open(target)
                        }
                    } message: {
                        if !message.isEmpty {
                            Text(verbatim: message).textSelection(.enabled)
                        }
                    }
                    .accessibilityIdentifier("subagent." + id)
                case let .divider(label):
                    ConversationDivider(label: label)
                }
            }
        }
        .buttonStyle(.plain)
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

private struct PlanDocument: FileDocument {
    static var readableContentTypes: [UTType] {
        [.markdown, .plainText]
    }

    let markdown: String
    init(_ markdown: String) {
        self.markdown = markdown
    }

    init(configuration: ReadConfiguration) throws {
        guard let data = configuration.file.regularFileContents, let text = String(data: data, encoding: .utf8)
        else { throw CocoaError(.fileReadCorruptFile) }
        markdown = text
    }

    func fileWrapper(configuration _: WriteConfiguration) throws -> FileWrapper {
        FileWrapper(regularFileWithContents: Data(markdown.utf8))
    }
}

struct ConversationPlanPresenter: ViewModifier {
    @Binding var request: ConversationPlanRequest?
    let write: (String, String, String, @escaping (Result<Void, Error>) -> Void) -> Void
    @State private var error: String?

    private var exporting: Binding<Bool> {
        Binding(
            get: { request?.destination == .export },
            set: {
                if !$0, request?.destination == .export {
                    request = nil
                }
            }
        )
    }

    private var workspace: Binding<ConversationPlanRequest?> {
        Binding(get: { request?.destination == .workspace ? request : nil }, set: { request = $0 })
    }

    func body(content: Content) -> some View {
        content
            .fileExporter(
                isPresented: exporting,
                document: request.map { PlanDocument($0.markdown) },
                contentType: .markdown,
                defaultFilename: request?.filename ?? "plan.md"
            ) { result in
                if case let .failure(failure) = result {
                    error = failure.localizedDescription
                }
            }
            .sheet(item: workspace) { request in PlanWorkspaceForm(request: request, write: write) }
            .alert("保存できませんでした", isPresented: Binding(get: { error != nil }, set: {
                if !$0 {
                    error = nil
                }
            })) {
                Button("閉じる") { error = nil }
            } message: { Text(verbatim: error ?? "") }
    }
}

private struct PlanWorkspaceForm: View {
    @Environment(\.dismiss) private var dismiss
    let request: ConversationPlanRequest
    let write: (String, String, String, @escaping (Result<Void, Error>) -> Void) -> Void
    @State private var path: String
    @State private var busy = false
    @State private var failure: String?

    init(
        request: ConversationPlanRequest,
        write: @escaping (String, String, String, @escaping (Result<Void, Error>) -> Void) -> Void
    ) {
        self.request = request; self.write = write; _path = State(initialValue: request.filename)
    }

    var body: some View {
        NavigationStack {
            Form {
                Text(verbatim: request.cwd).font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
                TextField("ワークスペース内のパス", text: $path)
                    .textInputAutocapitalization(.never).autocorrectionDisabled().disabled(busy)
                if let failure {
                    Text(verbatim: failure).foregroundStyle(.red).textSelection(.enabled)
                }
            }
            .navigationTitle("ワークスペースに保存…")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("キャンセル") { dismiss() }.disabled(busy) }
                ToolbarItem(placement: .confirmationAction) {
                    Button("保存") {
                        busy = true; failure = nil
                        write(request.cwd, path, request.markdown) { result in
                            busy = false
                            switch result {
                            case .success: dismiss()
                            case let .failure(error): failure = error.localizedDescription
                            }
                        }
                    }.disabled(busy || path.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }
            }
            .interactiveDismissDisabled(busy)
        }
    }
}
