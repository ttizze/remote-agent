import AgentCore
import SwiftUI

/// "New task": choose a project, then write the first message.
struct NewTaskFlow: View {
    @ObservedObject var model: BexAppViewModel
    /// Core already opened the draft (a new thread on a branch): show it directly.
    var draftOpen = false
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
            if draftOpen {
                path = [model.snapshot.selectedProjectId() ?? "chats"]
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
        let projects = model.environmentProjects(query)
        return ScrollView {
            VStack(spacing: 12) {
                Button { choose(nil) } label: {
                    ProjectChoiceRow(symbol: "text.bubble", title: "No project",
                                     subtitle: "Start a task without a project", glyph: nil, icon: nil)
                        .background(AppTheme.groupedCard, in: RoundedRectangle(cornerRadius: 24))
                }
                .buttonStyle(.plain)
                if projects.isEmpty, !query.isEmpty {
                    EmptyStateText(title: "No matching projects",
                                   detail: "Try a different project name or workspace path.")
                } else if !projects.isEmpty {
                    VStack(spacing: 0) {
                        ForEach(Array(projects.enumerated()), id: \.element.projectId) { index, project in
                            Button { choose(project.projectId) } label: {
                                ProjectChoiceRow(symbol: nil, title: "\(project.environmentLabel) · \(project.title)",
                                                 subtitle: project.subtitle, glyph: project.title,
                                                 icon: nil)
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
        .background(AppTheme.sheet.ignoresSafeArea())
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
        model.perform(draftOpen
            ? .setNewThreadProject(projectId: project)
            : .newThread(projectId: project))
        path = [project ?? "chats"]
    }
}

private struct ProjectChoiceRow: View {
    let symbol: String?
    let title: String
    let subtitle: String
    let glyph: String?
    let icon: UIImage?

    var body: some View {
        HStack(spacing: 12) {
            Group {
                if let symbol {
                    Image(systemName: symbol).font(.system(size: 18))
                } else if let glyph {
                    ProjectGlyph(name: glyph, icon: icon, size: 20)
                }
            }
            .frame(width: 28, height: 28)
            VStack(alignment: .leading, spacing: 2) {
                Text(title).font(AppTheme.font(16, weight: .bold)).foregroundStyle(AppTheme.text)
                if !subtitle.isEmpty {
                    Text(subtitle).font(AppTheme.font(12)).foregroundStyle(AppTheme.muted)
                        .lineLimit(1).truncationMode(.middle)
                }
            }
            Spacer()
            Image(systemName: "chevron.right").font(.system(size: 14))
                .foregroundStyle(AppTheme.color("mobileChevron"))
        }
        .padding(.horizontal, 16).padding(.vertical, 14)
        .contentShape(Rectangle())
    }
}

/// The new task's hero headline and composer.
private struct NewTaskDraft: View {
    @ObservedObject var model: BexAppViewModel
    let cancel: () -> Void
    @State private var showingSettings = false
    @State private var showingBranches = false

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
        .background(AppTheme.sheet.ignoresSafeArea())
        .safeAreaInset(edge: .bottom, spacing: 0) {
            Composer(
                model: model, composer: view.composer, alwaysExpanded: true,
                accessory: view.workspace.map { AnyView(workspaceControls($0)) },
                sendBlockedReason: view.workspace?.blockedReason
            ) { showingSettings = true }
        }
        .navigationBarBackButtonHidden(true)
        .toolbar {
            ToolbarItem(placement: .cancellationAction) { Button("Cancel", action: cancel) }
        }
        .sheet(isPresented: $showingSettings) {
            ThreadSettingsSheet(model: model, controls: view.composer.controls)
        }
        .navigationDestination(isPresented: $showingBranches) {
            BranchPicker(model: model)
        }
        .task(id: view.projectId) {
            if view.workspace != nil {
                model.perform(.searchNewThreadBranches(query: ""))
            }
        }
    }

    private func workspaceControls(_ workspace: NewThreadWorkspaceView) -> some View {
        HStack(spacing: 4) {
            Button {
                model.perform(.setNewThreadWorkspace(mode: workspace.mode == .local ? .worktree : .local))
            } label: {
                InlineControl(label: workspace.workspaceLabel, maxWidth: workspace.mode == .local ? 220 : 148,
                              chevron: nil) {
                    WorkspaceIcon(branched: workspace.mode == .worktree || workspace.inWorktree)
                }
            }
            .buttonStyle(.plain)
            .accessibilityLabel(workspace.workspaceLabel)
            .accessibilityHint(workspace
                .mode == .local ? "Switches to a new worktree" : "Switches to the current checkout")
            Button { showingBranches = true } label: {
                InlineControl(label: workspace.branchLabel, maxWidth: 190, chevron: "chevron.right") {
                    Image(systemName: "arrow.triangle.branch").font(.system(size: 14))
                }
            }
            .buttonStyle(.plain)
            .accessibilityLabel("\(workspace.branchRole): \(workspace.branchLabel)")
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 8)
        .padding(.bottom, 4)
    }
}

/// A quiet control in the composer's context row: icon, label, optional chevron.
private struct InlineControl<Icon: View>: View {
    let label: String
    let maxWidth: CGFloat
    let chevron: String?
    @ViewBuilder let icon: () -> Icon

    var body: some View {
        CappedWidth(maxWidth: maxWidth) {
            HStack(spacing: 8) {
                icon().frame(width: 16, height: 16).foregroundStyle(AppTheme.color("mobileIconMuted"))
                Text(label).font(AppTheme.font(14, weight: .medium)).foregroundStyle(AppTheme.muted).lineLimit(1)
                if let chevron {
                    Image(systemName: chevron).font(.system(size: 10))
                        .foregroundStyle(AppTheme.color("mobileIconMuted"))
                }
            }
            .padding(.horizontal, 8)
            .frame(height: 44)
        }
        .contentShape(RoundedRectangle(cornerRadius: 12))
    }
}

/// Its content at its natural width, truncated past `maxWidth`.
private struct CappedWidth: Layout {
    let maxWidth: CGFloat

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache _: inout ()) -> CGSize {
        subviews.first?.sizeThatFits(capped(proposal)) ?? .zero
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache _: inout ()) {
        subviews.first?.place(at: bounds.origin, proposal: capped(proposal))
    }

    private func capped(_ proposal: ProposedViewSize) -> ProposedViewSize {
        ProposedViewSize(width: min(proposal.width ?? maxWidth, maxWidth), height: proposal.height)
    }
}

/// A folder, with a branch badge for a worktree.
private struct WorkspaceIcon: View {
    let branched: Bool

    var body: some View {
        Image(systemName: "folder").font(.system(size: 14))
            .overlay(alignment: .bottomTrailing) {
                if branched {
                    Image(systemName: "arrow.triangle.branch").font(.system(size: 8))
                        .offset(x: 4, y: 4)
                }
            }
    }
}
