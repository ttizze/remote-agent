import AgentCore
import Foundation

extension TimelineDisclosure {
    static let empty = TimelineDisclosure(
        expandedRuns: [], expandedAttempts: [], expandedWorkGroups: [], expandedEntries: [], changedFiles: []
    )
}

/// Builds the selected thread's view off the main thread whenever the
/// snapshot or what the screen has expanded changes.
extension BexAppViewModel {
    func schedulePresentation() {
        guard let owner = store else {
            presentation?.cancel()
            presentation = nil
            threadView = nil
            timelineRows = []
            return
        }
        guard presentation == nil else { return }
        let expectedHost = selectedProfileId
        presentation = Task { [weak self] in
            guard let self else { return }
            defer {
                if store === owner, selectedProfileId == expectedHost {
                    presentation = nil
                }
            }
            while !Task.isCancelled {
                do { try await Task.sleep(for: .milliseconds(16)) } catch { return }
                let latest = snapshot
                let options = threadOptions
                let now = Int64(Date().timeIntervalSince1970 * 1000)
                let previousRows = threadView.map { "\($0.threadId):\($0.rowsRevision)" }
                let (view, rows) = await Task.detached(priority: .userInitiated) {
                    let view = latest.selectedThread(nowMs: now, options: options)
                    let rows = view.flatMap { view in
                        previousRows != "\(view.threadId):\(view.rowsRevision)" ? view.rows
                            .values() : nil
                    }
                    return (view, rows)
                }.value
                guard !Task.isCancelled, store === owner, selectedProfileId == expectedHost else { return }
                if snapshot.selectedThreadId() == latest.selectedThreadId() {
                    timelineRows = rows ?? (view == nil ? [] : timelineRows)
                    threadView = view
                    scheduleTick(view)
                }
                if snapshot === latest, options == threadOptions {
                    return
                }
            }
        }
    }

    var threadOptions: ThreadViewOptions {
        ThreadViewOptions(
            layout: .mobile, disclosure: disclosure,
            panels: HeaderPanelState(threadPanelOpen: false, terminalOpen: false, rightPanelOpen: false,
                                     filesOpen: false),
            composer: ComposerOptions(compact: true, alternateModifier: false,
                                      shortcuts: ComposerShortcuts(alternateSend: nil, queueSteer: nil,
                                                                   queueEdit: nil)),
            showScrollToEnd: showScrollToEnd
        )
    }

    /// Elapsed labels the view holds as text refresh once a second while live.
    private func scheduleTick(_ view: ThreadView?) {
        presentationTick?.cancel()
        let live = (view?.agents?.liveCount ?? 0) > 0 || view?.setup.card?.phase == .running
        guard live else { return }
        presentationTick = Task { [weak self] in
            do { try await Task.sleep(for: .seconds(1)) } catch { return }
            self?.schedulePresentation()
        }
    }

    func toggle(_ keyPath: WritableKeyPath<TimelineDisclosure, [String]>, _ id: String) {
        if let index = disclosure[keyPath: keyPath].firstIndex(of: id) {
            disclosure[keyPath: keyPath].remove(at: index)
        } else {
            disclosure[keyPath: keyPath].append(id)
        }
    }
}
