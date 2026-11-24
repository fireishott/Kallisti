import SwiftUI

// MARK: - Turn accounting

/// What a finished turn cost, as the daemon records it on the message row.
///
/// Every field is optional on purpose. The daemon writes only the numbers the
/// provider actually reported, so a number that is absent here is a fact it
/// never stated, never a zero. A row written before the daemon grew the object
/// carries no `accounting` at all, and that has to decode rather than take the
/// whole transcript down with it.
///
/// The field set follows the `hermes-signature` footer this replaced: what model
/// and provider answered, how long it took, what it spent in tokens, how much of
/// that came from cache, what it cost in dollars, what the session has cost so
/// far, how many turns deep the conversation is, and how full the window was.
struct TurnAccounting: Equatable, Hashable, Sendable {
    /// The model the daemon resolved for this turn, when it named one.
    ///
    /// Carried on the frame and on the row. A footer that omits the model says
    /// less than the same answer would in a Discord message, which is where this
    /// shape came from.
    let model: String?
    /// The provider that served it, when the daemon named one.
    let provider: String?
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
    /// The window declared for this model, so the context can be shown as a
    /// share of what it had to work with.
    let contextWindow: Int?
    /// What this turn cost in US dollars, when the operator declared a price.
    ///
    /// Absent is a fact, not a zero: the daemon does not guess a rate, so an
    /// unpriced model produces nothing here and the footer says nothing rather
    /// than "free".
    let costUSD: Double?
    /// What the whole conversation has cost so far, on the same terms.
    let sessionCostUSD: Double?
    /// Answers in this conversation that carry a price.
    ///
    /// Compared with the session's answer count to tell a complete total from a
    /// floor. The daemon sends both; the footer only claims a total when they
    /// agree.
    let pricedAnswers: Int?
    /// Answers in this conversation, which is the turn counter's denominator
    /// and its source.
    let sessionAnswers: Int?
    /// How many turns deep this conversation is.
    ///
    /// Read from the conversation's own total rather than counted in the footer:
    /// the transcript is paged, so a count taken from the messages on screen
    /// would start at one every time an older page loaded.
    let sessionTurns: Int?
    /// The tools this turn ran, as a count.
    ///
    /// Kept as a count rather than names so the footer stays one line: the tool
    /// rail under the answer is where the detail belongs.
    let toolCalls: Int?
    /// Whether the tool budget, not the model, ended this turn.
    let capped: Bool?
    /// Whether the engine took the tools away early and the model wrote the
    /// answer itself.
    let wrappedUp: Bool?

    init(
        model: String? = nil,
        provider: String? = nil,
        promptTokens: Int? = nil,
        completionTokens: Int? = nil,
        cachedTokens: Int? = nil,
        durationMs: Int? = nil,
        rounds: Int? = nil,
        contextTokens: Int? = nil,
        contextWindow: Int? = nil,
        costUSD: Double? = nil,
        sessionCostUSD: Double? = nil,
        pricedAnswers: Int? = nil,
        sessionAnswers: Int? = nil,
        sessionTurns: Int? = nil,
        toolCalls: Int? = nil,
        capped: Bool? = nil,
        wrappedUp: Bool? = nil
    ) {
        self.model = model
        self.provider = provider
        self.promptTokens = promptTokens
        self.completionTokens = completionTokens
        self.cachedTokens = cachedTokens
        self.durationMs = durationMs
        self.rounds = rounds
        self.contextTokens = contextTokens
        self.contextWindow = contextWindow
        self.costUSD = costUSD
        self.sessionCostUSD = sessionCostUSD
        self.pricedAnswers = pricedAnswers
        self.sessionAnswers = sessionAnswers
        self.sessionTurns = sessionTurns
        self.toolCalls = toolCalls
        self.capped = capped
        self.wrappedUp = wrappedUp
    }

    /// Read one receipt out of a `done` frame's `accounting` object.
    ///
    /// The stream and the transcript carry the same object, so this reads the
    /// same keys the `Decodable` conformance does, for the one caller that has a
    /// dictionary rather than a decoder. Every key is optional and independently
    /// tolerated: a frame that names three of these fields still produces a
    /// footer with three fields.
    init?(json: [String: Any]) {
        func count(_ key: String) -> Int? {
            if let value = json[key] as? Int { return value }
            if let value = json[key] as? Double { return Int(value) }
            if let value = json[key] as? NSNumber { return value.intValue }
            return nil
        }
        func text(_ key: String) -> String? {
            guard let value = json[key] as? String else { return nil }
            let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
            return trimmed.isEmpty ? nil : trimmed
        }
        func money(_ key: String) -> Double? {
            if let value = json[key] as? Double { return value.isFinite ? value : nil }
            if let value = json[key] as? Int { return Double(value) }
            if let value = json[key] as? NSNumber { return value.doubleValue }
            return nil
        }
        func flag(_ key: String) -> Bool? {
            if let value = json[key] as? Bool { return value }
            if let value = json[key] as? NSNumber { return value.boolValue }
            return nil
        }
        // An object carrying none of the fields is "no receipt", not a receipt
        // of nothing: returning one would let the footer draw a blank line.
        let candidate = TurnAccounting(
            model: text("model"),
            provider: text("provider"),
            promptTokens: count("prompt_tokens"),
            completionTokens: count("completion_tokens"),
            cachedTokens: count("cached_tokens"),
            durationMs: count("duration_ms"),
            rounds: count("rounds"),
            contextTokens: count("context_tokens"),
            contextWindow: count("context_window"),
            costUSD: money("cost_usd"),
            sessionCostUSD: money("session_cost_usd"),
            pricedAnswers: count("priced_answers"),
            sessionAnswers: count("session_answers"),
            sessionTurns: count("session_turns"),
            toolCalls: count("tool_calls"),
            capped: flag("capped"),
            wrappedUp: flag("wrapped_up")
        )
        guard !candidate.isEmpty else { return nil }
        self = candidate
    }

    /// Whether the row stated anything at all.
    var isEmpty: Bool {
        model == nil && provider == nil && promptTokens == nil && completionTokens == nil
            && cachedTokens == nil && durationMs == nil && rounds == nil
            && contextTokens == nil && costUSD == nil
    }

    // MARK: Rendered fields

    /// The model, trimmed. Empty ids are treated as absent: a daemon that sends
    /// `""` said nothing, and a footer with a trailing separator is worse than
    /// one without the field.
    var modelText: String? {
        guard let model else { return nil }
        let trimmed = model.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        return trimmed
    }

    var providerText: String? {
        guard let provider else { return nil }
        let trimmed = provider.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        return trimmed
    }

    /// Wall time as "1.4s" under a minute and "1m 04s" past it. Sub-second
    /// turns keep a decimal, because "0s" for a real answer reads like a bug.
    ///
    /// Labelled `est.` the way the Hermes footer labelled it: the daemon times
    /// from request to last byte, which includes time the provider spent
    /// queueing, so the figure is an estimate of the turn rather than a
    /// measurement of the model.
    var durationText: String? {
        guard let durationMs else { return nil }
        if durationMs < 60_000 {
            let seconds = Double(durationMs) / 1_000
            return String(format: "~%.1fs est.", seconds)
        }
        let total = durationMs / 1_000
        return String(format: "~%dm %02ds est.", total / 60, total % 60)
    }

    /// Tokens in and out, either half shown when only one was reported.
    var tokensText: String? {
        switch (promptTokens, completionTokens) {
        case let (input?, output?):
            return "\(Self.compact(input))↑ \(Self.compact(output))↓"
        case let (input?, nil):
            return "\(Self.compact(input))↑"
        case let (nil, output?):
            return "\(Self.compact(output))↓"
        case (nil, nil):
            return nil
        }
    }

    /// The combined count, which is the number the Hermes footer showed beside
    /// the split.
    var totalTokensText: String? {
        let input = promptTokens ?? 0
        let output = completionTokens ?? 0
        let total = input + output
        guard total > 0 else { return nil }
        return "\(Self.compact(total)) tok"
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

    /// The cached count itself, which is more useful than the share when the
    /// operator is watching a long conversation get expensive.
    var cacheText: String? {
        guard let cachedTokens, cachedTokens > 0 else { return nil }
        var text = "\(Self.compact(cachedTokens)) cached"
        if let percent = cacheHitPercent {
            text += " (\(percent)%)"
        }
        return text
    }

    var roundsText: String? {
        guard let rounds, rounds > 0 else { return nil }
        return rounds == 1 ? "1 round" : "\(rounds) rounds"
    }

    /// The tool calls the turn ran, as a count.
    var toolsText: String? {
        guard let toolCalls, toolCalls > 0 else { return nil }
        return toolCalls == 1 ? "1 tool" : "\(toolCalls) tools"
    }

    /// The live context size, plus the window when the daemon knew it.
    var contextText: String? {
        guard let contextTokens else { return nil }
        guard let window = contextWindow, window > 0 else {
            return "ctx \(Self.compact(contextTokens))"
        }
        let percent = Int((Double(contextTokens) / Double(window) * 100).rounded())
        return "ctx \(Self.compact(contextTokens))/\(Self.compact(window)) (\(percent)%)"
    }

    /// Dollars, on the same rule the Hermes footer used.
    ///
    /// Four decimals below a cent because a turn costs a fraction of one, and
    /// `$0.00` under every answer reads like a broken meter. A real but
    /// smaller-than-displayable charge says so instead of rounding to nothing.
    static func money(_ amount: Double, suffix: String) -> String {
        if amount <= 0 {
            return "free"
        }
        if amount < 0.0001 {
            return "<$0.0001 \(suffix)"
        }
        return String(format: "~$%.4f %@", amount, suffix)
    }

    var costText: String? {
        guard let costUSD else { return nil }
        return Self.money(costUSD, suffix: "trn")
    }

    /// The session figure, marked as a floor when it does not cover every
    /// answer.
    ///
    /// A total over some of the rows presented as a total over all of them is
    /// the one thing this footer must not do: it is the number an operator would
    /// use to decide whether a chat is getting expensive.
    var sessionCostText: String? {
        guard let sessionCostUSD else { return nil }
        let text = Self.money(sessionCostUSD, suffix: "ses")
        guard let priced = pricedAnswers, let answers = sessionAnswers, priced < answers else {
            return text
        }
        return text + "+"
    }

    var turnsText: String? {
        guard let sessionTurns, sessionTurns > 0 else { return nil }
        return sessionTurns == 1 ? "1 turn" : "\(sessionTurns) turns"
    }

    /// The truth about how the turn ended, when it was not a plain finish.
    var endingText: String? {
        if capped == true { return "capped" }
        if wrappedUp == true { return "wrapped up" }
        return nil
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
        case model
        case provider
        case promptTokens = "prompt_tokens"
        case completionTokens = "completion_tokens"
        case cachedTokens = "cached_tokens"
        case durationMs = "duration_ms"
        case rounds
        case contextTokens = "context_tokens"
        case contextWindow = "context_window"
        case costUSD = "cost_usd"
        case sessionCostUSD = "session_cost_usd"
        case pricedAnswers = "priced_answers"
        case sessionAnswers = "session_answers"
        case sessionTurns = "session_turns"
        case toolCalls = "tool_calls"
        case capped
        case wrappedUp = "wrapped_up"
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
        func text(_ key: CodingKeys) -> String? {
            guard let value = try? container.decodeIfPresent(String.self, forKey: key) else {
                return nil
            }
            let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
            return trimmed.isEmpty ? nil : trimmed
        }
        // Money arrives from a Rust `f64`, which JSON writes as a number. Read
        // as a double so a cost of a few thousandths does not truncate to zero
        // the way an `Int` decode would.
        func money(_ key: CodingKeys) -> Double? {
            if let value = try? container.decodeIfPresent(Double.self, forKey: key) {
                return value.isFinite ? value : nil
            }
            if let value = try? container.decodeIfPresent(Int.self, forKey: key) {
                return Double(value)
            }
            return nil
        }
        func flag(_ key: CodingKeys) -> Bool? {
            try? container.decodeIfPresent(Bool.self, forKey: key)
        }
        self.init(
            model: text(.model),
            provider: text(.provider),
            promptTokens: count(.promptTokens),
            completionTokens: count(.completionTokens),
            cachedTokens: count(.cachedTokens),
            durationMs: count(.durationMs),
            rounds: count(.rounds),
            contextTokens: count(.contextTokens),
            contextWindow: count(.contextWindow),
            costUSD: money(.costUSD),
            sessionCostUSD: money(.sessionCostUSD),
            pricedAnswers: count(.pricedAnswers),
            sessionAnswers: count(.sessionAnswers),
            sessionTurns: count(.sessionTurns),
            toolCalls: count(.toolCalls),
            capped: flag(.capped),
            wrappedUp: flag(.wrappedUp)
        )
    }
}

extension TurnAccounting: Encodable {
    /// Encoded only so `Message`, which is `Codable`, can re-encode a row that
    /// carries accounting into the local cache. The daemon is the writer of
    /// record; the app never posts this object back to it.
    func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encodeIfPresent(model, forKey: .model)
        try container.encodeIfPresent(provider, forKey: .provider)
        try container.encodeIfPresent(promptTokens, forKey: .promptTokens)
        try container.encodeIfPresent(completionTokens, forKey: .completionTokens)
        try container.encodeIfPresent(cachedTokens, forKey: .cachedTokens)
        try container.encodeIfPresent(durationMs, forKey: .durationMs)
        try container.encodeIfPresent(rounds, forKey: .rounds)
        try container.encodeIfPresent(contextTokens, forKey: .contextTokens)
        try container.encodeIfPresent(contextWindow, forKey: .contextWindow)
        try container.encodeIfPresent(costUSD, forKey: .costUSD)
        try container.encodeIfPresent(sessionCostUSD, forKey: .sessionCostUSD)
        try container.encodeIfPresent(pricedAnswers, forKey: .pricedAnswers)
        try container.encodeIfPresent(sessionAnswers, forKey: .sessionAnswers)
        try container.encodeIfPresent(sessionTurns, forKey: .sessionTurns)
        try container.encodeIfPresent(toolCalls, forKey: .toolCalls)
        try container.encodeIfPresent(capped, forKey: .capped)
        try container.encodeIfPresent(wrappedUp, forKey: .wrappedUp)
    }
}

// MARK: - Preferences

/// The switches the settings screen writes and the footer reads.
///
/// Keep the strings here and nowhere else: a literal repeated in two files is
/// a footer that stops following its own toggle. The field list mirrors the
/// `hermes-signature` toggles, one per rendered field.
enum TurnFooterPrefs {
    static let enabled = "kallisti.turnFooter.enabled"
    static let model = "kallisti.turnFooter.model"
    static let provider = "kallisti.turnFooter.provider"
    static let rounds = "kallisti.turnFooter.rounds"
    static let duration = "kallisti.turnFooter.duration"
    static let tokens = "kallisti.turnFooter.tokens"
    static let cache = "kallisti.turnFooter.cache"
    static let context = "kallisti.turnFooter.context"
    static let cost = "kallisti.turnFooter.cost"
    static let sessionCost = "kallisti.turnFooter.sessionCost"
    static let turns = "kallisti.turnFooter.turns"
    static let tools = "kallisti.turnFooter.tools"
}

// MARK: - Footer

/// The one-line receipt under a finished answer.
///
/// It draws nothing at all - no divider, no empty strip - when the master
/// switch is off, when every field is off, or when the row carried no numbers.
/// A turn that was never measured must not leave a gap in the transcript.
///
/// The line is built in the order of the settings list rather than in whatever
/// order the daemon happens to send, so the operator's ordering is the one they
/// see and one field never floats around between turns.
struct TurnFooterBar: View {
    let accounting: TurnAccounting?

    @AppStorage(TurnFooterPrefs.enabled) private var isEnabled: Bool = true
    @AppStorage(TurnFooterPrefs.model) private var showsModel: Bool = true
    @AppStorage(TurnFooterPrefs.provider) private var showsProvider: Bool = true
    @AppStorage(TurnFooterPrefs.rounds) private var showsRounds: Bool = true
    @AppStorage(TurnFooterPrefs.duration) private var showsDuration: Bool = true
    @AppStorage(TurnFooterPrefs.tokens) private var showsTokens: Bool = true
    @AppStorage(TurnFooterPrefs.cache) private var showsCache: Bool = true
    @AppStorage(TurnFooterPrefs.context) private var showsContext: Bool = false
    @AppStorage(TurnFooterPrefs.cost) private var showsCost: Bool = true
    @AppStorage(TurnFooterPrefs.sessionCost) private var showsSessionCost: Bool = true
    @AppStorage(TurnFooterPrefs.turns) private var showsTurns: Bool = true
    @AppStorage(TurnFooterPrefs.tools) private var showsTools: Bool = false

    var body: some View {
        if let line {
            Text(line)
                .font(.system(.caption2, design: .monospaced))
                .foregroundStyle(.secondary)
                .lineLimit(2)
                .minimumScaleFactor(0.75)
                .fixedSize(horizontal: false, vertical: true)
                .accessibilityLabel(line)
        }
    }

    /// The fields that are both switched on and actually known, in reading
    /// order. nil when there are none, which is what keeps the row at zero
    /// height instead of leaving a trimmed blank.
    var line: String? {
        guard isEnabled, let accounting else { return nil }
        var fields: [String] = []
        // The model leads, as it does in the Hermes footer: it is the single
        // most useful fact under an answer when an operator is comparing turns.
        if showsModel, let text = accounting.modelText { fields.append(text) }
        if showsProvider, let text = accounting.providerText { fields.append(text) }
        if showsRounds, let text = accounting.roundsText { fields.append(text) }
        if showsTools, let text = accounting.toolsText { fields.append(text) }
        if showsDuration, let text = accounting.durationText { fields.append(text) }
        if showsTokens, let text = accounting.tokensText { fields.append(text) }
        if showsTokens, let text = accounting.totalTokensText { fields.append(text) }
        if showsCache, let text = accounting.cacheText { fields.append(text) }
        if showsCost, let text = accounting.costText { fields.append(text) }
        if showsSessionCost, let text = accounting.sessionCostText { fields.append(text) }
        if showsTurns, let text = accounting.turnsText { fields.append(text) }
        if showsContext, let text = accounting.contextText { fields.append(text) }
        if let text = accounting.endingText { fields.append(text) }
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
