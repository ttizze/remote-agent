import AgentCore
import SwiftUI

/// "Agents": the subagents of the current turn.
struct AgentsSheet: View {
    @ObservedObject var model: BexAppViewModel
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        let rows = model.threadView?.agents?.rows ?? []
        NavigationStack {
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0) {
                    if rows.isEmpty {
                        Text("No agents in this turn.").font(AppTheme.font(14)).foregroundStyle(AppTheme.muted)
                            .frame(maxWidth: .infinity).padding(.top, 21)
                    }
                    ForEach(rows, id: \.id) { agent in
                        Button {
                            dismiss()
                            model.openThread(agent.childThreadId)
                        } label: {
                            AgentRowView(agent: agent)
                        }
                        .buttonStyle(.plain)
                        .disabled(agent.childThreadId.isEmpty)
                    }
                }
                .padding(.horizontal, 17.5)
            }
            .background(AppTheme.color("surfaceOverlay"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .principal) {
                    Text("Agents").font(AppTheme.font(18, weight: .heavy))
                }
            }
        }
        .presentationDetents([.fraction(0.5), .fraction(0.9)])
        .presentationDragIndicator(.visible)
    }
}

private struct AgentRowView: View {
    let agent: AgentRow

    var body: some View {
        HStack(alignment: .top, spacing: 10.5) {
            Circle().fill(agent.tone.color).frame(width: 7, height: 7).frame(height: 17.5)
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 4) {
                    Text(agent.title).font(AppTheme.font(14, weight: .medium)).foregroundStyle(AppTheme.text)
                        .lineLimit(1)
                    Text("·").foregroundStyle(AppTheme.muted)
                    Text(agent.statusLabel).font(AppTheme.font(13, weight: .medium)).foregroundStyle(agent.tone.color)
                    if let elapsed = agent.elapsed {
                        Text(elapsed).font(AppTheme.font(13)).monospacedDigit().foregroundStyle(AppTheme.muted)
                    }
                    Spacer(minLength: 0)
                    if !agent.childThreadId.isEmpty {
                        Image(systemName: "chevron.right").font(.system(size: 12)).foregroundStyle(AppTheme.muted)
                    }
                }
                HStack(spacing: 4) {
                    if let driver = agent.driver {
                        Image(driver.iconName).resizable().scaledToFit().frame(width: 12, height: 12)
                    }
                    Text(agent.modelLabel).lineLimit(1)
                    ForEach(Array(agent.workspace.enumerated()), id: \.offset) { _, entry in
                        Text("·")
                        Image(systemName: entry.label == "Branch" ? "arrow.triangle.branch" : "folder")
                            .font(.system(size: 11))
                        Text(entry.value).lineLimit(1).truncationMode(.middle)
                    }
                }
                .font(AppTheme.font(13)).foregroundStyle(AppTheme.muted)
                if let detail = agent.detail {
                    Text(detail).font(AppTheme.font(13)).lineLimit(3)
                        .foregroundStyle(agent.tone == .failed ? AppTheme.rose : AppTheme.muted)
                }
            }
        }
        .padding(.vertical, 12.25)
        .overlay(alignment: .bottom) { Rectangle().fill(AppTheme.border).frame(height: 1) }
        .contentShape(Rectangle())
    }
}
