import AgentCore
import SwiftUI

/// The shared scheduled-task editor. Core owns schedule validation and the
/// Host request; this screen only edits the draft and presents the pickers.
struct ScheduledTasksScreen: View {
    @ObservedObject var model: BexAppViewModel
    @Environment(.dismiss) private var dismiss
    @State private var draft: ScheduledTaskDraft?
    @State private var selectedId: String?
    @State private var saveError: String?

    var body: some View {
        List {
            Section("Automations") {
                ForEach(model.snapshot.scheduledTasks().tasks, id: \.id) { task in
                    Button {
                        selectedId = task.id
                        draft = model.snapshot.scheduledTaskDraft(id: task.id)
                        saveError = nil
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
                                saveError = nil
                            }
                        }
                    }
                }
                Button {
                    selectedId = nil
                    draft = model.snapshot.scheduledTaskDraft(id: nil)
                    saveError = nil
                } label: {
                    Label("New scheduled task", systemImage: "plus")
                }
            }
            if let draft {
                ScheduledTaskEditor(model: model, draft: Binding(
                    get: { self.draft ?? draft },
                    set: { self.draft = $0 }
                ), taskMissing: selectedId.map { taskID in
                    draft.id == taskID && !model.snapshot.scheduledTasks().tasks.contains { $0.id == taskID }
                } == true,
                saveError: saveError) {
                    model.perform(.saveScheduledTask(draft: self.draft ?? draft)) { result in
                        switch result {
                        case .success:
                            self.draft = nil
                            selectedId = nil
                            saveError = nil
                        case let .failure(error):
                            saveError = error.localizedDescription
                        }
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
