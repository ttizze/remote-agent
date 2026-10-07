import AgentCore
import SwiftUI

/// The worktree setup card under the first message, with its details sheet.
struct SetupCard: View {
    let card: SetupCardView
    let cancel: () -> Void
    let workLocally: () -> Void
    @State private var showingDetails = false

    var body: some View {
        VStack(alignment: .leading, spacing: 7) {
            if card.showHeader {
                header
            }
            if card.showStages {
                ForEach(Array(card.stages.enumerated()), id: \.offset) { _, stage in
                    StageRow(status: stage.status, label: stage.label, trailing: stage.trailing,
                             elapsed: stage.elapsed, height: 28)
                }
            } else if let summary = card.summary {
                StageRow(status: summary.status, label: summary.label, trailing: nil, elapsed: summary.elapsed,
                         height: 28)
            }
        }
        .padding(.vertical, 3.5)
        .sheet(isPresented: $showingDetails) {
            SetupDetailsSheet(card: card, cancel: cancel, workLocally: workLocally)
        }
    }

    private var header: some View {
        HStack(spacing: 8) {
            Text(card.title).font(AppTheme.font(14)).foregroundStyle(card.tone.color)
                .shimmering(card.phase == .running)
            if let elapsed = card.elapsed {
                Text(elapsed).font(AppTheme.font(12)).monospacedDigit().foregroundStyle(AppTheme.muted)
            }
            Spacer()
            Button { showingDetails = true } label: {
                HStack(spacing: 3) {
                    if card.phase == .failed {
                        Image(systemName: "exclamationmark.circle").font(.system(size: 12))
                    }
                    if let script = card.backgroundScript {
                        ProgressView().controlSize(.mini)
                        Text(script)
                    } else {
                        Text("Details")
                    }
                    Image(systemName: "chevron.right").font(.system(size: 10))
                }
                .font(AppTheme.font(12))
                .padding(.horizontal, card.backgroundScript == nil ? 0 : 8)
                .padding(.vertical, card.backgroundScript == nil ? 0 : 4)
                .overlay {
                    if card.backgroundScript != nil {
                        Capsule().stroke(AppTheme.border)
                    }
                }
            }
            .buttonStyle(.plain)
            .foregroundStyle(card.phase == .failed ? AppTheme.dangerForeground : AppTheme.muted)
        }
        .frame(minHeight: 38.5)
        .overlay(alignment: .bottom) { Rectangle().fill(AppTheme.borderSubtle).frame(height: 1) }
    }
}

extension SetupTone {
    var color: Color {
        switch self {
        case .muted: AppTheme.muted
        case .warning: AppTheme.warningForeground
        case .destructive: AppTheme.dangerForeground
        }
    }
}

private struct StageRow: View {
    let status: SetupStageStatus
    let label: String
    let trailing: String?
    let elapsed: String?
    let height: CGFloat

    var body: some View {
        HStack(spacing: 7) {
            icon.frame(width: 16)
            Text(label).font(AppTheme.font(14)).foregroundStyle(AppTheme.muted).lineLimit(1)
            Spacer()
            if let trailing {
                Text(trailing).font(AppTheme.font(12)).foregroundStyle(AppTheme.muted).lineLimit(1)
            }
            if let elapsed {
                Text(elapsed).font(AppTheme.font(12)).monospacedDigit().foregroundStyle(AppTheme.muted)
            }
        }
        .frame(minHeight: height)
        .opacity(status == .pending ? 0.4 : 1)
    }

    @ViewBuilder
    private var icon: some View {
        switch status {
        case .pending: Image(systemName: "circle").font(.system(size: 12))
        case .running: ProgressView().scaleEffect(0.75)
        case .done: Image(systemName: "checkmark").font(.system(size: 12))
        case .skipped: Image(systemName: "minus").font(.system(size: 12))
        case .failed: Image(systemName: "xmark").font(.system(size: 12)).foregroundStyle(AppTheme.dangerForeground)
        case .warning: Image(systemName: "exclamationmark.triangle").font(.system(size: 12))
        }
    }
}

private struct SetupDetailsSheet: View {
    let card: SetupCardView
    let cancel: () -> Void
    let workLocally: () -> Void
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 7) {
                    ForEach(Array(card.stages.enumerated()), id: \.offset) { _, stage in
                        StageRow(status: stage.status, label: stage.label, trailing: stage.trailing,
                                 elapsed: stage.elapsed, height: 38.5)
                        if let output = stage.output, !output.lines.isEmpty {
                            Text(output.lines.joined(separator: "\n"))
                                .font(.custom("Menlo", size: 12)).lineSpacing(3.5)
                                .lineLimit(4, reservesSpace: true)
                                .foregroundStyle(output.failed ? AppTheme.dangerForeground : AppTheme.text)
                                .padding(8)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .background(output.failed ? AppTheme.danger : AppTheme.cardAlt,
                                            in: RoundedRectangle(cornerRadius: 5.25))
                        }
                    }
                    ForEach(Array(card.details.enumerated()), id: \.offset) { _, detail in
                        HStack {
                            Text(detail.label).foregroundStyle(AppTheme.muted)
                            Spacer()
                            Text(detail.value).lineLimit(1).truncationMode(.middle)
                        }
                        .font(AppTheme.font(13))
                    }
                    if let error = card.error {
                        Text(error).font(AppTheme.font(13)).foregroundStyle(AppTheme.dangerForeground)
                    }
                    if card.canCancel || card.canWorkLocally {
                        HStack(spacing: 16) {
                            Spacer()
                            if card.canCancel {
                                Button("Cancel setup") {
                                    dismiss()
                                    cancel()
                                }
                                .foregroundStyle(AppTheme.dangerForeground)
                                .accessibilityLabel("Cancel worktree setup")
                            }
                            if card.canWorkLocally {
                                Button("Work locally") {
                                    dismiss()
                                    workLocally()
                                }
                                .foregroundStyle(AppTheme.text)
                            }
                        }
                        .buttonStyle(.plain)
                        .font(AppTheme.font(14))
                        .frame(minHeight: 44)
                        .padding(.top, 4)
                        .overlay(alignment: .top) { Rectangle().fill(AppTheme.border).frame(height: 1) }
                        .padding(.top, 12)
                    }
                }
                .padding(.horizontal, 20).padding(.vertical, 12)
            }
            .navigationTitle("Worktree setup")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
        }
    }
}
