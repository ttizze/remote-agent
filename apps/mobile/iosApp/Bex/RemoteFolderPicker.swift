import AgentCore
import Foundation
import SwiftUI

/// Remote Host folder selection.
/// Browses directories on the connected Host. The path is sent through the
/// same core folder-browser projection used by the Add Project flow, so a
/// device-local folder can never be persisted as a Host project directory.
struct RemoteFolderPicker: View {
    @ObservedObject var model: BexAppViewModel
    let initialPath: String?
    let onSelect: (String) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var path = ""
    @State private var busy = false
    @State private var error: String?

    var body: some View {
        let browser = model.snapshot.folderBrowser(query: path)
        NavigationStack {
            VStack(spacing: 0) {
                HStack(spacing: 8) {
                    TextField("Host folder path", text: $path)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .textFieldStyle(.roundedBorder)
                    Button("Browse") { requestListing(path) }
                        .disabled(busy)
                }
                .padding()
                if let error {
                    NoticeText(text: error).padding(.horizontal)
                }
                if busy {
                    ProgressView().padding(.bottom)
                }
                List {
                    Button("Use \(browser.directoryPath)") {
                        onSelect(browser.directoryPath)
                        dismiss()
                    }
                    .disabled(!browser.isBrowsing || !browser.listed || busy)
                    if let parent = browser.parentQuery {
                        Button("..") { requestListing(parent) }
                    }
                    ForEach(browser.entries, id: \.query) { entry in
                        Button {
                            requestListing(entry.query)
                        } label: {
                            Label(entry.name, systemImage: "folder")
                        }
                    }
                }
                .listStyle(.plain)
            }
            .navigationTitle("Choose Host folder")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
            }
        }
        .onAppear {
            if path.isEmpty {
                path = initialFolderQuery(initialPath)
                requestListing(path)
            }
        }
    }

    func requestListing(_ query: String) {
        let browser = model.snapshot.folderBrowser(query: query)
        path = query
        error = nil
        guard browser.isBrowsing else {
            error = "Connect to a Host to browse its folders."
            return
        }
        guard !browser.listed else { return }
        busy = true
        model.requestSnapshot(.listFiles(path: browser.directoryPath)) { _, result in
            busy = false
            if case let .failure(failure) = result {
                error = failure.localizedDescription
            }
        }
    }

    func initialFolderQuery(_ value: String?) -> String {
        let trimmed = value?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        if trimmed.isEmpty {
            return "~/"
        }
        return trimmed.hasSuffix("/") || trimmed.hasSuffix("\\") ? trimmed : trimmed + "/"
    }
}
