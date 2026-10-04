import SwiftUI
import WebKit

struct ConversationVisualization: View {
    let path: String
    let media: ConversationMediaAccess
    @State private var html: String?
    @State private var error: String?
    @State private var expanded = false

    var body: some View {
        VStack(alignment: .leading) {
            if let html {
                Button("インタラクティブ表示を拡大") { expanded = true }
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
        .sheet(isPresented: $expanded) {
            NavigationStack {
                if let html {
                    VisualizationWebView(html: html)
                        .toolbar {
                            ToolbarItem(placement: .confirmationAction) {
                                Button("閉じる") { expanded = false }
                            }
                        }
                }
            }
        }
    }
}

/// HTML receives no native message handlers, cookies, file origin or popup capability.
struct VisualizationWebView: UIViewRepresentable {
    let html: String
    func makeCoordinator() -> Coordinator {
        Coordinator()
    }

    func makeUIView(context: Context) -> WKWebView {
        let configuration = WKWebViewConfiguration()
        configuration.websiteDataStore = .nonPersistent()
        configuration.preferences.javaScriptCanOpenWindowsAutomatically = false
        let view = WKWebView(frame: .zero, configuration: configuration)
        view.navigationDelegate = context.coordinator
        view.isOpaque = false
        view.accessibilityIdentifier = "visualize.webview"
        return view
    }

    func updateUIView(_ view: WKWebView, context: Context) {
        guard context.coordinator.html != html else { return }
        context.coordinator.html = html
        view.loadHTMLString(html, baseURL: nil)
    }

    static func dismantleUIView(_ view: WKWebView, coordinator _: Coordinator) {
        view.stopLoading()
        view.navigationDelegate = nil
    }

    final class Coordinator: NSObject, WKNavigationDelegate {
        var html: String?
        func webView(_: WKWebView, decidePolicyFor action: WKNavigationAction,
                     decisionHandler: @escaping (WKNavigationActionPolicy) -> Void) {
            let url = action.request.url
            decisionHandler(url?.scheme == "about" && action.navigationType == .other ? .allow : .cancel)
        }
    }
}
