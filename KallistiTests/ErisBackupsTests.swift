import Foundation
import Testing
@testable import Kallisti

// MARK: - What these hold
//
// Backups are the one part of Settings where a wrong action loses data, so the
// wire types have to carry enough for a person to decide before they tap. Three
// things matter and each has a case below:
//
//  1. The list says what each archive is. A file that does not open as an
//     archive has no member count, and that absence is shown as "unreadable"
//     rather than as zero members, because zero is a real answer and this is not
//     one.
//  2. A create reports what it wrote, so a confirmation names the file.
//  3. A restore reports whether the archive verified, and `false` is not a
//     success. The daemon also cannot take effect while it holds the database
//     open, which is why the screen says the next start rather than "restored".

@Suite("Eris backups")
struct ErisBackupsTests {

    /// The list, as the daemon writes it: `created_at` is the file's own stamp and
    /// `members` is null for a file that does not open as an archive.
    @Test("the archive list decodes, including the unreadable one")
    func pageDecodes() throws {
        let body = #"""
        {"ok":true,"count":2,"archives":[
          {"name":"kallisti-20261027-130000.bk","created_at":"2026-10-27T13:00:00Z","bytes":4194304,"members":42},
          {"name":"truncated.bk","created_at":"2026-10-26T09:12:00Z","bytes":12,"members":null}
        ]}
        """#
        let page = try JSONDecoder().decode(ErisBackupPage.self, from: Data(body.utf8))
        #expect(page.archives.count == 2)
        #expect(page.archives[0].name == "kallisti-20261027-130000.bk")
        #expect(page.archives[0].members == 42)
        #expect(page.archives[0].membersText == "42 members")
        #expect(page.archives[0].date != nil)
        #expect(!page.archives[0].sizeText.isEmpty)

        // A file the daemon could not open as an archive says so, rather than
        // claiming no members.
        #expect(page.archives[1].members == nil)
        #expect(page.archives[1].membersText == "unreadable")
    }

    /// A create names the file it wrote, so the confirmation is checkable against
    /// the list rather than a generic "done".
    @Test("a created archive reports its name, size and members")
    func createdDecodes() throws {
        let body = #"{"ok":true,"name":"kallisti-20261027-140000.bk","members":42,"bytes":4194304}"#
        let created = try JSONDecoder().decode(ErisBackupCreated.self, from: Data(body.utf8))
        #expect(created.name == "kallisti-20261027-140000.bk")
        #expect(created.members == 42)
        #expect(created.summary.contains("kallisti-20261027-140000.bk"))
        #expect(created.summary.contains("42 members"))
    }

    /// `restored: true` is the daemon stating that every member verified and is in
    /// place. Anything else must not read as success.
    @Test("a restore reports whether it landed")
    func restoredDecodes() throws {
        let ok = #"{"ok":true,"name":"kallisti-20261027-140000.bk","restored":true}"#
        let landed = try JSONDecoder().decode(ErisBackupRestored.self, from: Data(ok.utf8))
        #expect(landed.restored)
        #expect(landed.name == "kallisti-20261027-140000.bk")

        let no = #"{"ok":true,"name":"kallisti-20261027-140000.bk","restored":false}"#
        #expect(try JSONDecoder().decode(ErisBackupRestored.self, from: Data(no.utf8)).restored == false)
    }

    /// The restore path carries a filename, which contains dots and dashes and
    /// could contain anything. It has to survive the URL as one segment.
    @Test("the routes point at the paths the daemon binds")
    func routesAreTheDaemons() {
        #expect(ErisRoute.backups.path == "/v1/backups")
        #expect(ErisRoute.backups.method == "GET")
        #expect(ErisRoute.createBackup.path == "/v1/backups")
        #expect(ErisRoute.createBackup.method == "POST")
        #expect(
            ErisRoute.restoreBackup(name: "kallisti-20261027-140000.bk").path
                == "/v1/backups/kallisti-20261027-140000.bk/restore"
        )
        #expect(ErisRoute.restoreBackup(name: "a b.bk").method == "POST")
        // A name with a space cannot break out of its own path segment.
        #expect(ErisRoute.restoreBackup(name: "a b.bk").path == "/v1/backups/a%20b.bk/restore")
        #expect(ErisRoute.restoreBackup(name: "../etc/passwd").path.contains("%2F") || !ErisRoute.restoreBackup(name: "../etc/passwd").path.contains("/etc/"))
    }
}
