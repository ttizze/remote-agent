import AgentCore
import PDFKit
import SwiftUI
import UIKit

/// What the conversation does with tapped Markdown links and images.
struct MarkdownLinkOpener {
    /// The thread's workspace on the Host.
    let workspaceRoot: String?
    /// Opens a Host file, by absolute path, at a line.
    let openFile: (FileTarget) -> Void
    /// Opens a resource-backed PDF instead of sending it through the text reader.
    let openPDF: ((FileTarget) -> Void)?
    let loadFile: @MainActor (String) async throws -> URL
}

extension EnvironmentValues {
    @Entry var markdownLinks: MarkdownLinkOpener?
}

/// A Host file a link opened.
struct FileTarget: Identifiable, Equatable {
    /// The absolute Host path.
    let path: String
    /// The workspace-relative path, or the absolute one outside the workspace.
    let displayPath: String
    let line: UInt64?
    var id: String {
        "\(path):\(line ?? 0)"
    }
}

/// Links in rendered Markdown carry their raw destination under this scheme
/// so the conversation, not the system, decides what they open.
enum MarkdownLinkURL {
    private static let prefix = "markdown-link:"

    static func url(for href: String) -> URL? {
        href.addingPercentEncoding(withAllowedCharacters: .alphanumerics).flatMap { URL(string: prefix + $0) }
    }

    static func href(from url: URL) -> String? {
        let value = url.absoluteString
        guard value.hasPrefix(prefix) else { return nil }
        return String(value.dropFirst(prefix.count)).removingPercentEncoding
    }

    /// Opens a workspace or Host file in the app; web, mail and phone links outside it.
    @MainActor static func open(
        _ url: URL,
        links: MarkdownLinkOpener?,
        contextAction: ((String) -> Void)? = nil
    ) -> OpenURLAction.Result {
        guard let href = href(from: url) else { return .systemAction }
        if href.hasPrefix("context://") {
            contextAction?(href)
            return .handled
        }
        switch markdownLinkAction(href: href, workspaceRoot: links?.workspaceRoot) {
        case let .workspaceFile(path, line):
            return openWorkspaceFile(path: path, line: line, links: links)
        case let .hostFile(path, line):
            return openHostFile(path: path, line: line, links: links)
        case let .external(target):
            guard let target = URL(string: target) else { return .handled }
            return .systemAction(target)
        case .nothing:
            return .handled
        }
    }

    @MainActor private static func openWorkspaceFile(
        path: String,
        line: UInt64?,
        links: MarkdownLinkOpener?
    ) -> OpenURLAction.Result {
        guard let root = links?.workspaceRoot, let links else { return .handled }
        let absolutePath = (root as NSString).appendingPathComponent(path)
        return openFile(FileTarget(path: absolutePath, displayPath: path, line: line), links: links)
    }

    @MainActor private static func openHostFile(
        path: String,
        line: UInt64?,
        links: MarkdownLinkOpener?
    ) -> OpenURLAction.Result {
        guard let links else { return .handled }
        return openFile(FileTarget(path: path, displayPath: path, line: line), links: links)
    }

    @MainActor private static func openFile(
        _ target: FileTarget,
        links: MarkdownLinkOpener
    ) -> OpenURLAction.Result {
        Haptics.selection()
        if isPdfFile(path: target.displayPath), let openPDF = links.openPDF {
            openPDF(target)
        } else {
            links.openFile(target)
        }
        return .handled
    }
}

/// Remote and inline images load directly; workspace images use the authenticated Host.
/// Tapping a loaded image previews it.
struct MarkdownImage: View {
    let href: String
    let alt: String
    @Environment(\.markdownLinks) private var links
    @State private var image: UIImage?
    @State private var failed = false
    @State private var width: CGFloat = 0
    @State private var previewing = false

    var body: some View {
        let source = markdownImageSource(href: href, workspaceRoot: links?.workspaceRoot)
        VStack(alignment: .leading, spacing: 6) {
            Group {
                if let image {
                    Button { previewing = true } label: {
                        Image(uiImage: image).resizable().scaledToFit()
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel(alt.isEmpty ? "Markdown image" : alt)
                } else {
                    Group {
                        if failed {
                            Text("Image unavailable").font(AppTheme.font(13)).foregroundStyle(AppTheme.muted)
                        } else {
                            Text("Loading image…").font(AppTheme.font(13)).foregroundStyle(AppTheme.muted)
                        }
                    }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            }
            .frame(width: frame.width, height: frame.height)
            .background(AppTheme.color("mobileMarkdownCode"), in: RoundedRectangle(cornerRadius: 10))
            .clipShape(RoundedRectangle(cornerRadius: 10))
            if !alt.isEmpty {
                Text(alt).font(AppTheme.font(13)).foregroundStyle(AppTheme.muted).textSelection(.enabled)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { width = $0 }
        .task(id: source) { await load(source) }
        .fullScreenCover(isPresented: $previewing) {
            if let image {
                ImagePreview(image: image, name: alt.isEmpty ? "Image" : alt)
            }
        }
    }

    /// The drawn size once the image is decoded; a 16:9 placeholder before.
    private var frame: CGSize {
        let available = max(width, 1)
        if let image, let size = markdownImageDisplaySize(
            sourceWidth: image.size.width, sourceHeight: image.size.height, availableWidth: available
        ) {
            return CGSize(width: size.width, height: size.height)
        }
        let placeholder = min(available, 480)
        return CGSize(width: placeholder, height: placeholder * 9 / 16)
    }

    private func inlineImageData(_ uri: String) throws -> Data {
        let parts = uri.dropFirst(5).split(separator: ",", maxSplits: 1, omittingEmptySubsequences: false)
        guard parts.count == 2 else { throw URLError(.badURL) }
        if parts[0].hasSuffix(";base64") {
            guard let decoded = Data(base64Encoded: String(parts[1])) else { throw URLError(.cannotDecodeContentData) }
            return decoded
        }
        guard let decoded = String(parts[1]).removingPercentEncoding else { throw URLError(.badURL) }
        return Data(decoded.utf8)
    }

    private func load(_ source: MarkdownImageSource) async {
        image = nil
        failed = false
        do {
            let data: Data
            switch source {
            case let .workspaceFile(path):
                guard let links else { throw URLError(.notConnectedToInternet) }
                let file = try await links.loadFile(path)
                defer { try? FileManager.default.removeItem(at: file.deletingLastPathComponent()) }
                data = try await Task.detached(priority: .utility) { try Data(contentsOf: file) }.value
            case let .direct(uri):
                guard let url = URL(string: uri.hasPrefix("//") ? "https:" + uri : uri) else {
                    throw URLError(.badURL)
                }
                if uri.hasPrefix("data:") {
                    data = try inlineImageData(uri)
                } else {
                    data = try await URLSession.shared.data(from: url).0
                }
            case .blocked:
                throw URLError(.badURL)
            }
            guard let decoded = UIImage(data: data) else { throw URLError(.cannotDecodeContentData) }
            try Task.checkCancellation()
            image = decoded
        } catch {
            if !Task.isCancelled {
                failed = true
            }
        }
    }
}

private struct ImagePreview: View {
    let image: UIImage
    let name: String
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            Image(uiImage: image).resizable().scaledToFit()
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .background(Color.black.ignoresSafeArea())
                .navigationTitle(name)
                .navigationBarTitleDisplayMode(.inline)
                .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
        }
    }
}

/// A Host file a conversation link opened: read-only, scrolled to its line.
struct ThreadFileSheet: View {
    @ObservedObject var model: BexAppViewModel
    let target: FileTarget
    @Environment(\.dismiss) private var dismiss
    @State private var error: String?

    var body: some View {
        let file = model.snapshot.file().flatMap { $0.path == target.path ? $0 : nil }
        NavigationStack {
            Group {
                if let file {
                    source(file.text)
                } else if let error {
                    EmptyStateText(title: "File unavailable", detail: error)
                        .frame(maxHeight: .infinity)
                } else {
                    ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            }
            .background(AppTheme.screen.ignoresSafeArea())
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .principal) {
                    VStack(spacing: 1) {
                        Text(URL(fileURLWithPath: target.path).lastPathComponent)
                            .font(AppTheme.font(17, weight: .heavy))
                        Text(fileHeaderSubtitle(projectName: model.threadView?.header?.project?.name ?? "",
                                                path: target.displayPath))
                            .font(AppTheme.font(12)).foregroundStyle(AppTheme.muted).lineLimit(1)
                    }
                }
                ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } }
            }
        }
        .task(id: target) {
            model.perform(.readFile(path: target.path, discardDraft: false)) { result in
                if case let .failure(failure) = result {
                    error = failure.localizedDescription
                }
            }
        }
    }

    private func source(_ text: String) -> some View {
        let lines = text.components(separatedBy: "\n")
        let wrapping = AppTheme.codeWordWrap
        return ScrollViewReader { reader in
            ScrollView(wrapping ? .vertical : [.vertical, .horizontal]) {
                LazyVStack(alignment: .leading, spacing: 0) {
                    ForEach(Array(lines.enumerated()), id: \.offset) { index, line in
                        HStack(alignment: .top, spacing: 12) {
                            Text("\(index + 1)").font(.custom("Menlo", size: AppTheme.codeLineNumberFontSize))
                                .foregroundStyle(AppTheme.tertiary)
                                .frame(minWidth: 36, alignment: .trailing)
                            Text(line.isEmpty ? " " : line).font(AppTheme.mono(13)).foregroundStyle(AppTheme.text)
                                .fixedSize(horizontal: !wrapping, vertical: false)
                                .frame(maxWidth: wrapping ? .infinity : nil, alignment: .leading)
                        }
                        .padding(.horizontal, 12).padding(.vertical, 1)
                        .frame(
                            maxWidth: wrapping ? .infinity : nil,
                            minHeight: AppTheme.codeLineHeight,
                            alignment: .top
                        )
                        .background(UInt64(index + 1) == target.line ? AppTheme.primary.opacity(0.12) : .clear)
                        .id(index + 1)
                    }
                }
                .padding(.vertical, 8)
                .textSelection(.enabled)
            }
            .onAppear {
                if let line = target.line,
                   let targetLine = markdownLineTarget(line: line, lineCount: UInt64(lines.count)),
                   targetLine <= UInt64(Int.max) {
                    reader.scrollTo(Int(targetLine), anchor: .center)
                }
            }
        }
    }
}

/// A resource-backed Host PDF. It downloads the bytes and lets PDFKit render
/// them, so PDF Markdown links never enter the text-file reader.
struct ThreadPDFSheet: View {
    @ObservedObject var model: BexAppViewModel
    let target: FileTarget
    @Environment(\.dismiss) private var dismiss
    @State private var local: URL?
    @State private var error: String?

    var body: some View {
        NavigationStack {
            Group {
                if let local {
                    PDFDocumentView(url: local)
                } else if let error {
                    EmptyStateText(title: "PDF unavailable", detail: error)
                        .frame(maxHeight: .infinity)
                } else {
                    ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            }
            .background(AppTheme.screen.ignoresSafeArea())
            .navigationTitle(URL(fileURLWithPath: target.path).lastPathComponent)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
        }
        .task(id: target) {
            do {
                local = try await model.download(target.path)
            } catch {
                self.error = error.localizedDescription
            }
        }
        .onDisappear {
            if let local {
                try? FileManager.default.removeItem(at: local.deletingLastPathComponent())
            }
        }
    }
}

/// The captured output behind a terminal context link, with a route to its source terminal.
struct ContextPreviewSheet: View {
    let chip: ContextChip
    let openTerminal: () -> Void
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            ScrollView {
                Text(chip.previewText ?? "Context unavailable")
                    .font(AppTheme.terminalMono())
                    .foregroundStyle(AppTheme.text)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(16)
            }
            .background(AppTheme.screen.ignoresSafeArea())
            .navigationTitle(chip.label)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Done") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) { Button("Open terminal", action: openTerminal) }
            }
        }
    }
}

private struct PDFDocumentView: UIViewRepresentable {
    let url: URL

    func makeUIView(context _: Context) -> PDFView {
        let view = PDFView()
        view.autoScales = true
        view.displayMode = .singlePageContinuous
        view.backgroundColor = UIColor.clear
        return view
    }

    func updateUIView(_ view: PDFView, context _: Context) {
        if view.document?.documentURL != url {
            view.document = PDFDocument(url: url)
        }
    }
}
