import AgentCore
import Foundation
import SwiftUI

struct ScheduledTaskEditor: View {
    @ObservedObject var model: BexAppViewModel
    @Binding var draft: ScheduledTaskDraft
    let taskMissing: Bool
    let saveError: String?
    let save: () -> Void

    var body: some View {
        ScheduledTaskDetailsSection(
            model: model,
            draft: $draft,
            taskMissing: taskMissing,
            saveError: saveError
        )
        ScheduledTaskScheduleSection(draft: $draft)
        ScheduledTaskWorkspaceSection(
            model: model,
            draft: $draft,
            taskMissing: taskMissing,
            save: save
        )
    }
}

private struct ScheduledTaskDetailsSection: View {
    @ObservedObject var model: BexAppViewModel
    @Binding var draft: ScheduledTaskDraft
    let taskMissing: Bool
    let saveError: String?

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
            ScheduledTaskModelMenu(model: model, draft: $draft)
            let traits = model.snapshot.scheduledTaskTraits(draft: draft)
            ForEach(Array(traits.controls.enumerated()), id: \.offset) { _, control in
                ScheduledTaskTraitControl(model: model, draft: $draft, control: control)
            }
            let runtimeChoices = model.snapshot.scheduledTaskRuntimeModes(draft: draft)
            Picker("Runtime", selection: $draft.runtimeMode) {
                ForEach(Array(runtimeChoices.enumerated()), id: \.offset) { _, choice in
                    Text(choice.label).tag(choice.mode)
                }
            }
            if taskMissing {
                Text("This task was deleted elsewhere. Close this editor and start again.")
                    .foregroundStyle(AppTheme.dangerForeground)
            }
            if let saveError {
                Text(saveError).foregroundStyle(AppTheme.dangerForeground)
            }
        }
    }
}

private struct ScheduledTaskModelMenu: View {
    @ObservedObject var model: BexAppViewModel
    @Binding var draft: ScheduledTaskDraft

    var body: some View {
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
                        if choice.instanceId == draft.instanceId, choice.slug == draft.model {
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
    }

    private var modelLabel: String {
        modelChoices.first { choice in
            choice.instanceId == draft.instanceId && choice.slug == draft.model
        }?.label ?? (draft.model.isEmpty ? "Choose a model" : "\(draft.instanceId) · \(draft.model)")
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
            return ScheduledModelChoice(
                key: key,
                instanceId: instanceId,
                driver: driver,
                slug: slug,
                label: label
            )
        }
    }
}

private struct ScheduledTaskTraitControl: View {
    @ObservedObject var model: BexAppViewModel
    @Binding var draft: ScheduledTaskDraft
    let control: TraitControl

    var body: some View {
        switch control {
        case let .select(descriptorID, label, choices, selectedValue, note, disabled):
            NavigationLink {
                ChoicePage(
                    title: label,
                    choices: choices.map {
                        Choice(id: $0.id, label: $0.label, description: $0.description)
                    },
                    selected: selectedValue
                ) { choice in
                    draft = model.snapshot.selectScheduledTaskTrait(
                        draft: draft,
                        descriptorId: descriptorID,
                        choice: choice
                    )
                }
            } label: {
                HStack {
                    Text(label)
                    Spacer()
                    Text(choices.first { choice in choice.id == selectedValue }?.label ?? "")
                        .foregroundStyle(AppTheme.muted)
                }
            }
            .disabled(disabled)
            .accessibilityHint(note ?? "")
        case let .toggle(descriptorID, label, isOn):
            Toggle(label, isOn: Binding(get: { isOn }, set: { value in
                draft = model.snapshot.toggleScheduledTaskTrait(
                    draft: draft,
                    descriptorId: descriptorID,
                    on: value
                )
            }))
        }
    }
}

private struct ScheduledTaskScheduleSection: View {
    @Binding var draft: ScheduledTaskDraft

    private let weekdays: [(String, UInt8)] = [
        ("Sun", 0),
        ("Mon", 1),
        ("Tue", 2),
        ("Wed", 3),
        ("Thu", 4),
        ("Fri", 5),
        ("Sat", 6)
    ]

    var body: some View {
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
                            let isSelected = fixedDays.contains(day)
                            Button(name) { toggleDay(day) }
                                .buttonStyle(.borderedProminent)
                                .tint(isSelected ? AppTheme.primary : AppTheme.groupedCard)
                                .foregroundStyle(
                                    isSelected ? AppTheme.color("mobilePrimaryForeground") : AppTheme.text
                                )
                        }
                    }
                }
            }
        }
    }

    private var scheduleIsInterval: Bool {
        if case .interval = draft.schedule {
            return true
        }
        return false
    }

    private var intervalMinutes: UInt64 {
        if case let .interval(everyMs) = draft.schedule {
            return max(1, everyMs / 60000)
        }
        return 15
    }

    private var fixedTime: String {
        if case let .fixedTime(time, _) = draft.schedule {
            return time
        }
        return "09:00"
    }

    private var fixedDays: Data {
        if case let .fixedTime(_, days) = draft.schedule {
            return days
        }
        return []
    }

    private func setSchedule(_ interval: Bool) {
        draft.schedule = interval
            ? .interval(everyMs: max(60000, intervalMinutes * 60000))
            : .fixedTime(
                timeOfDay: fixedTime,
                weekdays: fixedDays.isEmpty ? Data([1, 2, 3, 4, 5]) : fixedDays
            )
    }

    private func setInterval(_ minutes: UInt64) {
        draft.schedule = .interval(everyMs: max(1, minutes) * 60000)
    }

    private func setFixedTime(_ value: String) {
        draft.schedule = .fixedTime(timeOfDay: value, weekdays: fixedDays)
    }

    private func toggleDay(_ day: UInt8) {
        var days = fixedDays
        if let index = days.firstIndex(of: day) {
            days.remove(at: index)
        } else {
            days.append(day)
        }
        draft.schedule = .fixedTime(timeOfDay: fixedTime, weekdays: Data(days.sorted()))
    }
}

private struct ScheduledTaskWorkspaceSection: View {
    @ObservedObject var model: BexAppViewModel
    @Binding var draft: ScheduledTaskDraft
    let taskMissing: Bool
    let save: () -> Void

    var body: some View {
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
                .disabled(taskMissing)
                .frame(maxWidth: .infinity)
        }
    }

    private var workspaceIdentifier: String {
        switch draft.workspace {
        case .root: "root"
        case .worktree: "worktree"
        case .existingWorktree: "existing"
        }
    }

    private var baseBranch: String {
        if case let .worktree(base, _, _) = draft.workspace {
            return base
        }
        return ""
    }

    private var worktreePath: String {
        if case let .existingWorktree(path, _) = draft.workspace {
            return path
        }
        return ""
    }

    private var startFromOrigin: Bool {
        if case let .worktree(_, _, startFromOrigin) = draft.workspace {
            return startFromOrigin
        }
        return true
    }

    private func setWorkspace(_ value: String) {
        draft.workspace = switch value {
        case "worktree": .worktree(
                baseRef: baseBranch.isEmpty ? "main" : baseBranch,
                branch: nil,
                startFromOrigin: startFromOrigin
            )
        case "existing": .existingWorktree(worktreePath: worktreePath, branch: nil)
        default: .root(branch: nil)
        }
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

private struct ScheduledModelChoice {
    let key: String
    let instanceId: String
    let driver: Driver
    let slug: String
    let label: String
}
