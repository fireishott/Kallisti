import XCTest

final class HeraldMobileUITests: XCTestCase {
    private struct UITestLaunchContext {
        private struct ExternalConfiguration: Decodable {
            let setupCode: String?
            let pairingMode: String?
        }

        private static let configurationPath = "/tmp/herald-uitest-config.json"

        let defaultsSuite = "uitest.defaults.\(UUID().uuidString)"
        let keychainService = "uitest.keychain.\(UUID().uuidString)"
        let setupCode: String
        let pairingMode: String

        init(
            setupCodeOverride: String? = ProcessInfo.processInfo.environment["UITEST_SETUP_CODE"],
            pairingMode: String = ProcessInfo.processInfo.environment["UITEST_PAIRING_MODE"] ?? "mock"
        ) {
            let externalConfiguration = Self.loadExternalConfiguration()
            self.pairingMode = externalConfiguration?.pairingMode ?? pairingMode

            let resolvedSetupCode = setupCodeOverride ?? externalConfiguration?.setupCode
            if let resolvedSetupCode, !resolvedSetupCode.isEmpty {
                self.setupCode = resolvedSetupCode
                return
            }

            self.setupCode = "ABCD-EFGH"
        }

        private static func loadExternalConfiguration() -> ExternalConfiguration? {
            guard let data = try? Data(contentsOf: URL(fileURLWithPath: configurationPath)) else {
                return nil
            }

            return try? JSONDecoder().decode(ExternalConfiguration.self, from: data)
        }
    }

    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    @MainActor
    func testManualPairingFlowShowsMainChatSurface() throws {
        let context = UITestLaunchContext()
        let app = makeApp(context: context)
        app.launch()

        XCTAssertTrue(app.buttons["Begin"].waitForExistence(timeout: 5))
        completePairing(in: app, setupCode: context.setupCode)

        XCTAssertTrue(composerInput(in: app).waitForExistence(timeout: 5))
        XCTAssertTrue(app.buttons["Start voice mode"].exists)
        XCTAssertTrue(app.buttons["Open settings"].exists)
    }

    @MainActor
    func testChatSendFlow() throws {
        let context = UITestLaunchContext()
        let app = makeApp(context: context)
        let message = "UI live chat smoke test"
        let chatResponseTimeout: TimeInterval = context.pairingMode == "mock" ? 20 : 60

        app.launch()
        completePairing(in: app, setupCode: context.setupCode)

        let input = composerInput(in: app)
        XCTAssertTrue(input.waitForExistence(timeout: 5))

        input.tap()
        input.typeText(message)
        app.buttons["Send message"].tap()

        XCTAssertTrue(app.staticTexts[message].waitForExistence(timeout: chatResponseTimeout))
    }

    @MainActor
    func testPairedLaunchSkipsOnboarding() throws {
        let context = UITestLaunchContext()
        let app = makeApp(context: context)
        app.launch()
        completePairing(in: app, setupCode: context.setupCode)
        XCTAssertTrue(app.buttons["Open settings"].waitForExistence(timeout: 5))

        app.terminate()

        let relaunchedApp = makeApp(context: context)
        relaunchedApp.launch()

        XCTAssertFalse(relaunchedApp.buttons["Begin"].waitForExistence(timeout: 2))
        XCTAssertTrue(composerInput(in: relaunchedApp).waitForExistence(timeout: 5))
    }

    @MainActor
    func testDisconnectReturnsToOnboarding() throws {
        let context = UITestLaunchContext()
        let app = makeApp(context: context)
        app.launch()
        completePairing(in: app, setupCode: context.setupCode)

        app.buttons["Open settings"].tap()
        let manageButton = app.buttons["settings.heraldHost"]
        XCTAssertTrue(manageButton.waitForExistence(timeout: 5))
        manageButton.tap()

        let disconnectButton = app.buttons["Disconnect"]
        XCTAssertTrue(disconnectButton.waitForExistence(timeout: 5))
        disconnectButton.tap()

        XCTAssertTrue(app.buttons["Begin"].waitForExistence(timeout: 5))
    }

    @MainActor
    func testSettingsCanShowHostStatusScreen() throws {
        let context = UITestLaunchContext()
        let app = makeApp(context: context)
        app.launch()
        completePairing(in: app, setupCode: context.setupCode)

        app.buttons["Open settings"].tap()
        let manageHostButton = app.buttons["settings.heraldHost"]
        XCTAssertTrue(manageHostButton.waitForExistence(timeout: 5))
        manageHostButton.tap()

        XCTAssertTrue(app.navigationBars["Connect Host"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.buttons["Disconnect"].waitForExistence(timeout: 5))
    }

    @MainActor
    func testLaunchPerformance() throws {
        let context = UITestLaunchContext()
        measure(metrics: [XCTApplicationLaunchMetric()]) {
            let app = makeApp(context: context)
            app.launch()
        }
    }

    @MainActor
    private func makeApp(context: UITestLaunchContext) -> XCUIApplication {
        let app = XCUIApplication()
        app.launchEnvironment["UITEST_DEFAULTS_SUITE"] = context.defaultsSuite
        app.launchEnvironment["UITEST_KEYCHAIN_SERVICE"] = context.keychainService
        app.launchEnvironment["UITEST_PAIRING_MODE"] = context.pairingMode
        return app
    }

    @MainActor
    private func completePairing(in app: XCUIApplication, setupCode: String) {
        // Welcome → Relay
        let beginButton = app.buttons["Begin"]
        if beginButton.waitForExistence(timeout: 5) {
            beginButton.tap()
        }

        // Relay → Pairing (default relay URL is pre-filled in debug builds)
        let continueToPairing = app.buttons["Continue"]
        XCTAssertTrue(continueToPairing.waitForExistence(timeout: 5))
        continueToPairing.tap()

        // Pairing
        let setupCodeField = app.textFields["Setup code"]
        XCTAssertTrue(setupCodeField.waitForExistence(timeout: 5))
        setupCodeField.tap()
        setupCodeField.typeText(setupCode)
        app.buttons["Connect Herald"].tap()

        // Permissions → Ready (tap Continue, then Open app)
        let continueFromPermissions = app.buttons["Continue"]
        if continueFromPermissions.waitForExistence(timeout: 5) {
            continueFromPermissions.tap()
        }

        let openApp = app.buttons["Open app"]
        if openApp.waitForExistence(timeout: 5) {
            openApp.tap()
        }

        XCTAssertTrue(app.buttons["Open settings"].waitForExistence(timeout: 8))
    }

    @MainActor
    private func composerInput(in app: XCUIApplication) -> XCUIElement {
        let identifiedField = app.textFields["chat.composer"]
        if identifiedField.exists {
            return identifiedField
        }
        let textField = app.textFields["Reply to Herald"]
        if textField.exists {
            return textField
        }
        return app.textViews["Reply to Herald"]
    }
}
