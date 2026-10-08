import AgentCore
import SwiftUI

struct GitPullRequestSection: View {
    @Binding var pullRequestReference: String
    @Binding var checkoutMode: String
    let onCheckout: () -> Void

    var body: some View {
        Section("Pull request thread") {
            TextField("PR number or URL", text: $pullRequestReference)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
            Picker("Checkout", selection: $checkoutMode) {
                Text("Worktree").tag("worktree")
                Text("Current checkout").tag("local")
            }
            Button(action: onCheckout) {
                Label("Open pull request as thread", systemImage: "arrow.branch")
            }
            .disabled(pullRequestReference.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
        }
    }
}

extension GitAction {
    var name: String {
        switch self {
        case .commit: "commit"
        case .push: "push"
        case .createPr: "create_pr"
        case .commitPush: "commit_push"
        case .commitPushPr: "commit_push_pr"
        case .openPr: "open_pr"
        }
    }

    var includesCommit: Bool {
        self == .commit || self == .commitPush || self == .commitPushPr
    }

    var symbol: String {
        switch self {
        case .commit: "checkmark.circle"
        case .push: "arrow.up"
        case .createPr, .commitPushPr, .openPr: "arrow.triangle.pull"
        case .commitPush: "arrow.up.circle"
        }
    }
}
