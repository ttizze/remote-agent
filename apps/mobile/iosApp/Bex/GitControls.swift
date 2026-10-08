import AgentCore
import SwiftUI

/// The Git affordance in the thread header. The sheet owns the action
/// decisions and sends only the core-produced intents to the Host.
struct GitToolbarButton: View {
    @ObservedObject var model: BexAppViewModel
    let cwd: String
    let review: () -> Void
    let mergeBack: () -> Void
    let mergeBackAvailable: Bool
    @State private var showingOverview = false

    var body: some View {
        Button { showingOverview = true } label: {
            Image(systemName: "point.topleft.down.curvedto.point.bottomright.up")
        }
        .accessibilityLabel("Git")
        .sheet(isPresented: $showingOverview) {
            GitOverviewSheet(
                model: model,
                cwd: cwd,
                review: review,
                mergeBack: mergeBack,
                mergeBackAvailable: mergeBackAvailable
            )
        }
        .onAppear { subscribe() }
        .onChange(of: cwd) { _, _ in subscribe() }
    }

    private func subscribe() {
        guard !cwd.isEmpty else { return }
        model.perform(.subscribeVcsStatus(cwd: cwd))
    }
}
