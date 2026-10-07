import AgentCore
import SwiftUI

/// The brand, or the connection's state once it has been away a moment.
struct ConnectionTitle: View {
    @ObservedObject var model: BexAppViewModel
    let open: () -> Void
    @State private var showsStatus = false

    var body: some View {
        Group {
            if showsStatus, !model.isConnected {
                Button(action: open) {
                    HStack(spacing: 6) {
                        if model.isConnecting {
                            ProgressView().controlSize(.small)
                        } else {
                            Image(systemName: "wifi.slash").font(.system(size: 15))
                        }
                        Text(statusLabel).font(AppTheme.font(16, weight: .bold)).lineLimit(1)
                    }
                    .foregroundStyle(AppTheme.muted)
                }
                .buttonStyle(.plain)
                .transition(.opacity)
            } else {
                Text("Bex").font(AppTheme.font(21, weight: .medium)).foregroundStyle(AppTheme.muted)
                    .accessibilityLabel("Bex, Threads")
            }
        }
        .animation(.easeOut(duration: 0.25), value: showsStatus)
        .task(id: model.isConnected) {
            showsStatus = false
            guard !model.isConnected else { return }
            try? await Task.sleep(for: .milliseconds(800))
            showsStatus = true
        }
    }

    private var statusLabel: String {
        let host = model.selectedProfileName ?? "environment"
        if model.isConnecting {
            return "Reconnecting to \(host)"
        }
        return model.notice ?? "Not connected"
    }
}

struct ThreadFilterMenu: View {
    @ObservedObject var model: BexAppViewModel

    var body: some View {
        let selectedProject = model.snapshot.selectedProjectId()
        Menu {
            Menu("Environment") {
                ForEach(model.profiles.sorted { $0.name < $1.name }) { profile in
                    Button { model.selectProfile(profile.id) } label: {
                        if profile.id == model.selectedProfileId {
                            Label(profile.name, systemImage: "checkmark")
                        } else {
                            Text(profile.name)
                        }
                    }
                }
            }
            let projects = model.snapshot.projects()
            if !projects.isEmpty {
                Menu("Project") {
                    Button { model.perform(.filterProject(projectId: nil)) } label: {
                        Label("All projects", systemImage: selectedProject == nil ? "checkmark" : "")
                        Text("Show threads from every project")
                    }
                    ForEach(projects, id: \.id) { project in
                        Button { model.perform(.filterProject(projectId: project.id)) } label: {
                            if project.id == selectedProject {
                                Label(project.name, systemImage: "checkmark")
                            } else {
                                Text(project.name)
                            }
                        }
                    }
                }
            }
        } label: {
            Image(systemName: selectedProject == nil
                ? "line.3.horizontal.decrease" : "line.3.horizontal.decrease.circle.fill")
        }
        .accessibilityLabel("Filter threads")
    }
}

struct ThreadListEmptyState: View {
    @ObservedObject var model: BexAppViewModel
    let newTask: () -> Void

    var body: some View {
        VStack(spacing: 12) {
            if model.isConnecting || !model.isConnected {
                if model.isConnecting {
                    ProgressView()
                }
                EmptyStateText(
                    title: model.isConnecting ? "Connecting to environment" : "Environment unavailable",
                    detail: model.isConnecting ? "Loading projects and threads from the saved environment."
                        : model.notice ?? "The saved environment is offline. Check the URL or start the environment, "
                        + "then retry."
                )
                if !model.isConnecting {
                    Button("Retry") { model.connect(afterForeground: true) }
                        .font(AppTheme.font(14, weight: .bold))
                }
            } else {
                EmptyStateText(
                    title: "No threads yet",
                    detail: "Create a task to start a new coding runtime in one of your connected projects."
                )
                Button("New task", action: newTask).font(AppTheme.font(14, weight: .bold))
            }
        }
        .frame(maxWidth: 430)
        .padding(24)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

struct EmptyStateText: View {
    let title: String
    let detail: String?

    var body: some View {
        VStack(spacing: 6) {
            Text(title).font(AppTheme.font(18, weight: .bold)).foregroundStyle(AppTheme.text)
            if let detail {
                Text(detail).font(AppTheme.font(14)).foregroundStyle(AppTheme.muted)
            }
        }
        .multilineTextAlignment(.center)
        .frame(maxWidth: .infinity)
        .padding(.vertical, 24)
    }
}
