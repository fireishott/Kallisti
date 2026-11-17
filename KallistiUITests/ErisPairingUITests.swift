import XCTest

final class ErisPairingUITests: XCTestCase {
    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    @MainActor
    func testPairsWithErisLive() throws {
        let suite = "eris.qa.\(UUID().uuidString)"
        let keychain = "eris.qa.\(UUID().uuidString)"
        let app = XCUIApplication()
        app.launchEnvironment["UITEST_DEFAULTS_SUITE"] = suite
        app.launchEnvironment["UITEST_KEYCHAIN_SERVICE"] = keychain

        addUIInterruptionMonitor(withDescription: "system permission") { alert in
            for label in ["Allow", "OK", "Continue"] where alert.buttons[label].exists {
                alert.buttons[label].tap()
                return true
            }
            return false
        }

        app.launch()

        let begin = app.buttons["Begin"]
        XCTAssertTrue(begin.waitForExistence(timeout: 15), "welcome did not render")
        begin.tap()

        // Self-hosted relay, then the Eris daemon address.
        let selfHosted = app.buttons.matching(NSPredicate(format: "label CONTAINS[c] 'self-hosted'")).firstMatch
        if selfHosted.waitForExistence(timeout: 5) { selfHosted.tap() }
        let field = app.textFields.firstMatch
        XCTAssertTrue(field.waitForExistence(timeout: 10), "relay field missing")
        field.tap()
        field.typeText("https://fih-ai-host.tail4d497d.ts.net")

        let cont = app.buttons["Continue"]
        XCTAssertTrue(cont.waitForExistence(timeout: 5), "continue missing")
        cont.tap()

        let codeField = app.textFields["Setup code"]
        XCTAssertTrue(codeField.waitForExistence(timeout: 15), "pairing code field missing")
        codeField.tap()
        codeField.typeText("7AUG585W")
        let connect = app.buttons["Connect Kallisti"]
        XCTAssertTrue(connect.waitForExistence(timeout: 5), "connect button missing")
        connect.tap()

        let failed = app.staticTexts.matching(NSPredicate(format: "label CONTAINS[c] 'Pairing failed'")).firstMatch
        let open = app.buttons.matching(NSPredicate(format: "label CONTAINS[c] 'open kallisti' OR label CONTAINS[c] 'open app'")).firstMatch
        let continueBtn = app.buttons["Continue"]
        let deadline = Date().addingTimeInterval(40)
        while Date() < deadline {
            if failed.exists {
                XCTFail("pairing failed: \(failed.label)")
                return
            }
            if open.exists { break }
            if continueBtn.exists { continueBtn.tap() }
            usleep(400_000)
        }
        XCTAssertTrue(open.exists, "the paired screen never appeared")
        open.tap()
        sleep(2)
        app.tap()
        let afterOpen = XCTAttachment(screenshot: app.screenshot())
        afterOpen.name = "after-open"
        afterOpen.lifetime = .keepAlways
        add(afterOpen)

        let composer = app.descendants(matching: .any)["Send message"]
        let settings = app.buttons["Settings"]
        let chatTab = app.buttons["Chat"]
        let surfaceDeadline = Date().addingTimeInterval(20)
        while Date() < surfaceDeadline {
            if composer.exists || settings.exists || chatTab.exists { break }
            usleep(400_000)
        }
        XCTAssertFalse(failed.exists, "the legacy gateway error was shown")
        XCTAssertTrue(composer.exists || settings.exists || chatTab.exists,
                      "pairing did not reach the app surface")
    }
}
