import AgentCore
import Foundation

typealias SnapshotRequest = (Intent, @escaping (AgentCore.Snapshot, Result<Outcome, Error>) -> Void) -> Void

extension BexAppViewModel {
    var isConnected: Bool {
        snapshot.connected()
    }

    var selectedProfileName: String? {
        profiles.first { $0.id == selectedProfileId }?.name
    }

    var selectedThreadId: String? {
        snapshot.selectedThreadId()
    }

    var cwd: String {
        snapshot.currentDirectory()
    }

    var draft: String {
        get { composerText }
        set { editDraft(newValue) }
    }

    func recordScene(_ value: UInt64) {
        store?.recordConnectionEvent(phase: .appScene, value: value)
    }

    func recordListViewUpdate() {
        store?.recordConnectionEvent(phase: .listViewUpdated, value: isConnected ? 1 : 0)
    }

    func requestSnapshot(_ intent: Intent, completion: @escaping (AgentCore.Snapshot, Result<Outcome, Error>) -> Void) {
        perform(intent) { [self] result in completion(snapshot, result) }
    }

    func editDraft(_ text: String) {
        composerText = text
        let edit = draftEdits.edit(text)
        perform(.editDraft(text: text, baseText: edit.base)) { [weak self] _ in
            guard let self, draftEdits.pending == edit.revision else { return }
            _ = draftEdits.acknowledge(edit.revision)
            composerText = snapshot.draft().text
            draftEdits.base = composerText
        }
    }

    /// Adds the template's prompt to the draft and brings the composer up with the cursor after it.
    func useArtifactTemplate(_ template: ArtifactTemplate) {
        let next = appendArtifactTemplateUsePrompt(draft: composerText, template: template)
        if next != composerText {
            editDraft(next)
        }
        composerFocusRequests += 1
    }

    private func resetEditor() {
        draftEdits.reset()
    }

    func openPairing() {
        persist()
        connection?.cancel()
        isConnecting = false
        pairingError = nil
        pairingInvitation = nil
        screen = .pairing
    }

    func dismissPairing() {
        connection?.cancel()
        isConnecting = false
        pairingInvitation = nil
        screen = .profiles
    }

    func showProfiles() {
        persist(); screen = .profiles
    }

    func showThreadList() {
        resetEditor(); screen = .threads; perform(.leaveThread)
    }

    func openThread(_ id: String) {
        resetEditor(); screen = .thread; perform(.openThread(threadId: id))
    }

    func openNewThread(project: String? = nil) {
        resetEditor(); screen = .thread; perform(.newThread(projectId: project ?? snapshot.selectedProjectId()))
    }

    func send(alternate: Bool = false) {
        perform(.send(alternate: alternate))
    }

    func loadEarlier() {
        perform(.loadEarlier)
    }

    func scanned(_ contents: String?) {
        isScanning = false
        if let contents {
            preparePairing(contents)
        }
    }

    func browser(_ request: BrowserRequest) async throws -> BrowserFrame {
        guard let owner = store else { throw URLError(.notConnectedToInternet) }
        let host = selectedProfileId
        let frame = try await owner.browser(request: request)
        guard host == selectedProfileId else { throw CancellationError() }
        return frame
    }

    func download(_ path: String) async throws -> URL {
        guard let owner = store else { throw URLError(.notConnectedToInternet) }
        let host = selectedProfileId
        return try await inTemporaryDirectory { directory in
            let target = directory.appendingPathComponent(URL(fileURLWithPath: path).lastPathComponent)
            try await owner.downloadFile(source: path, destination: target.path)
            guard selectedProfileId == host else { throw CancellationError() }
            try Task.checkCancellation()
            return target
        }
    }

    func downloadAttachment(_ id: String, _ name: String) async throws -> URL {
        guard let owner = store else { throw URLError(.notConnectedToInternet) }
        let host = selectedProfileId
        return try await inTemporaryDirectory { directory in
            let target = directory.appendingPathComponent(URL(fileURLWithPath: name).lastPathComponent)
            try await owner.downloadAttachment(id: id, destination: target.path)
            guard selectedProfileId == host else { throw CancellationError() }
            try Task.checkCancellation()
            return target
        }
    }
}
