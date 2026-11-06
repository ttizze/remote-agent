import SwiftUI
import WebKit

struct ConversationVisualization: View {
    let path: String
    let media: ConversationMediaAccess
    @State private var html: String?
    @State private var error: String?

    var body: some View {
        VStack(alignment: .leading) {
            if let html {
                Button("インタラクティブ表示を拡大") {
                    media.presentation.visualization = .init(html: html)
                }
                .accessibilityIdentifier("visualize.expand")
                VisualizationWebView(html: html).frame(height: 420)
            } else if let error {
                Text("表示できません: \(error)").font(.caption)
            } else {
                ProgressView("インタラクティブ表示を読み込み中…")
            }
        }
        .task(id: "\(media.host ?? ""):\(media.cwd):\(path)") {
            html = nil; error = nil
            do {
                let document = try await media.visualization(path)
                try Task.checkCancellation()
                html = document
            } catch {
                if !Task.isCancelled {
                    self.error = error.localizedDescription
                }
            }
        }
    }
}

/// HTML receives no native message handlers, cookies, file origin or popup capability.
struct VisualizationWebView: View {
    let html: String
    @State private var page: WebPage?

    var body: some View {
        ZStack {
            if let page {
                WebView(page).webViewContentBackground(.hidden)
            } else {
                ProgressView()
            }
        }
        .accessibilityIdentifier("visualize.webview")
        .task(id: html) {
            let page = page ?? WebPage(configuration: Self.configuration, navigationDecider: InitialDocumentOnly())
            self.page = page
            page.load(html: html)
        }
    }

    private static var configuration: WebPage.Configuration {
        var configuration = WebPage.Configuration()
        configuration.websiteDataStore = .nonPersistent()
        return configuration
    }
}

/// Pages stay on the document they were given; links and scripted navigation do not leave it.
struct InitialDocumentOnly: WebPage.NavigationDeciding {
    @MainActor func decidePolicy(for action: WebPage.NavigationAction,
                                 preferences _: inout WebPage.NavigationPreferences) async -> WKNavigationActionPolicy {
        action.request.url?.scheme == "about" && action.navigationType == .other ? .allow : .cancel
    }
}
