import AgentCore
import SwiftUI

/// "Custom snooze": a date and time, or a duration from now.
struct CustomSnoozeSheet: View {
    let snooze: (String) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var byDate = true
    @State private var date = Date().addingTimeInterval(3600)
    @State private var amount = 2
    @State private var unit = SnoozeDurationUnit.hours
    @State private var error: String?

    var body: some View {
        NavigationStack {
            VStack(spacing: 16) {
                Picker("Mode", selection: $byDate) {
                    Text("Date and time").tag(true)
                    Text("Duration").tag(false)
                }
                .pickerStyle(.segmented)
                if byDate {
                    DatePicker("", selection: $date, displayedComponents: [.date, .hourAndMinute])
                        .datePickerStyle(.wheel).labelsHidden().frame(height: 180)
                } else {
                    HStack(spacing: 0) {
                        Picker("Amount", selection: $amount) {
                            ForEach(1 ..< 100, id: \.self) { Text("\($0)").tag($0) }
                        }
                        Picker("Unit", selection: $unit) {
                            Text("Minutes").tag(SnoozeDurationUnit.minutes)
                            Text("Hours").tag(SnoozeDurationUnit.hours)
                            Text("Days").tag(SnoozeDurationUnit.days)
                        }
                    }
                    .pickerStyle(.wheel).frame(height: 180)
                }
                if let error {
                    Text(error).font(.footnote).foregroundStyle(AppTheme.dangerForeground)
                }
            }
            .padding(16)
            .navigationTitle("Custom snooze")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button { dismiss() } label: { Image(systemName: "xmark") }
                        .accessibilityLabel("Cancel custom snooze")
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Snooze", action: submit)
                }
            }
        }
        .presentationDetents([.height(error == nil ? 324 : 364)])
    }

    private func submit() {
        let now = Int64(Date().timeIntervalSince1970 * 1000)
        let input: CustomSnoozeInput = if byDate {
            .date(date: Self.day.string(from: date), time: Self.clock.string(from: date))
        } else {
            .duration(amount: String(amount), unit: unit)
        }
        guard let until = customSnooze(nowMs: now, input: input) else {
            error = byDate ? "Choose a date and time in the future." : "Enter a positive duration."
            return
        }
        snooze(until)
        dismiss()
    }

    private static let day: DateFormatter = {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.dateFormat = "yyyy-MM-dd"
        return formatter
    }()

    private static let clock: DateFormatter = {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.dateFormat = "HH:mm"
        return formatter
    }()
}
