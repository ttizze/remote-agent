import AgentCore
import SwiftUI

/// "New task": choose a project, then write the first message.
struct NewTaskFlow: View {
    @ObservedObject var model: BexAppViewModel
    /// Opens the draft for this project directly.
    var projectId: String?
    let started: (String) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var path: [String] = []
    @State private var query = ""
    @State private var adding = false
    @State private var projectPath = ""

    var body: some View {
        NavigationStack(path: $path) {
            chooser
                .navigationDestination(for: String.self) { _ in
                    NewTaskDraft(model: model, cancel: { dismiss() })
                }
        }
        .onAppear {
            if let projectId {
                choose(projectId)
            }
        }
        .onChange(of: model.selectedThreadId) { _, thread in
            if let thread {
                started(thread)
                dismiss()
            }
        }
    }

    private var chooser: some View {
        let projects = model.snapshot.projects().filter {
            $0.id != "chats" && (query.isEmpty || $0.name.localizedCaseInsensitiveContains(query)
                || ($0.roots.first?.path.localizedCaseInsensitiveContains(query) ?? false))
        }
        return ScrollView {
            VStack(spacing: 12) {
                Button { choose(nil) } label: {
                    ProjectChoiceRow(symbol: "text.bubble", title: "No project",
                                     subtitle: "Start a task without a project", glyph: nil)
                        .background(AppTheme.groupedCard, in: RoundedRectangle(cornerRadius: 24))
                }
                .buttonStyle(.plain)
                if projects.isEmpty, !query.isEmpty {
                    EmptyStateText(title: "No matching projects",
                                   detail: "Try a different project name or workspace path.")
                } else if !projects.isEmpty {
                    VStack(spacing: 0) {
                        ForEach(Array(projects.enumerated()), id: \.element.id) { index, project in
                            Button { choose(project.id) } label: {
                                ProjectChoiceRow(symbol: nil, title: project.name,
                                                 subtitle: project.roots.first?.path ?? "", glyph: project.name)
                            }
                            .buttonStyle(.plain)
                            .overlay(alignment: .top) {
                                if index > 0 {
                                    Rectangle().fill(AppTheme.borderSubtle).frame(height: 1)
                                }
                            }
                        }
                    }
                    .background(AppTheme.groupedCard, in: RoundedRectangle(cornerRadius: 24))
                }
            }
            .padding(.horizontal, 20).padding(.top, 8)
        }
        .background(AppTheme.color("surfaceOverlay").ignoresSafeArea())
        .searchable(text: $query, prompt: "Search projects")
        .navigationTitle("Choose project")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
            ToolbarItem(placement: .primaryAction) {
                Button { adding = true } label: { Image(systemName: "plus") }
                    .accessibilityLabel("Add project")
            }
        }
        .alert("Add project", isPresented: $adding) {
            TextField("Absolute path on the environment", text: $projectPath)
                .textInputAutocapitalization(.never).autocorrectionDisabled()
            Button("Add") {
                model.perform(.addProject(path: projectPath))
                projectPath = ""
            }
            Button("Cancel", role: .cancel) {}
        }
    }

    private func choose(_ project: String?) {
        model.perform(.newThread(projectId: project))
        path = [project ?? "chats"]
    }
}

private struct ProjectChoiceRow: View {
    let symbol: String?
    let title: String
    let subtitle: String
    let glyph: String?

    var body: some View {
        HStack(spacing: 10.5) {
            if let symbol {
                Image(systemName: symbol).font(.system(size: 18)).frame(width: 24.5, height: 24.5)
            } else if let glyph {
                ProjectGlyph(name: glyph).scaleEffect(20 / 15)
            }
            VStack(alignment: .leading, spacing: 2) {
                Text(title).font(AppTheme.font(16, weight: .bold)).foregroundStyle(AppTheme.text)
                if !subtitle.isEmpty {
                    Text(subtitle).font(AppTheme.font(13)).foregroundStyle(AppTheme.muted)
                        .lineLimit(1).truncationMode(.middle)
                }
            }
            Spacer()
            Image(systemName: "chevron.right").font(.system(size: 14)).foregroundStyle(AppTheme.muted)
        }
        .padding(.horizontal, 14).padding(.vertical, 12.25)
        .contentShape(Rectangle())
    }
}

/// The new task's hero headline and composer.
private struct NewTaskDraft: View {
    @ObservedObject var model: BexAppViewModel
    let cancel: () -> Void
    @State private var showingSettings = false

    var body: some View {
        let view = model.snapshot.newThread(options: ComposerOptions(
            compact: true, alternateModifier: false,
            shortcuts: ComposerShortcuts(alternateSend: nil, queueSteer: nil, queueEdit: nil)
        ))
        ScrollView {
            VStack(spacing: 6) {
                Text(view.hero.headingLabel)
                if !view.hero.projectLabel.isEmpty {
                    Text(view.hero.projectLabel)
                        .overlay(alignment: .bottom) {
                            Rectangle().fill(AppTheme.muted).frame(height: 1)
                        }
                }
                if let host = model.selectedProfileName {
                    HStack(spacing: 4) {
                        Image(systemName: "laptopcomputer").font(.system(size: 14))
                        Text("on \(host)")
                    }
                    .font(AppTheme.font(16)).foregroundStyle(AppTheme.muted).padding(.top, 6)
                }
            }
            .font(AppTheme.font(26, weight: .medium))
            .multilineTextAlignment(.center)
            .frame(maxWidth: .infinity)
            .padding(.top, 72).padding(.bottom, 236)
        }
        .background(AppTheme.color("surfaceOverlay").ignoresSafeArea())
        .safeAreaInset(edge: .bottom, spacing: 0) {
            VStack(spacing: 6) {
                HStack(spacing: 6) {
                    let worktree = model.snapshot.worktreeSettings()?.createOnNewSession == true
                    Image(systemName: worktree ? "arrow.triangle.branch" : "folder").font(.system(size: 13))
                    Text(worktree ? "New worktree" : "Current checkout").font(AppTheme.font(14, weight: .medium))
                    Spacer()
                }
                .foregroundStyle(AppTheme.muted)
                .padding(.horizontal, 19)
                Composer(model: model, composer: view.composer, alwaysExpanded: true) { showingSettings = true }
            }
        }
        .navigationBarBackButtonHidden(true)
        .toolbar {
            ToolbarItem(placement: .cancellationAction) { Button("Cancel", action: cancel) }
        }
        .sheet(isPresented: $showingSettings) {
            ThreadSettingsSheet(model: model, controls: view.composer.controls)
        }
    }
}
