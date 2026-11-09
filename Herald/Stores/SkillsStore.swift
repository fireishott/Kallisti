import Foundation

/// Loads the skill catalog from the connected host.
///
/// Listing comes from `GET /skills` on the relay and `GET /v1/skills` on the Eris
/// daemon.
///
/// Reading is universal; writing is not. The relay and DSH transports are
/// read-only by design, so their affordances are hidden rather than failing. The
/// Eris control plane is the one transport with a write behind it, and it carries
/// both a body rewrite (`PUT /v1/skills/{key}`) and an archive move
/// (`DELETE /v1/skills/{key}`).
@MainActor
@Observable
final class SkillsStore {
    /// One row of the catalog.
    ///
    /// The first three fields are what every transport has always sent. The last
    /// four are the editing lane's and arrive only from a daemon that carries
    /// them; each decodes as "the daemon did not say" rather than as a value this
    /// app invented, so an older daemon still lists.
    struct HeraldSkill: Decodable, Identifiable, Hashable {
        /// The directory name the daemon addresses the skill by. This is the
        /// identity on the wire, and the only thing a write or an archive may
        /// name.
        ///
        /// It is not the display name. A learned skill can be filed under
        /// `pre-theater-dinner-planning` while its frontmatter says
        /// `Pre-Theater Dinner Planning`, and the daemon refuses a key carrying a
        /// space, so addressing by name makes exactly those skills uneditable.
        let key: String
        let name: String
        let description: String
        let path: String
        /// `operator` or `learned`; nil when the transport did not say.
        let origin: String?
        /// `active`, `stale` or `archived`; nil when the transport did not say.
        let state: String?
        let pinned: Bool
        /// True only when the daemon said this body may be rewritten from here.
        let editable: Bool

        var id: String { key }

        init(
            key: String,
            name: String,
            description: String,
            path: String,
            origin: String? = nil,
            state: String? = nil,
            pinned: Bool = false,
            editable: Bool = false
        ) {
            self.key = key
            self.name = name
            self.description = description
            self.path = path
            self.origin = origin
            self.state = state
            self.pinned = pinned
            self.editable = editable
        }

        private enum CodingKeys: String, CodingKey {
            case key, name, description, path, origin, state, pinned, editable
        }

        init(from decoder: Decoder) throws {
            let box = try decoder.container(keyedBy: CodingKeys.self)
            let decodedName = try box.decode(String.self, forKey: .name)
            name = decodedName
            // A transport that sends no key addresses its rows by name, which is
            // the only identity those transports have ever had. Eris always sends
            // one, and it is what save and delete use.
            key = try box.decodeIfPresent(String.self, forKey: .key) ?? decodedName
            description = try box.decodeIfPresent(String.self, forKey: .description) ?? ""
            path = try box.decodeIfPresent(String.self, forKey: .path) ?? ""
            origin = try box.decodeIfPresent(String.self, forKey: .origin)
            state = try box.decodeIfPresent(String.self, forKey: .state)
            pinned = try box.decodeIfPresent(Bool.self, forKey: .pinned) ?? false
            editable = try box.decodeIfPresent(Bool.self, forKey: .editable) ?? false
        }

        /// Where this skill came from, as a word worth drawing. Nil when the
        /// daemon did not say, so no badge claims a provenance it was not given.
        var originLabel: String? {
            switch origin {
            case "learned": return "Learned"
            case "operator": return "Operator"
            default: return nil
            }
        }

        /// The curator's word for this row, when it is not simply active.
        var stateLabel: String? {
            switch state {
            case "stale": return "Stale"
            case "archived": return "Archived"
            default: return nil
            }
        }
    }

    private struct SkillCatalogResponse: Decodable {
        let skills: [HeraldSkill]
    }

    /// Build 135.41: full skill detail (SKILL.md content) from the
    /// connector's /v1/skills/{name} endpoint.
    struct SkillDetail: Decodable, Sendable {
        let name: String
        let path: String?
        let description: String?
        let content: String?
    }

    private struct SkillDetailEnvelope: Decodable {
        let data: SkillDetail
    }

    private(set) var skills: [HeraldSkill] = []
    private(set) var isLoading = false
    private(set) var errorMessage: String?
    var searchText = ""

    private var lastLoadedAt: Date?

    private static let refreshInterval: TimeInterval = 120

    private let apiClient: RelayAPIClient?
    private let accessTokenProvider: () async -> String?
    private let nativeFeatureClientProvider: () -> NativeGatewayFeatureClient?
    /// DSH skill transport. Non-nil when the app runs on the DSH path, which has
    /// no relay and no native gateway — without this the browser falls through
    /// to "Not connected to a relay." and the Skills screen renders empty.
    var dshSkillsProvider: (() async throws -> [HeraldSkill])?
    var dshSkillDetailProvider: ((String) async throws -> SkillDetail)?

    /// Eris control-plane transport: the library and one skill's body.
    var erisSkillsProvider: (@MainActor () async throws -> [HeraldSkill])?
    var erisSkillDetailProvider: (@MainActor (String) async throws -> SkillDetail)?

    /// Eris control-plane writes: a body rewrite and an archive move.
    ///
    /// Both are nil on every other transport, and that nil is what hides the
    /// affordances: a relay or DSH connection shows no editor and no swipe action
    /// at all, rather than offering one that could only fail.
    var erisSkillSave: (@MainActor (_ key: String, _ content: String) async throws -> Void)?
    var erisSkillDelete: (@MainActor (_ key: String) async throws -> Void)?

    /// Whether this transport can rewrite a skill's body.
    ///
    /// A row's `editable` narrows this further: the daemon and the transport must
    /// both allow it before an editor is drawn.
    var canEditSkills: Bool { erisSkillSave != nil }

    /// Whether this transport can archive a skill.
    var canDeleteSkills: Bool { erisSkillDelete != nil }

    /// Raised when a write is attempted on a transport that can only read.
    ///
    /// The views hide the affordances on those transports, so reaching this means
    /// a caller wired a write where no transport exists to carry it.
    enum SkillsStoreError: Error, LocalizedError {
        case readOnlyTransport

        var errorDescription: String? {
            switch self {
            case .readOnlyTransport:
                return "This connection can only read skills. Editing needs the Eris daemon."
            }
        }
    }

    init(apiClient: RelayAPIClient?, accessTokenProvider: @escaping () async -> String?, nativeFeatureClientProvider: @escaping () -> NativeGatewayFeatureClient? = { nil }) {
        self.apiClient = apiClient
        self.accessTokenProvider = accessTokenProvider
        self.nativeFeatureClientProvider = nativeFeatureClientProvider
    }

    /// Skills filtered by the current search text, matching on name or description.
    var filteredSkills: [HeraldSkill] {
        if searchText.isEmpty { return skills }
        return skills.filter {
            $0.name.localizedCaseInsensitiveContains(searchText) ||
            $0.description.localizedCaseInsensitiveContains(searchText)
        }
    }

    func loadSkills(force: Bool = false) async {
        if !force,
           let lastLoadedAt,
           Date().timeIntervalSince(lastLoadedAt) < Self.refreshInterval,
           !skills.isEmpty {
            return
        }
        isLoading = true
        errorMessage = nil
        defer { isLoading = false }

        do {
            // DSH path first: the relay and native gateway are both absent, so
            // without this the screen reports "Not connected to a relay."
            if let erisSkillsProvider {
                skills = try await erisSkillsProvider()
            } else if let dshSkillsProvider {
                skills = try await dshSkillsProvider()
            } else if let apiClient, let token = await accessTokenProvider() {
                let response: SkillCatalogResponse = try await apiClient.get(path: "skills", accessToken: token)
                skills = response.skills
            } else if let nativeFeatureClient = nativeFeatureClientProvider() {
                skills = try await nativeFeatureClient.managedSkills().map { HeraldSkill(key: $0.name, name: $0.name, description: $0.description, path: $0.path) }
            } else {
                errorMessage = "Not connected to a relay."
                return
            }
            lastLoadedAt = .now
        } catch {
            // Build 135.40: a failed refresh must NOT clear the loaded list.
            // The old code left `skills` as-is but the view had no error UI,
            // so a transient RPC failure during back-nav rendered the bare
            // yellow-triangle empty state (and a successful list vanished).
            // Surface the error for the view; the stale list stays visible.
            if skills.isEmpty {
                errorMessage = error.localizedDescription
            }
        }
    }

    /// Build 135.41: fetch one skill's SKILL.md content via the REST
    /// /v1/skills/{key} endpoint (falls back to native gateway if REST is
    /// unavailable). Returns the parsed detail or throws.
    ///
    /// Addressed by key, never by display name: the daemon refuses a key with a
    /// space in it, and a skill whose frontmatter name is prose would otherwise
    /// be unopenable as well as uneditable.
    func loadDetail(key: String) async throws -> SkillDetail {
        if let erisSkillDetailProvider {
            return try await erisSkillDetailProvider(key)
        }
        if let dshSkillDetailProvider {
            return try await dshSkillDetailProvider(key)
        }
        if let apiClient, let token = await accessTokenProvider() {
            do {
                let envelope: SkillDetailEnvelope = try await apiClient.get(path: "skills/\(key)", accessToken: token)
                return envelope.data
            } catch {
                // Fall through to native gateway on REST failure
            }
        }
        if let nativeFeatureClient = nativeFeatureClientProvider() {
            // Native gateway has no per-skill content endpoint; return a
            // minimal detail from the list entry so the view still renders.
            if let skill = skills.first(where: { $0.key == key }) {
                return SkillDetail(name: skill.name, path: skill.path, description: skill.description, content: nil)
            }
        }
        throw URLError(.badURL)
    }

    /// Rewrite one skill's `SKILL.md` through the Eris control plane.
    ///
    /// The body sent is the whole file the editor holds. A refusal throws the
    /// daemon's own answer (`ErisError`) and leaves the row untouched; the caller
    /// re-reads the body afterwards, because the daemon is the one that knows
    /// what it stored.
    func saveSkill(key: String, content: String) async throws {
        guard let erisSkillSave else { throw SkillsStoreError.readOnlyTransport }
        try await erisSkillSave(key, content)
    }

    /// Archive one skill, then drop the row once the daemon confirmed.
    ///
    /// The row leaves the list only on a 200, so a refused archive leaves the
    /// list exactly as it was and the view still has a sentence to show.
    func deleteSkill(key: String) async throws {
        guard let erisSkillDelete else { throw SkillsStoreError.readOnlyTransport }
        try await erisSkillDelete(key)
        skills.removeAll { $0.key == key }
        lastLoadedAt = .now
    }

    /// The sentence to show when a skill write is refused.
    ///
    /// The daemon answers `{ok:false, code, message}` and its `message` is already
    /// the sentence to read, so it is preferred to any paraphrase here. The one
    /// case worth spelling out is a 403 on a token without `admin`: `scope_denied`
    /// on its own tells the operator nothing about the fix.
    static func refusal(_ error: Error) -> String {
        guard let eris = error as? ErisError else { return error.localizedDescription }
        if eris.status == 403 {
            return eris.message
                ?? "This device is paired without the admin scope, so it cannot change a skill."
        }
        if let message = eris.message, !message.isEmpty { return message }
        return eris.localizedDescription
    }

    func reset() {
        skills = []
        errorMessage = nil
        lastLoadedAt = nil
        searchText = ""
    }
}
