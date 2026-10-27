import Foundation

// MARK: - Backups
//
// Shapes mirror `crates/eris-host/src/device/handlers.rs` (`backups`,
// `create_backup`, `restore_backup`). All three need the `admin` scope, and all
// three are the same code the browser's buttons reach, so the phone refuses what
// the browser refuses for the same stated reason.
//
// `created_at` is the archive file's own stamp, not an RFC 3339 field the daemon
// formats, so it stays a `String` here and is parsed only for display. A stamp
// that does not parse is not a reason to hide a backup: the file is still there.

/// One archive on the daemon.
struct ErisBackup: Identifiable, Equatable, Sendable {
    /// The bare filename, which is also the name a restore names.
    let name: String
    /// When the file was written, as the daemon stamps it.
    let createdAt: String
    /// The archive's size in bytes.
    let bytes: Int
    /// How many members the manifest lists, when the file opens as an archive.
    ///
    /// `nil` means the file is not a readable archive: the daemon says so by
    /// leaving the count out rather than by guessing a zero.
    let members: Int?

    var id: String { name }

    /// The size, in the unit a person reads.
    var sizeText: String {
        ByteCountFormatter.string(fromByteCount: Int64(bytes), countStyle: .file)
    }

    /// The member count as a phrase, or the honest unknown.
    var membersText: String {
        guard let members else { return "unreadable" }
        return members == 1 ? "1 member" : "\(members) members"
    }

    /// The stamp as a date, when it parses.
    var date: Date? { ErisTimestamp.date(from: createdAt) }

    init(name: String, createdAt: String, bytes: Int, members: Int? = nil) {
        self.name = name
        self.createdAt = createdAt
        self.bytes = bytes
        self.members = members
    }
}

extension ErisBackup: Decodable {
    private enum CodingKeys: String, CodingKey {
        case name
        case createdAt = "created_at"
        case bytes
        case members
    }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let name = try container.decodeIfPresent(String.self, forKey: .name) ?? ""
        guard !name.isEmpty else {
            throw ErisError.decoding("a backup in the list carried no name")
        }
        self.init(
            name: name,
            createdAt: try container.decodeIfPresent(String.self, forKey: .createdAt) ?? "",
            bytes: try container.decodeIfPresent(Int.self, forKey: .bytes) ?? 0,
            members: try container.decodeIfPresent(Int.self, forKey: .members)
        )
    }
}

/// The `GET /v1/backups` body.
struct ErisBackupPage: Equatable, Sendable, Decodable {
    /// The archives, as the daemon ordered them.
    let archives: [ErisBackup]

    init(archives: [ErisBackup]) {
        self.archives = archives
    }

    private enum CodingKeys: String, CodingKey {
        case archives
    }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.init(
            archives: try container.decodeIfPresent([ErisBackup].self, forKey: .archives) ?? []
        )
    }
}

/// The `201` body from `POST /v1/backups`: the archive that was written.
struct ErisBackupCreated: Equatable, Sendable, Decodable {
    /// The filename, which is what a restore names.
    let name: String
    /// How many members the archive holds.
    let members: Int
    /// The archive's size in bytes.
    let bytes: Int

    init(name: String, members: Int, bytes: Int) {
        self.name = name
        self.members = members
        self.bytes = bytes
    }

    private enum CodingKeys: String, CodingKey {
        case name, members, bytes
    }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let name = try container.decodeIfPresent(String.self, forKey: .name) ?? ""
        guard !name.isEmpty else {
            throw ErisError.decoding("the created backup carried no name")
        }
        self.init(
            name: name,
            members: try container.decodeIfPresent(Int.self, forKey: .members) ?? 0,
            bytes: try container.decodeIfPresent(Int.self, forKey: .bytes) ?? 0
        )
    }

    /// One line for a confirmation.
    var summary: String {
        let size = ByteCountFormatter.string(fromByteCount: Int64(bytes), countStyle: .file)
        return "\(name) written (\(size), \(members) members)"
    }
}

/// The `POST /v1/backups/{name}/restore` body.
///
/// `restored` is the daemon stating that every member verified and is in place.
/// It has not taken effect yet: the daemon holds the live database open, so the
/// restored state is what the next start reads. A surface that says "restored"
/// and stops there is telling half of it.
struct ErisBackupRestored: Equatable, Sendable, Decodable {
    /// The archive that was restored.
    let name: String
    /// Whether the archive verified and its members are in place.
    let restored: Bool

    init(name: String, restored: Bool) {
        self.name = name
        self.restored = restored
    }

    private enum CodingKeys: String, CodingKey {
        case name, restored
    }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.init(
            name: try container.decodeIfPresent(String.self, forKey: .name) ?? "",
            restored: try container.decodeIfPresent(Bool.self, forKey: .restored) ?? false
        )
    }
}
