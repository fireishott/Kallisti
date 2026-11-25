//
//  ModelPickerSheet.swift
//  Kallisti
//
//  Choosing a model is choosing a provider and a model, so the picker shows
//  both: a searchable list grouped by the provider that publishes each id, with
//  the provider's initial in its own tile. A plain menu of 500 ids named the
//  model and hid the one fact that decides where the turn is spent.
//

import SwiftUI

/// One catalogue row. Decodes from the daemon's `{id, provider}` object and,
/// because an older daemon sent bare strings, from a string too.
struct RoutingModel: Decodable, Identifiable, Hashable, Sendable {
    let id: String
    let provider: String

    var providerLabel: String {
        switch provider.lowercased() {
        case "", "default": "Daemon's provider"
        case "antigravity", "ag": "Antigravity"
        case "claude", "cc", "claude-code": "Claude Code"
        case "commandcode", "cmc": "Command Code"
        case "deepseek", "ds": "DeepSeek"
        case "minimax", "mm": "MiniMax"
        default: provider.replacingOccurrences(of: "-", with: " ").capitalized
        }
    }

    /// Up to two letters for the provider tile, so a long list is scannable.
    var monogram: String {
        let words = providerLabel.split(separator: " ")
        if words.count >= 2 {
            return words.prefix(2).compactMap(\.first).map(String.init).joined().uppercased()
        }
        return String(providerLabel.prefix(2)).uppercased()
    }

    private enum CodingKeys: String, CodingKey { case id, provider }

    init(id: String, provider: String) {
        self.id = id
        self.provider = provider
    }

    init(from decoder: Decoder) throws {
        if let single = try? decoder.singleValueContainer(), let id = try? single.decode(String.self) {
            self.id = id
            self.provider = ""
            return
        }
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.id = try container.decode(String.self, forKey: .id)
        self.provider = (try? container.decodeIfPresent(String.self, forKey: .provider)) ?? ""
    }
}

struct ModelPickerSheet: View {
    let title: String
    let models: [RoutingModel]
    let selection: String
    let onPick: (String) -> Void
    let onDismiss: () -> Void

    @State private var query = ""

    /// How many catalogues publish this id. Two or more means the daemon cannot
    /// tell which one a bare id means, so a pick has to name the provider.
    private func publisherCount(_ id: String) -> Int {
        models.filter { $0.id == id }.count
    }

    /// Whether this row is the pin the daemon is on.
    ///
    /// The same id is published by several providers, so a name-only match ticked
    /// every row that shared it. The comparison is over the values this sheet
    /// itself writes: the bare id when one provider publishes it, and
    /// `provider/id` when more than one does. A stored pin of either shape is
    /// accepted for the row it belongs to, and nothing else matches.
    private func isSelected(_ model: RoutingModel) -> Bool {
        let trimmed = selection.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return false }
        // The exact value this row would write.
        if trimmed == value(for: model) { return true }
        // A pin stored in the qualified form for a row that currently reads as
        // unique (the other publisher was dropped from the catalogue) still
        // belongs to that provider, not to every row with the id.
        if trimmed == "\(model.provider)/\(model.id)" { return true }
        // A bare stored pin matches only when this id really is unambiguous, which
        // is the same rule `value(for:)` uses when writing one.
        if trimmed == model.id, publisherCount(model.id) == 1 { return true }
        return false
    }


    /// The value a pick writes. Bare when one provider publishes the id, which
    /// is what every pin written before providers were plural looks like;
    /// `provider/id` when two do.
    private func value(for model: RoutingModel) -> String {
        guard publisherCount(model.id) > 1 else { return model.id }
        return "\(model.provider)/\(model.id)"
    }

    private var grouped: [(provider: String, models: [RoutingModel])] {
        let needle = query.trimmingCharacters(in: .whitespaces).lowercased()
        let filtered = needle.isEmpty
            ? models
            : models.filter { $0.id.lowercased().contains(needle) || $0.providerLabel.lowercased().contains(needle) }
        let byProvider = Dictionary(grouping: filtered) { $0.providerLabel }
        return byProvider.keys.sorted().map { (provider: $0, models: byProvider[$0]!.sorted { $0.id < $1.id }) }
    }

    var body: some View {
        NavigationStack {
            List {
                Section {
                    Button {
                        onPick("")
                    } label: {
                        row(title: "The daemon's own", subtitle: "Inherit for this kind of turn", monogram: "AI", selected: selection.isEmpty)
                    }
                    .buttonStyle(.plain)
                }

                ForEach(grouped, id: \.provider) { group in
                    Section {
                        ForEach(group.models) { model in
                            Button {
                                onPick(value(for: model))
                            } label: {
                                row(
                                    title: model.id,
                                    subtitle: model.providerLabel,
                                    monogram: model.monogram,
                                    selected: isSelected(model)
                                )
                            }
                            .buttonStyle(.plain)
                        }
                    } header: {
                        HStack(spacing: Design.Spacing.xs) {
                            providerTile(group.models.first?.monogram ?? "—", size: 18)
                            Text(group.provider)
                        }
                        .font(Design.Typography.caption)
                        .foregroundStyle(Design.Colors.secondaryForeground)
                    }
                }
            }
            .searchable(text: $query, prompt: "Search models or providers")
            .navigationTitle(title)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { onDismiss() }
                }
            }
        }
    }

    private func row(title: String, subtitle: String, monogram: String, selected: Bool) -> some View {
        HStack(spacing: Design.Spacing.sm) {
            providerTile(monogram, size: 30)
            VStack(alignment: .leading, spacing: 2) {
                Text(title)
                    .font(Design.Typography.callout)
                    .foregroundStyle(Design.Colors.foreground)
                Text(subtitle)
                    .font(Design.Typography.caption)
                    .foregroundStyle(Design.Colors.secondaryForeground)
            }
            Spacer()
            if selected {
                Image(systemName: "checkmark.circle.fill")
                    .foregroundStyle(Design.Brand.accent)
            }
        }
        .contentShape(Rectangle())
        .padding(.vertical, 2)
    }

    private func providerTile(_ monogram: String, size: CGFloat) -> some View {
        Text(monogram)
            .font(.system(size: size * 0.38, weight: .bold, design: .rounded))
            .foregroundStyle(Design.Colors.background)
            .frame(width: size, height: size)
            .background(Design.Brand.accent.opacity(0.85), in: RoundedRectangle(cornerRadius: size * 0.3))
    }
}
