import AgentCore
import SwiftUI

/// The shared scheduled-task editor. Core owns schedule validation and the
/// Host request; this screen only edits the draft and presents the pickers.
struct ScheduledTasksScreen: View {
    @ObservedObject var model: BexAppViewModel
    @Environment(.dismiss) private var dismiss
    @State private var draft: ScheduledTaskDraft?
    @State private var selectedId: String?

    var body: some View {
        List {
            Section("Automations") {
                ForEach(model.snapshot.scheduledTasks().tasks, id: \.id) { task in
                    Button {
                        selectedId = task.id
                        draft = model.snapshot.scheduledTaskDraft(id: task.id)
                    } label: {
                        HStack {
                            VStack(alignment: .leading, spacing: 4) {
                                Text(task.title).foregroundStyle(AppTheme.text)
                                Text("\(task.scheduleLabel) · \(task.lastRunStatus.capitalized)")
                                    .font(AppTheme.font(13)).foregroundStyle(AppTheme.muted)
                            }
                            Spacer()
                            Image(systemName: task.enabled ? "checkmark.circle.fill" : "pause.circle")
                                .foregroundStyle(task.enabled ? AppTheme.primary : AppTheme.muted)
                        }
                    }
                    .buttonStyle(.plain)
                    .contextMenu {
                        Button(task.enabled ? "Pause" : "Enable") {
                            model.perform(.setScheduledTaskEnabled(id: task.id, enabled: !task.enabled))
                        }
                        Button("Run now") {
                            model.perform(.runScheduledTaskNow(id: task.id))
                        }
                        Button("Delete", role: .destructive) {
                            model.perform(.deleteScheduledTask(id: task.id))
                            if selectedId == task.id {
                                selectedId = nil
                                draft = nil
                            }
                        }
                    }
                }
                Button {
                    selectedId = nil
                    draft = model.snapshot.scheduledTaskDraft(id: nil)
                } label: {
                    Label("New scheduled task", systemImage: "plus")
                }
            }
            if let draft {
                ScheduledTaskEditor(model: model, draft: Binding(
                    get: { self.draft ?? draft },
                    set: { self.draft = $0 }
                )) {
                    model.perform(.saveScheduledTask(draft: self.draft ?? draft)) { result in
                        guard case .success = result else { return }
                        self.draft = nil
                        self.selectedId = nil
                    }
                }
            }
        }
        .scrollContentBackground(.hidden)
        .background(AppTheme.sheet)
        .navigationTitle("Scheduled tasks")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .confirmationAction) {
                Button("Done") { dismiss() }
            }
        }
    }
}

private struct ScheduledTaskEditor: View {
    @ObservedObject var model: BexAppViewModel
    @Binding var draft: ScheduledTaskDraft
    let save: () -> Void

    private let weekdays: [(String, UInt8)] = [("Sun", 0), ("Mon", 1), ("Tue", 2), ("Wed", 3), ("Thu", 4), ("Fri", 5), ("Sat", 6)]

    var body: some View {
        Section("Task") {
            TextField("Title", text: $draft.title)
            TextField("Prompt", text: $draft.prompt, axis: .vertical)
                .lineLimit(3 ... 8)
            Toggle("Enabled", isOn: $draft.enabled)
            Picker("Project", selection: $draft.projectId) {
                ForEach(model.snapshot.projects(), id: \.id) { project in
                    Text(project.name).tag(project.id)
                }
            }
            Menu {
                ForEach(modelChoices, id: \.key) { choice in
                    Button {
                        let sameModel = choice.instanceId == draft.instanceId && choice.slug == draft.model
                        draft.instanceId = choice.instanceId
                        draft.driver = choice.driver
                        draft.model = choice.slug
                        if !sameModel {
                            draft.options = []
                        }
                    } label: {
                        HStack {
                            Text(choice.label)
                            Spacer()
                            if choice.instanceId == draft.instanceId && choice.slug == draft.model {
                                Image(systemName: "checkmark")
                            }
                        }
                    }
                }
            } label: {
                HStack {
                    Text("Model")
                    Spacer()
                    Text(modelLabel).foregroundStyle(AppTheme.muted)
                    Image(systemName: "chevron.up.chevron.down")
                }
            }
            Picker("Runtime", selection: $draft.runtimeMode) {
                Text("Ask before changes").tag(RuntimeMode.approvalRequired)
                Text("Auto accept edits").tag(RuntimeMode.autoAcceptEdits)
                Text("Automatic").tag(RuntimeMode.auto)
                Text("Full access").tag(RuntimeMode.fullAccess)
            }
        }
        Section("Schedule") {
            Picker("Type", selection: Binding(
                get: { scheduleIsInterval ? "interval" : "fixed" },
                set: { setSchedule($0 == "interval") }
            )) {
                Text("Fixed local time").tag("fixed")
                Text("Interval").tag("interval")
            }
            if scheduleIsInterval {
                Stepper(
                    "Every \(intervalMinutes) minute\(intervalMinutes == 1 ? "" : "s")",
                    value: Binding(
                        get: { intervalMinutes },
                        set: { setInterval($0) }
                    ),
                    in: 1 ... 31_536_000
                )
            } else {
                TextField("Local time (HH:MM)", text: Binding(
                    get: { fixedTime },
                    set: { setFixedTime($0) }
                ))
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack {
                        ForEach(Array(weekdays.enumerated()), id: \.offset) { _, weekday in
                            let (name, day) = weekday
                            let on = fixedDays.contains(day)
                            Button(name) { toggleDay(day) }
                                .buttonStyle(.borderedProminent)
                                .tint(on ? AppTheme.primary : AppTheme.groupedCard)
                                .foregroundStyle(on ? AppTheme.primaryForeground : AppTheme.text)
                        }
                    }
                }
            }
        }
        Section("Workspace") {
            Picker("Workspace", selection: Binding(
                get: { workspaceIdentifier },
                set: { setWorkspace($0) }
            )) {
                Text("Project checkout").tag("root")
                Text("New worktree").tag("worktree")
                Text("Existing worktree").tag("existing")
            }
            if workspaceIdentifier == "worktree" {
                NavigationLink {
                    ScheduledTaskBranchPicker(model: model, draft: $draft)
                } label: {
                    HStack {
                        Text("Base branch")
                        Spacer()
                        Text(baseBranch.isEmpty ? "Choose a branch" : baseBranch)
                            .foregroundStyle(AppTheme.muted)
                        Image(systemName: "chevron.right")
                            .foregroundStyle(AppTheme.muted)
                    }
                }
                Toggle("Start from origin", isOn: Binding(
                    get: { startFromOrigin },
                    set: { setStartFromOrigin($0) }
                ))
            } else if workspaceIdentifier == "existing" {
                TextField("Worktree path", text: Binding(
                    get: { worktreePath },
                    set: { setWorktreePath($0) }
                ))
            }
            Button("Save scheduled task", action: save)
                .frame(maxWidth: .infinity)
        }
    }

    private var scheduleIsInterval: Bool {
        if case .interval = draft.schedule { return true }
        return false
    }

    private var modelLabel: String {
        modelChoices.first(where: { $0.instanceId == draft.instanceId && $0.slug == draft.model })?.label
            ?? (draft.model.isEmpty ? "Choose a model" : "\(draft.instanceId) · \(draft.model)")
    }

    private var modelChoices: [ScheduledModelChoice] {
        let options = CatalogSheetOptions(
            filter: .all,
            showLegacy: false,
            query: "",
            expansionOverrides: [],
            stagedKey: nil
        )
        let catalog = model.snapshot.catalogSheet(options: options)
        return catalog.items.compactMap { item in
            guard case let .model(key, instanceId, driver, slug, label, _, _, _, _, _, _, _, _) = item else {
                return nil
            }
            return ScheduledModelChoice(key: key, instanceId: instanceId, driver: driver, slug: slug, label: label)
        }
    }

    private var intervalMinutes: UInt64 {
        if case let .interval(everyMs) = draft.schedule { return max(1, everyMs / 60_000) }
        return 15
    }

    private var fixedTime: String {
        if case let .fixedTime(time, _) = draft.schedule { return time }
        return "09:00"
    }

    private var fixedDays: [UInt8] {
        if case let .fixedTime(_, days) = draft.schedule { return days }
        return []
    }

    private var workspaceIdentifier: String {
        switch draft.workspace {
        case .root: "root"
        case .worktree: "worktree"
        case .existingWorktree: "existing"
        }
    }

    private var baseBranch: String {
        if case let .worktree(base, _, _) = draft.workspace { return base }
        return ""
    }

    private var worktreePath: String {
        if case let .existingWorktree(path, _) = draft.workspace { return path }
        return ""
    }

    private func setSchedule(_ interval: Bool) {
        draft.schedule = interval
            ? .interval(everyMs: max(60_000, intervalMinutes * 60_000))
            : .fixedTime(timeOfDay: fixedTime, weekdays: fixedDays.isEmpty ? [1, 2, 3, 4, 5] : fixedDays)
    }

    private func setInterval(_ minutes: UInt64) {
        draft.schedule = .interval(everyMs: max(1, minutes) * 60_000)
    }

    private func setFixedTime(_ value: String) {
        draft.schedule = .fixedTime(timeOfDay: value, weekdays: fixedDays)
    }

    private func toggleDay(_ day: UInt8) {
        var days = fixedDays
        if let index = days.firstIndex(of: day) { days.remove(at: index) } else { days.append(day) }
        draft.schedule = .fixedTime(timeOfDay: fixedTime, weekdays: days.sorted())
    }

    private func setWorkspace(_ value: String) {
        draft.workspace = switch value {
        case "worktree": .worktree(baseRef: baseBranch.isEmpty ? "main" : baseBranch, branch: nil, startFromOrigin: startFromOrigin)
        case "existing": .existingWorktree(worktreePath: worktreePath, branch: nil)
        default: .root(branch: nil)
        }
    }

    private var startFromOrigin: Bool {
        if case let .worktree(_, _, startFromOrigin) = draft.workspace { return startFromOrigin }
        return true
    }

    private func setStartFromOrigin(_ value: Bool) {
        if case let .worktree(baseRef, branch, _) = draft.workspace {
            draft.workspace = .worktree(baseRef: baseRef, branch: branch, startFromOrigin: value)
        }
    }

    private func setWorktreePath(_ value: String) {
        draft.workspace = .existingWorktree(worktreePath: value, branch: nil)
    }
}

private struct ScheduledTaskBranchPicker: View {
    @ObservedObject var model: BexAppViewModel
    @Binding var draft: ScheduledTaskDraft
    @Environment(.dismiss) private var dismiss
    @State private var query = ""

    private var baseBranch: String {
        if case let .worktree(base, _, _) = draft.workspace { return base }
        return ""
    }

    private var branchView: ScheduledTaskBranchView {
        model.snapshot.scheduledTaskBranches(
            projectId: draft.projectId,
            selectedBranch: baseBranch
        )
    }

    var body: some View {
        List {
            if branchView.loading {
                ProgressView("Loading branches…")
            }
            if let error = branchView.error {
                Text(error).foregroundStyle(AppTheme.muted)
            }
            ForEach(branchView.branches, id: \.name) { branch in
                Button {
                    select(branch.name)
                } label: {
                    HStack {
                        Image(systemName: "arrow.triangle.branch")
                            .foregroundStyle(AppTheme.muted)
                        VStack(alignment: .leading, spacing: 2) {
                            Text(branch.name).foregroundStyle(AppTheme.text)
                            if let badge = branch.badge {
                                Text(badge.uppercased())
                                    .font(AppTheme.font(12))
                                    .foregroundStyle(AppTheme.muted)
                            }
                        }
                        Spacer()
                        if branch.selected {
                            Image(systemName: "checkmark")
                                .foregroundStyle(AppTheme.primary)
                        }
                    }
                }
            }
            if branchView.hasMore {
                Button("Load more branches") {
                    model.perform(.loadMoreScheduledTaskBranches(projectId: draft.projectId))
                }
            }
            if branchView.branches.isEmpty && !branchView.loading && branchView.error == nil {
                Text("No local branches available").foregroundStyle(AppTheme.muted)
            }
        }
        .navigationTitle("Base branch")
        .searchable(text: $query, prompt: "Find a branch")
        .onAppear {
            model.perform(.searchScheduledTaskBranches(projectId: draft.projectId, query: query))
        }
        .onChange(of: query) { _, value in
            model.perform(.searchScheduledTaskBranches(projectId: draft.projectId, query: value))
        }
    }

    private func select(_ branch: String) {
        guard case let .worktree(_, currentBranch, startFromOrigin) = draft.workspace else {
            return
        }
        draft.workspace = .worktree(
            baseRef: branch,
            branch: currentBranch,
            startFromOrigin: startFromOrigin
        )
        dismiss()
    }
}

private struct ScheduledModelChoice {
    let key: String
    let instanceId: String
    let driver: Driver
    let slug: String
    let label: String
}
