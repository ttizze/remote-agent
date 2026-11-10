import Foundation

#if os(iOS)
    import SwiftUI
    import UIKit
#endif

enum DetailsContent: Decodable {
    case separator
    case error(message: String)
    case welcome(directory: String)
    case projectEditor(editor: ProjectEditorData?)
    case worktreeEditor(data: WorktreeEditorData?)
    case modelPicker(data: ModelPickerData?)
    case taskList(data: TaskListData)
}

#if os(iOS)
private struct DetailsView: View {
    let content: DetailsContent
    var send: ([String: String]) -> Void = { _ in }

    var body: some View {
        Group {
            switch content {
            case let .projectEditor(editor):
                ProjectEditorPresenter(editor: editor, send: send)
            case let .worktreeEditor(data):
                WorktreeEditorPresenter(data: data, send: send)
            case let .modelPicker(data):
                ModelPickerPresenter(data: data, send: send)
            case let .taskList(data):
                TaskListView(data: data, send: send)
            case .separator: Divider()
            case let .welcome(directory):
                VStack(spacing: 16) {
                    Text("何から始めましょうか？").font(.system(size: 24, weight: .semibold))
                    Text(directory.isEmpty ? "メッセージを入力して、作業を始めましょう。" : directory)
                        .font(.system(size: 14))
                }.frame(maxWidth: .infinity, maxHeight: .infinity)
            case let .error(message):
                ContentUnavailableView("表示できませんでした", systemImage: "exclamationmark.triangle",
                                       description: Text(verbatim: message))
            }
        }.frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

@MainActor private final class DetailsOwner {
    let context: UnsafeMutableRawPointer?
    var callback: (@convention(c) (UnsafeMutableRawPointer?, UnsafePointer<UInt8>?, Int) -> Void)?

    init(
        context: UnsafeMutableRawPointer?,
        callback: @escaping @convention(c) (UnsafeMutableRawPointer?, UnsafePointer<UInt8>?, Int) -> Void
    ) {
        self.context = context
        self.callback = callback
    }

    let view = NativeHostingView(rootView: DetailsView(content: .separator))

    func update(_ data: Data) {
        let content: DetailsContent
        do {
            content = try JSONDecoder().decode(DetailsContent.self, from: data)
        } catch {
            content = .error(message: error.localizedDescription)
        }
        let root = DetailsView(content: content) { [weak self] action in
            guard let self, let data = try? JSONSerialization.data(withJSONObject: action) else { return }
            data.withUnsafeBytes { self.callback?(self.context, $0.bindMemory(to: UInt8.self).baseAddress, data.count) }
        }
        view.controller.rootView = root
    }
}

@_cdecl("bex_nsh_details_create")
@MainActor public func detailsCreate(
    _ context: UnsafeMutableRawPointer?,
    _ callback: @escaping @convention(c) (UnsafeMutableRawPointer?, UnsafePointer<UInt8>?, Int) -> Void
) -> UnsafeMutableRawPointer {
    Unmanaged.passRetained(DetailsOwner(context: context, callback: callback)).toOpaque()
}

@_cdecl("bex_nsh_details_view")
@MainActor public func detailsView(_ handle: UnsafeMutableRawPointer) -> UnsafeMutableRawPointer {
    Unmanaged.passUnretained(Unmanaged<DetailsOwner>.fromOpaque(handle).takeUnretainedValue().view).toOpaque()
}

@_cdecl("bex_nsh_details_update")
@MainActor public func detailsUpdate(_ handle: UnsafeMutableRawPointer, _ bytes: UnsafePointer<UInt8>, _ count: Int) {
    Unmanaged<DetailsOwner>.fromOpaque(handle).takeUnretainedValue().update(Data(bytes: bytes, count: count))
}

@_cdecl("bex_nsh_details_release")
@MainActor public func detailsRelease(_ handle: UnsafeMutableRawPointer) {
    let owner = Unmanaged<DetailsOwner>.fromOpaque(handle).takeRetainedValue()
    owner.callback = nil
    owner.view.detach()
}

#endif
