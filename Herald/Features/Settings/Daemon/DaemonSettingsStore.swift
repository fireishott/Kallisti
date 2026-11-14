import Foundation
import Observation

/// The daemon settings store: one read, a dirty set, one commit.
///
/// The daemon publishes the record and validates every write, so this store owns
/// no defaults and no field list. It owns three things only: the loaded record, the
/// set of fields the operator has changed, and the refusal for whichever field the
/// daemon refused.
///
/// Optimistic apply, roll back on failure. A bool writes immediately; a string or
/// a number waits for the save button or a focus loss, because that is what iOS
/// does and because a text field that posts on every keystroke is a request per
/// character.
@MainActor
@Observable
final class DaemonSettingsStore {
    /// Every field the daemon published, in its own group order.
    private(set) var settings: [DaemonSetting] = []
    /// The keys changed locally and not yet accepted by the daemon.
    private(set) var dirty: Set<String> = []
    /// The sentence under one refused field.
    private(set) var refusal: [String: String] = [:]
    /// Whether the last load is still running, for the skeleton.
    private(set) var isLoading = false
    /// The read failed for a reason the screen states.
    private(set) var loadError: String?
    /// The read was refused for a missing `admin` scope.
    private(set) var needsAdmin = false
    /// Set briefly after a commit lands, for the checkmark.
    private(set) var justSaved = false

    private let baseURLProvider: @MainActor () -> String?
    private let tokenProvider: @MainActor () async -> String?

    init(
        baseURLProvider: @escaping @MainActor () -> String?,
        tokenProvider: @escaping @MainActor () async -> String?
    ) {
        self.baseURLProvider = baseURLProvider
        self.tokenProvider = tokenProvider
    }

    // MARK: - Reading

    /// Fields of one group, in record order.
    func fields(in group: DaemonGroup) -> [DaemonSetting] {
        settings.filter { $0.group == group.rawValue }
    }

    /// How many settings a group holds, for the entry card.
    func count(in group: DaemonGroup) -> Int {
        fields(in: group).count
    }

    /// Load the record. Never blanks a loaded screen on a failed refresh.
    func load() async {
        isLoading = settings.isEmpty
        loadError = nil
        needsAdmin = false
        defer { isLoading = false }
        guard let url = endpoint() else {
            loadError = "No daemon is configured on this phone yet."
            return
        }
        guard let token = await tokenProvider() else {
            loadError = "This phone is not signed in to the daemon."
            return
        }
        var request = URLRequest(url: url)
        request.httpMethod = "GET"
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        do {
            let (data, response) = try await URLSession.shared.data(for: request)
            guard let http = response as? HTTPURLResponse else {
                loadError = "The daemon did not answer."
                return
            }
            if http.statusCode == 403 {
                needsAdmin = true
                return
            }
            guard http.statusCode == 200 else {
                loadError = "The daemon answered \(http.statusCode)."
                return
            }
            let record = try JSONDecoder().decode(DaemonCapabilityRecord.self, from: data)
            settings = record.capabilities.sorted {
                let left = DaemonGroup.order(of: $0.group)
                let right = DaemonGroup.order(of: $1.group)
                return left == right ? $0.key < $1.key : left < right
            }
            // A refresh replaces values, but a field the operator is mid-way
            // through should not be yanked back: dirty keys keep their local value.
            dirty = dirty.intersection(settings.map(\.key))
        } catch {
            loadError = "The record could not be read: \(error.localizedDescription)"
        }
    }

    // MARK: - Changing

    /// Change one value locally. Returns false when the field is not writable.
    @discardableResult
    func set(_ key: String, to value: String) -> Bool {
        guard let index = settings.firstIndex(where: { $0.key == key }), settings[index].writable
        else { return false }
        let old = settings[index]
        settings[index] = DaemonSetting(
            key: old.key,
            label: old.label,
            group: old.group,
            rawType: old.rawType,
            value: value,
            writable: old.writable,
            options: old.options,
            minimum: old.minimum,
            maximum: old.maximum
        )
        dirty.insert(key)
        refusal[key] = nil
        return true
    }

    /// A bool writes at once, because a switch that waits for a save button is a
    /// switch iOS users press twice.
    func setBool(_ key: String, to on: Bool) async {
        guard set(key, to: on ? "true" : "false") else { return }
        await commit()
    }

    // MARK: - Committing

    /// Send every dirty field in one write.
    ///
    /// On a refusal the whole set rolls back to what the daemon still holds, the
    /// sentence lands under the refused row, and the rest of the dirty fields are
    /// re-marked so a retry after the fix still carries them.
    func commit() async {
        guard !dirty.isEmpty else { return }
        guard let url = endpoint() else { return }
        guard let token = await tokenProvider() else { return }
        let sending = dirty
        let before = settings.filter { sending.contains($0.key) }

        let body = sending.compactMap { key in
            settings.first(where: { $0.key == key }).map { "\(escape(key))=\(escape($0.value))" }
        }
        .joined(separator: "&")

        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        request.setValue("application/x-www-form-urlencoded", forHTTPHeaderField: "Content-Type")
        request.httpBody = Data(body.utf8)
        do {
            let (data, response) = try await URLSession.shared.data(for: request)
            guard let http = response as? HTTPURLResponse else { return }
            if http.statusCode == 200 {
                let changed = (try? JSONDecoder().decode([String].self, from: .init())) ?? []
                _ = changed
                dirty.subtract(sending)
                for key in sending { refusal[key] = nil }
                justSaved = true
                try? await Task.sleep(for: .seconds(2))
                justSaved = false
                await load()
                return
            }
            if let parsed = try? JSONDecoder().decode(DaemonRefusal.self, from: data) {
                let failed = parsed.field ?? sending.first ?? ""
                if let original = before.first(where: { $0.key == failed }) {
                    restore(original)
                }
                if !failed.isEmpty {
                    refusal[failed] = parsed.message ?? "The daemon refused that value."
                }
                dirty.subtract([failed])
                return
            }
            restoreAll(before)
            dirty = []
        } catch {
            restoreAll(before)
            if let first = before.first {
                refusal[first.key] = "The write did not reach the daemon."
            }
            dirty = []
        }
    }

    /// Throw away the local changes and read the daemon's values again.
    func revert() async {
        for key in dirty {
            if let current = settings.first(where: { $0.key == key }) {
                _ = current
            }
        }
        dirty = []
        refusal = [:]
        await load()
    }

    private func restore(_ setting: DaemonSetting) {
        guard let index = settings.firstIndex(where: { $0.key == setting.key }) else { return }
        settings[index] = setting
    }

    private func restoreAll(_ list: [DaemonSetting]) {
        for setting in list { restore(setting) }
    }

    // MARK: - Transport

    private func endpoint() -> URL? {
        guard let base = baseURLProvider()?.trimmingCharacters(in: CharacterSet(charactersIn: "/")),
              !base.isEmpty
        else { return nil }
        return URL(string: "\(base)/v1/settings")
    }

    /// Percent-encode a form value, so a value with a space or an ampersand survives.
    private func escape(_ value: String) -> String {
        var allowed = CharacterSet.alphanumerics
        allowed.insert(charactersIn: "-._~")
        return value.addingPercentEncoding(withAllowedCharacters: allowed) ?? value
    }
}
