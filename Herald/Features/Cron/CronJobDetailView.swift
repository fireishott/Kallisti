import SwiftUI

struct CronJobDetailView: View {
    let job: CronStore.CronJob

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                LabeledContent("Schedule", value: job.schedule)
                if let lastRun = job.lastRun {
                    LabeledContent("Last Run", value: lastRun.formatted())
                }
                if let nextRun = job.nextRun {
                    LabeledContent("Next Run", value: nextRun.formatted())
                }
                if let result = job.lastResult, !result.isEmpty {
                    Divider()
                    Text("Last Result")
                        .font(.headline)
                    Text(result)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Divider()
                Text("Prompt")
                    .font(.headline)
                Text(job.prompt)
                    .font(.body)
            }
            .padding()
        }
        .navigationTitle(job.name)
        .navigationBarTitleDisplayMode(.inline)
    }
}
