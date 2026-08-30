import SwiftUI

struct CronManagerView: View {
    @Environment(CronStore.self) private var cronStore
    @State private var showCreateSheet = false

    var body: some View {
        Group {
            if cronStore.isLoading && cronStore.jobs.isEmpty {
                ProgressView("Loading cron jobs...")
            } else if let error = cronStore.errorMessage, cronStore.jobs.isEmpty {
                // Build 135.40: explicit error state instead of the bare
                // yellow-triangle empty state. Shows what failed + Retry.
                ContentUnavailableView {
                    Label("Unable to load cron jobs", systemImage: "exclamationmark.triangle")
                } description: {
                    Text(error)
                } actions: {
                    Button("Retry") {
                        Task { await cronStore.loadJobs() }
                    }
                    .buttonStyle(.borderedProminent)
                }
            } else if cronStore.jobs.isEmpty {
                ContentUnavailableView(
                    "No Cron Jobs",
                    systemImage: "clock.badge",
                    description: Text("Create a scheduled job to automate tasks.")
                )
            } else {
                List {
                    ForEach(cronStore.jobs) { job in
                        NavigationLink(value: job) {
                            HStack {
                                VStack(alignment: .leading, spacing: 4) {
                                    Text(job.name)
                                        .font(.headline)
                                    Text(job.schedule)
                                        .font(.caption)
                                        .foregroundStyle(.secondary)
                                    if let nextRun = job.nextRun {
                                        Text("Next: \(nextRun.formatted(.relative(presentation: .named)))")
                                            .font(.caption2)
                                            .foregroundStyle(.tertiary)
                                    }
                                }
                                Spacer()
                                Toggle("", isOn: Binding(
                                    get: { job.enabled },
                                    set: { _ in
                                        Task { try? await cronStore.toggleJob(job) }
                                    }
                                ))
                                .labelsHidden()
                            }
                        }
                        .swipeActions {
                            Button(role: .destructive) {
                                Task { try? await cronStore.deleteJob(job) }
                            } label: {
                                Label("Delete", systemImage: "trash")
                            }
                        }
                    }
                }
                .refreshable { await cronStore.loadJobs() }
            }
        }
        .navigationTitle("Cron Jobs")
        .navigationDestination(for: CronStore.CronJob.self) { job in
            CronJobDetailView(job: job)
        }
        .task { await cronStore.loadJobs() }
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button {
                    showCreateSheet = true
                } label: {
                    Image(systemName: "plus")
                }
            }
        }
        .sheet(isPresented: $showCreateSheet) {
            CreateCronJobSheet { name, schedule, prompt in
                Task { try? await cronStore.createJob(name: name, schedule: schedule, prompt: prompt) }
            }
        }
    }
}
