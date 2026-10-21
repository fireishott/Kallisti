import SwiftUI

// MARK: - Senses model

/// Read-only model for the paired daemon's health/location snapshot.
///
/// Loads the latest metric per tracked kind and the most recent location from
/// `GET /v1/device/health/latest` and `GET /v1/device/location`. Both are read
/// independently so a daemon that does not yet serve one route cannot blank the
/// other: a missing value stays "no data" rather than a hard failure.
@MainActor
@Observable
final class ErisSensesModel {
    private(set) var metrics: [ErisHealthMetric] = []
    private(set) var location: ErisLocationSnapshot?
    private(set) var isLoading = false
    private(set) var hasLoaded = false

    private let client: ErisClient

    init(client: ErisClient) {
        self.client = client
    }

    func loadIfNeeded() async {
        guard !hasLoaded else { return }
        await load()
    }

    func load() async {
        isLoading = true
        defer { isLoading = false; hasLoaded = true }
        if let latest = try? await client.latestHealthMetrics() {
            metrics = latest.metrics
        }
        if let loc = try? await client.latestLocation() {
            location = loc
        }
    }
}

// MARK: - Senses screen

/// The health + location data view. Read-only: it reports what the daemon holds,
/// never asks the device to collect on demand (collection is the upload
/// pipeline's job, behind the permission gate).
struct ErisSensesScreen: View {
    let model: ErisSensesModel

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: Design.Spacing.lg) {
                if model.isLoading, !model.hasLoaded {
                    ErisLoadingView(label: "Reading your senses…")
                } else {
                    locationSection
                    healthSection
                }
            }
            .padding(Design.Spacing.md)
            .padding(.bottom, Design.Spacing.xl)
        }
        .background(Design.Colors.background)
        .navigationTitle("Senses")
        .navigationBarTitleDisplayMode(.inline)
        .task { await model.loadIfNeeded() }
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button {
                    Task { await model.load() }
                } label: {
                    Label("Reload", systemImage: "arrow.clockwise")
                        .font(Design.Typography.footnote)
                }
                .disabled(model.isLoading)
                .accessibilityIdentifier("eris.reloadSenses")
            }
        }
    }

    // MARK: Sections

    private var locationSection: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.sm) {
            sectionHeader("Location")
            if let loc = model.location {
                card {
                    VStack(alignment: .leading, spacing: Design.Spacing.xxs) {
                        Label(
                            String(format: "%.5f, %.5f", loc.latitude, loc.longitude),
                            systemImage: "location.fill"
                        )
                        .font(Design.Typography.body)
                        .foregroundStyle(Design.Colors.foreground)

                        if let address = loc.address, !address.isEmpty {
                            Text(address)
                                .font(Design.Typography.footnote)
                                .foregroundStyle(Design.Colors.secondaryForeground)
                        }
                        if let recordedAt = Self.friendlyStamp(loc.recordedAt) {
                            Text("recorded \(recordedAt)")
                                .font(Design.Typography.helper)
                                .foregroundStyle(Design.Colors.tertiaryForeground)
                        }
                    }
                }
            } else {
                noData
            }
        }
    }

    private var healthSection: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.sm) {
            sectionHeader("Health")
            let byKey = Dictionary(uniqueKeysWithValues: model.metrics.map { ($0.metric, $0) })
            ForEach(Self.descriptors) { descriptor in
                card {
                    metricRow(descriptor, metric: byKey[descriptor.key])
                }
            }
        }
    }

    private func sectionHeader(_ title: String) -> some View {
        Text(title)
            .font(Design.Typography.sectionTitle)
            .foregroundStyle(Design.Colors.foreground)
    }

    @ViewBuilder
    private func card(@ViewBuilder content: () -> some View) -> some View {
        content()
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(Design.Spacing.sm)
            .background(Design.Colors.backgroundRaised)
            .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.lg))
            .overlay(
                RoundedRectangle(cornerRadius: Design.CornerRadius.lg)
                    .stroke(Design.Colors.border, lineWidth: 1)
            )
    }

    private func metricRow(_ descriptor: MetricDescriptor, metric: ErisHealthMetric?) -> some View {
        HStack(spacing: Design.Spacing.sm) {
            Image(systemName: descriptor.icon)
                .foregroundStyle(Design.Brand.accent)
                .frame(width: 28)
            Text(descriptor.label)
                .font(Design.Typography.body)
                .foregroundStyle(Design.Colors.foreground)
            Spacer()
            if let metric {
                Text("\(Self.format(metric.value)) \(metric.unit)")
                    .font(Design.Typography.body)
                    .foregroundStyle(Design.Colors.foreground)
            } else {
                Text("no data")
                    .font(Design.Typography.footnote)
                    .foregroundStyle(Design.Colors.tertiaryForeground)
            }
        }
    }

    private var noData: some View {
        Text("no data")
            .font(Design.Typography.footnote)
            .foregroundStyle(Design.Colors.tertiaryForeground)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(Design.Spacing.sm)
            .background(Design.Colors.backgroundRaised)
            .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.lg))
    }

    // MARK: Metric descriptors (the 11 legacy metrics)

    private struct MetricDescriptor: Identifiable {
        let key: String
        let label: String
        let icon: String
        var id: String { key }
    }

    private static let descriptors: [MetricDescriptor] = [
        MetricDescriptor(key: "steps", label: "Steps", icon: "figure.walk"),
        MetricDescriptor(key: "active_calories", label: "Active Calories", icon: "flame"),
        MetricDescriptor(key: "distance_walking", label: "Walking Distance", icon: "figure.walk.motion"),
        MetricDescriptor(key: "heart_rate", label: "Heart Rate", icon: "heart"),
        MetricDescriptor(key: "resting_heart_rate", label: "Resting Heart Rate", icon: "heart.fill"),
        MetricDescriptor(key: "blood_oxygen", label: "Blood Oxygen", icon: "drop"),
        MetricDescriptor(key: "respiratory_rate", label: "Respiratory Rate", icon: "lungs"),
        MetricDescriptor(key: "body_mass", label: "Body Mass", icon: "scalemass"),
        MetricDescriptor(key: "workout_minutes", label: "Workout Minutes", icon: "figure.run"),
        MetricDescriptor(key: "stand_hours", label: "Stand Hours", icon: "figure.stand"),
        MetricDescriptor(key: "sleep_duration", label: "Sleep Duration", icon: "bed.double"),
    ]

    // MARK: Formatting

    private static func format(_ value: Double) -> String {
        if value == value.rounded() {
            return String(Int(value))
        }
        return String(format: "%.1f", value)
    }

    /// Turn an ISO8601 stamp into a compact "Mar 21, 14:03" string, or nil.
    private static func friendlyStamp(_ text: String) -> String? {
        guard !text.isEmpty else { return nil }
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        let date = formatter.date(from: text) ?? {
            let plain = ISO8601DateFormatter()
            plain.formatOptions = [.withInternetDateTime]
            return plain.date(from: text)
        }()
        guard let date else { return text }
        let out = DateFormatter()
        out.dateFormat = "MMM d, HH:mm"
        return out.string(from: date)
    }
}
