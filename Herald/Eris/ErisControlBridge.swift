import Foundation

/// Raised when a control-plane call is attempted with no daemon paired.
enum ErisControlError: Error, LocalizedError {
    case notPaired

    var errorDescription: String? {
        switch self {
        case .notPaired: "No Eris daemon is paired."
        }
    }
}

/// The one place the daemon's control-plane shapes become the shapes the 0.4.0
/// stores already draw.
///
/// The mapping lives here rather than in the stores so a store keeps its own
/// model and the wire stays snake_case on one side and relay-shaped on the
/// other, with exactly one seam between them.
@MainActor
enum ErisControlMapping {
    /// The stamps `/v1/cron` carries, parsed. The daemon sends RFC 3339 UTC and
    /// may or may not include a fractional second, so both forms are tried
    /// before the field is given up as unreadable.
    private static let fractional: ISO8601DateFormatter = {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return f
    }()

    private static let plain: ISO8601DateFormatter = {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime]
        return f
    }()

    static func stamp(_ raw: String?) -> Date? {
        guard let raw, !raw.isEmpty else { return nil }
        return fractional.date(from: raw) ?? plain.date(from: raw)
    }

    /// The catalog, the pin, and the provider that would answer a turn.
    ///
    /// A pinned id that is not in the catalog still becomes the active model:
    /// the daemon holds that pin, so hiding it would leave the picker claiming
    /// nothing is selected on a daemon that has one selected.
    static func catalog(_ wire: ErisControlModels) -> (models: [ModelStore.HeraldModel], active: ModelStore.ActiveModel?) {
        let models = wire.models.map { row in
            ModelStore.HeraldModel(
                name: row.id,
                provider: row.provider ?? "",
                providerName: row.provider,
                contextWindow: row.contextWindow,
                isProviderDefault: row.isDefault
            )
        }
        let pinned = wire.model.trimmingCharacters(in: .whitespacesAndNewlines)
        // A pin is stored `provider/model` and a row's `id` is the bare id its
        // own provider spells. Match either spelling, or a pinned row is never
        // recognised: no tick is drawn and the active model is named after the
        // primary provider instead of the one that serves it.
        func isPinned(_ row: ErisControlModelRow) -> Bool {
            row.id == pinned || "\(row.provider ?? "")/\(row.id)" == pinned
        }
        let active: ModelStore.ActiveModel?
        if pinned.isEmpty {
            active = nil
        } else if let row = wire.models.first(where: isPinned) {
            active = ModelStore.ActiveModel(
                name: row.id,
                provider: row.provider,
                contextWindow: row.contextWindow
            )
        } else {
            active = ModelStore.ActiveModel(
                name: pinned,
                provider: wire.providers.first(where: \.primary)?.id,
                contextWindow: nil
            )
        }
        return (models, active)
    }

    static func job(_ wire: ErisControlCronJob) -> CronStore.CronJob {
        CronStore.CronJob(
            id: wire.id,
            name: wire.name,
            schedule: wire.schedule,
            prompt: wire.prompt,
            enabled: wire.enabled,
            lastRun: stamp(wire.lastRun),
            nextRun: stamp(wire.nextRun),
            lastResult: wire.lastStatus
        )
    }

    static func jobs(_ wire: ErisControlCronList) -> [CronStore.CronJob] {
        wire.jobs.map(job)
    }

    static func skill(_ wire: ErisControlSkill) -> SkillsStore.HeraldSkill {
        SkillsStore.HeraldSkill(
            name: wire.name,
            description: wire.description ?? "",
            path: wire.path ?? ""
        )
    }

    static func skills(_ wire: ErisControlSkillList) -> [SkillsStore.HeraldSkill] {
        wire.skills.map(skill)
    }

    static func skillDetail(_ wire: ErisControlSkillDetail) -> SkillsStore.SkillDetail {
        SkillsStore.SkillDetail(
            name: wire.name,
            path: wire.path,
            description: wire.description,
            content: wire.content
        )
    }

    /// The aux rows. `isAuto` is the daemon's `inherits`, which is the state a
    /// row draws as "follows the primary" rather than a match inferred here.
    static func auxTasks(_ wire: ErisControlAuxList) -> [AuxTask] {
        wire.roles.map { role in
            AuxTask(
                key: role.key,
                task: role.label,
                provider: role.provider,
                model: role.model,
                isAuto: role.inherits
            )
        }
    }
}
