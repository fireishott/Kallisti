import SwiftUI

/// Sheet listing every model configured on the Hermes host, grouped by
/// provider. Selecting a model switches it directly via
/// `ModelStore.switchModel(to:provider:)` (`POST /v1/model`).
struct ModelSelectorSheet: View {
    @Environment(\.dismiss) private var dismiss
    @Environment(ModelStore.self) private var modelStore

    @State private var setAsGlobalDefault = false
    @State private var isSwitching = false
    @State private var switchingModelID: String?
    @State private var switchError: String?

    /// Called after a model switch succeeds (the switch itself already
    /// happened via `ModelStore.switchModel`). Lets the presenter react —
    /// e.g. dismiss a popover — without re-dispatching anything.
    var onSelect: (ModelStore.HermesModel, Bool) -> Void

    var body: some View {
        NavigationStack {
            VStack(spacing: 0) {
                if let switchError {
                    errorBanner(message: switchError)
                        .padding(.horizontal, Design.Spacing.md)
                        .padding(.top, Design.Spacing.sm)
                }

                Group {
                    if modelStore.isLoading && modelStore.models.isEmpty {
                        ProgressView("Loading models…")
                            .frame(maxWidth: .infinity, maxHeight: .infinity)
                    } else if modelStore.models.isEmpty {
                        emptyState
                    } else {
                        modelList
                    }
                }
            }
            .background(Design.Colors.background)
            .navigationTitle("Switch Model")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Done") { dismiss() }
                }
            }
        }
        .task {
            await modelStore.loadModels(force: true)
        }
    }

    private var modelList: some View {
        List {
            ForEach(modelStore.modelsByProvider, id: \.provider) { group in
                Section(group.provider) {
                    ForEach(group.models) { model in
                        modelRow(model)
                    }
                }
            }

            Section {
                Toggle(isOn: $setAsGlobalDefault) {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Set as default")
                            .font(Design.Typography.callout)
                        Text("Persist beyond the current session (--global)")
                            .font(Design.Typography.caption)
                            .foregroundStyle(Design.Colors.secondaryForeground)
                    }
                }
                .tint(Design.Brand.accent)
            }
        }
        .scrollContentBackground(.hidden)
    }

    private func modelRow(_ model: ModelStore.HermesModel) -> some View {
        Button {
            selectModel(model)
        } label: {
            HStack(spacing: Design.Spacing.sm) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(model.name)
                        .font(.system(.callout, design: .monospaced, weight: .medium))
                        .foregroundStyle(Design.Colors.foreground)
                        .lineLimit(1)

                    HStack(spacing: 6) {
                        if let contextWindow = model.contextWindow {
                            Text("\(formatTokenCount(contextWindow)) context")
                                .font(Design.Typography.caption)
                                .foregroundStyle(Design.Colors.secondaryForeground)
                        }
                        if model.isProviderDefault == true {
                            Text("provider default")
                                .font(Design.Typography.caption)
                                .foregroundStyle(Design.Colors.secondaryForeground)
                        }
                    }
                }

                Spacer()

                if isSwitching && switchingModelID == model.id {
                    ProgressView()
                } else if modelStore.isActive(model) {
                    Image(systemName: "checkmark.circle.fill")
                        .foregroundStyle(Design.Brand.accent)
                }
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(isSwitching)
    }

    private func selectModel(_ model: ModelStore.HermesModel) {
        switchError = nil
        isSwitching = true
        switchingModelID = model.id
        Task {
            do {
                try await modelStore.switchModel(to: model.name, provider: model.provider)
                isSwitching = false
                onSelect(model, setAsGlobalDefault)
                dismiss()
            } catch {
                isSwitching = false
                switchError = error.localizedDescription
            }
        }
    }

    private func errorBanner(message: String) -> some View {
        HStack(spacing: Design.Spacing.sm) {
            Image(systemName: "exclamationmark.triangle.fill")
                .foregroundStyle(.orange)
            Text(message)
                .font(Design.Typography.caption)
                .foregroundStyle(Design.Colors.foreground)
                .lineLimit(2)
        }
        .padding(Design.Spacing.md)
        .background(Design.Colors.surface)
        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.lg))
    }

    private var emptyState: some View {
        VStack(spacing: Design.Spacing.md) {
            Image(systemName: "cpu")
                .font(.system(size: 32))
                .foregroundStyle(Design.Colors.secondaryForeground)
            Text(modelStore.errorMessage ?? "No models available")
                .font(Design.Typography.callout)
                .foregroundStyle(Design.Colors.secondaryForeground)
                .multilineTextAlignment(.center)
            Text("Model list comes from the Hermes host — make sure it's online.")
                .font(Design.Typography.caption)
                .foregroundStyle(Design.Colors.secondaryForeground)
                .multilineTextAlignment(.center)

            Button("Retry") {
                Task { await modelStore.loadModels(force: true) }
            }
            .buttonStyle(.bordered)
        }
        .padding(Design.Spacing.lg)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private func formatTokenCount(_ count: Int) -> String {
        if count >= 1_000_000 {
            return String(format: "%.1fM", Double(count) / 1_000_000)
                .replacingOccurrences(of: ".0M", with: "M")
        } else if count >= 1_000 {
            return "\(count / 1_000)K"
        }
        return "\(count)"
    }
}
