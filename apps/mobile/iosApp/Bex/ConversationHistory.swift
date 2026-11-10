import AgentCore
import SwiftUI
import UIKit

extension ConversationModel {
    func mediaAccess(presentation: ConversationMediaPresentation,
                     open: @escaping (WorkspaceDestination) -> Void) -> ConversationMediaAccess {
        let draft = coreDraftKey
        let cwd = cwd
        return ConversationMediaAccess(
            host: host,
            cwd: cwd,
            home: hostHomeDirectory,
            draft: draft,
            presentation: presentation,
            download: { [self] path in
                return try await download(path)
            },
            visualization: { [self] path in
                let result = try await outcome(for: .loadVisualization(LoadVisualization(path: path, cwd: cwd)))
                guard case let .visualization(html) = result else { throw URLError(.badServerResponse) }
                return html
            },
            resolve: { [self, home = hostHomeDirectory] source in
                try await resolveConversationFile(source, cwd: cwd, home: home)
            },
            openDirectory: { path in
                guard self.coreDraftKey == draft else { return }
                open(WorkspaceDestination(
                    scope: self.snapshot.panelScope(host: self.host, draft: draft), directory: path
                ))
            },
            openFile: { path, line in
                guard self.coreDraftKey == draft else { return }
                open(WorkspaceDestination(
                    scope: self.snapshot.panelScope(host: self.host, draft: draft), directory: cwd, filePath: path,
                    fileLine: line
                ))
            }
        )
    }

    private func resolveConversationFile(_ source: String, cwd: String,
                                         home: String?) async throws -> ConversationMediaAccess.ResolvedFile {
        guard let request = mobileFileOpenRequest(source: source, cwd: cwd, home: home)
        else { throw URLError(.unsupportedURL) }
        let result = try await outcome(for: .resolveFileReference(request))
        guard case let .fileReferenceResolved(path, directory) = result
        else { throw URLError(.badServerResponse) }
        return ConversationMediaAccess.ResolvedFile(
            path: path, isDirectory: directory,
            line: markdownFileTarget(source: source, cwd: cwd, surface: .mobile)?.line
        )
    }
}

/// History and row presentation
extension ThreadScreen {
    var mediaAccess: ConversationMediaAccess {
        model.mediaAccess(presentation: mediaPresentation) { openedDirectory = $0 }
    }

}
