import AgentCore
import SwiftUI
import WebKit

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
