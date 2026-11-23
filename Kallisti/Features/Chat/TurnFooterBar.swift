import SwiftUI

// MARK: - Turn accounting

/// What a finished turn cost, as the daemon records it on the message row.
///
/// Every field is optional on purpose. The daemon writes only the numbers the
/// provider actually reported, so a number that is absent here is a fact it
/// never stated, never a zero. A row written before the daemon grew the object
/// carries no `accounting` at all, and that has to decode rather than take the
/// whole transcript down with it.
struct TurnAccounting: Equatable, Sendable {
    /// Prompt tokens the provider reported.
    let promptTokens: Int?
    /// Completion tokens the provider reported.
    let completionTokens: Int?
    /// Prompt tokens the provider served from cache, when it said so.
    let cachedTokens: Int?
    /// Wall time from request to last byte.
    let durationMs: Int?
    /// Tool rounds the turn ran.
    let rounds: Int?
    /// The last round trip's own prompt size, which is the live context size.
    let contextTokens: Int?

    init(
        promptTokens: Int? = nil,
        completionTokens: Int? = nil,
        cachedTokens: Int? = nil,
        durationMs: Int? = nil,
        rounds: Int? = nil,
        contextTokens: Int? = nil
    ) {
        self.promptTokens = promptTokens
        self.completionTokens = completionTokens
        self.cachedTokens = cachedTokens
        self.durationMs = durationMs
        self.rounds = rounds
        self.contextTokens = contextTokens
    }

    /// Whether the row stated anything at all.
    var isEmpty: Bool {
        promptTokens == nil && completionTokens == nil && cachedTokens == nil
            && durationMs == nil && rounds == nil && contextTokens == nil
    }

    /// Wall time as "12.4s" under a minute and "1m 04s" past it. Sub-second
    /// turns keep a decimal, because "0s" for a real answer reads like a bug.
    var durationText: String? {
        guard let durationMs else { return nil }
        if durationMs < 60_000 {
            let seconds = Double(durationMs) / 1_000
            return String(format: "%.1fs", seconds)
        }
        let total = durationMs / 1_000
        return String(format: "%dm %02ds", total / 60, total % 60)
    }

    /// Tokens in and out, either half shown when only one was reported.
    var tokensText: String? {
        switch (promptTokens, completionTokens) {
        case let (input?, output?):
            return "\(Self.compact(input)) in / \(Self.compact(output)) out"
        case let (input?, nil):
            return "\(Self.compact(input)) in"
        case let (nil, output?):
            return "\(Self.compact(output)) out"
        case (nil, nil):
            return nil
        }
    }

    /// The share of the prompt the provider served from cache.
    ///
    /// The denominator is the whole prompt, cached and uncached together, so a
    /// turn that was mostly cache shows as such. Zero on the bottom means there
    /// was nothing to divide, and no percentage is printed rather than 0%.
    var cacheHitPercent: Int? {
        guard let cachedTokens else { return nil }
        let prompt = promptTokens ?? 0
        let total = prompt + cachedTokens
        guard total > 0 else { return nil }
        return Int((Double(cachedTokens) / Double(total) * 100).rounded())
    }

    var cacheText: String? {
        guard let percent = cacheHitPercent else { return nil }
        return "cache \(percent)%"
    }

    var roundsText: String? {
        guard let rounds, rounds > 0 else { return nil }
        return rounds == 1 ? "1 round" : "\(rounds) rounds"
    }

    var contextText: String? {
        guard let contextTokens else { return nil }
        return "ctx \(Self.compact(contextTokens))"
    }

    /// One decimal of thousands, so a footer line stays one line.
    static func compact(_ value: Int) -> String {
        guard value >= 1_000 else { return "\(value)" }
        let thousands = Double(value) / 1_000
        return String(format: thousands >= 100 ? "%.0fk" : "%.1fk", thousands)
    }
}

extension TurnAccounting: Decodable {
    private enum CodingKeys: String, CodingKey {
        case promptTokens = "prompt_tokens"
        case completionTokens = "completion_tokens"
        case cachedTokens = "cached_tokens"
        case durationMs = "duration_ms"
        case rounds
        case contextTokens = "context_tokens"
    }

    /// Tolerant on purpose: each key is read on its own, so one field a newer
    /// daemon sends differently costs that field and not the message it sits on.
    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        func count(_ key: CodingKeys) -> Int? {
            if let value = try? container.decodeIfPresent(Int.self, forKey: key) {
                return value
            }
            // A daemon that serialises a u64 through a float still means a count.
            if let value = try? container.decodeIfPresent(Double.self, forKey: key) {
                return Int(value)
            }
            return nil
        }
        self.init(
            promptTokens: count(.promptTokens),
            completionTokens: count(.completionTokens),
            cachedTokens: count(.cachedTokens),
            durationMs: count(.durationMs),
            rounds: count(.rounds),
            contextTokens: count(.contextTokens)
        )
    }
}

// MARK: - Preferences

/// The six switches the settings screen writes and the footer reads.
///
/// Keep the strings here and nowhere else: a literal repeated in two files is
/// a footer that stops following its own toggle.
enum TurnFooterPrefs {
    static let enabled = "kallisti.turnFooter.enabled"
    static let rounds = "kallisti.turnFooter.rounds"
    static let duration = "kallisti.turnFooter.duration"
    static let tokens = "kallisti.turnFooter.tokens"
    static let cache = "kallisti.turnFooter.cache"
    static let context = "kallisti.turnFooter.context"
}

// MARK: - Footer

/// The one-line receipt under a finished answer.
///
/// It draws nothing at all - no divider, no empty strip - when the master
/// switch is off, when every field is off, or when the row carried no numbers.
/// A turn that was never measured must not leave a gap in the transcript.
struct TurnFooterBar: View {
    let accounting: TurnAccounting?

    @AppStorage(TurnFooterPrefs.enabled) private var isEnabled: Bool = true
    @AppStorage(TurnFooterPrefs.rounds) private var showsRounds: Bool = true
    @AppStorage(TurnFooterPrefs.duration) private var showsDuration: Bool = true
    @AppStorage(TurnFooterPrefs.tokens) private var showsTokens: Bool = true
    @AppStorage(TurnFooterPrefs.cache) private var showsCache: Bool = true
    @AppStorage(TurnFooterPrefs.context) private var showsContext: Bool = false

    var body: some View {
        if let line {
            Text(line)
                .font(.system(.caption2, design: .monospaced))
                .foregroundStyle(.secondary)
                .lineLimit(1)
                .minimumScaleFactor(0.8)
                .accessibilityLabel(line)
        }
    }

    /// The fields that are both switched on and actually known, in reading
    /// order. nil when there are none, which is what keeps the row at zero
    /// height instead of leaving a trimmed blank.
    var line: String? {
        guard isEnabled, let accounting else { return nil }
        var fields: [String] = []
        if showsRounds, let text = accounting.roundsText { fields.append(text) }
        if showsDuration, let text = accounting.durationText { fields.append(text) }
        if showsTokens, let text = accounting.tokensText { fields.append(text) }
        if showsCache, let text = accounting.cacheText { fields.append(text) }
        if showsContext, let text = accounting.contextText { fields.append(text) }
        guard !fields.isEmpty else { return nil }
        return fields.joined(separator: "  \u{00B7}  ")
    }
}

// MARK: - Live round

/// The turn loop's current round, as one `round` frame stated it.
///
/// The count is one-based on the wire. Zero means no round has been announced,
/// and that is how the badge knows to stay off the screen.
struct LiveRoundProgress: Equatable, Sendable {
    var count: Int
    var total: Int

    init(count: Int = 0, total: Int = 0) {
        self.count = count
        self.total = total
    }

    /// Read a round straight off a decoded frame.
    init(frame: ErisFrame) {
        if case let .round(count, total, _) = frame {
            self.init(count: count, total: total)
        } else {
            self.init()
        }
    }

    var isVisible: Bool { count > 0 }

    var text: String? {
        guard count > 0 else { return nil }
        return total > 0 ? "round \(count)/\(total)" : "round \(count)"
    }
}

/// `round 3/100` beside the working indicator while a turn streams.
///
/// A long turn used to show a spinner and nothing else, which is the shape the
/// operator cannot tell apart from a daemon that has stopped working.
struct LiveRoundBadge: View {
    let progress: LiveRoundProgress

    var body: some View {
        if let text = progress.text {
            Text(text)
                .font(.system(.caption2, design: .monospaced))
                .foregroundStyle(.secondary)
                .monospacedDigit()
                .accessibilityLabel("tool round \(progress.count) of \(progress.total)")
        }
    }
}

// MARK: - Settings section

/// The "Turn footer" block. Field toggles hide under the master switch so a
/// footer the operator turned off cannot leave five live switches behind it.
struct TurnFooterSettingsSection: View {
    @AppStorage(TurnFooterPrefs.enabled) private var isEnabled: Bool = true
    @AppStorage(TurnFooterPrefs.rounds) private var showsRounds: Bool = true
    @AppStorage(TurnFooterPrefs.duration) private var showsDuration: Bool = true
    @AppStorage(TurnFooterPrefs.tokens) private var showsTokens: Bool = true
    @AppStorage(TurnFooterPrefs.cache) private var showsCache: Bool = true
    @AppStorage(TurnFooterPrefs.context) private var showsContext: Bool = false

    var body: some View {
        Section {
            Toggle("Show turn footer", isOn: $isEnabled)
            if isEnabled {
                Toggle("Rounds", isOn: $showsRounds)
                Toggle("Wall time", isOn: $showsDuration)
                Toggle("Tokens in / out", isOn: $showsTokens)
                Toggle("Cache hit", isOn: $showsCache)
                Toggle("Context tokens", isOn: $showsContext)
            }
        } header: {
            Text("Turn footer")
        } footer: {
            Text("One line under a finished answer. A field with nothing measured is left out, never shown as zero.")
        }
    }
}
