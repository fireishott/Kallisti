import Foundation
import Testing
@testable import Herald

@Suite("ModelStore state management")
struct ModelStoreTests {

    @Test("Initial state is loading with no model or error")
    @MainActor
    func testInitialState() async {
        let store = ModelStore(apiClient: nil, accessTokenProvider: { nil })

        #expect(store.isLoading == false)
        #expect(store.currentModel == nil)
        #expect(store.isError == false)
        #expect(store.errorMessage == nil)
    }

    @Test("loadModels sets error when no API client configured")
    @MainActor
    func testLoadModelsWithoutAPIClient() async {
        let store = ModelStore(apiClient: nil, accessTokenProvider: { nil })

        await store.loadModels()

        #expect(store.isLoading == false)
        #expect(store.isError == true)
        #expect(store.errorMessage != nil)
        #expect(store.currentModel == nil)
    }

    @Test("loadModels sets error when access token unavailable")
    @MainActor
    func testLoadModelsNoToken() async {
        // Create a real client with a dummy URL (won't be reached since token is nil)
        let client = RelayAPIClient(baseURLProvider: { "http://localhost" })
        let store = ModelStore(apiClient: client, accessTokenProvider: { nil })

        await store.loadModels()

        #expect(store.isLoading == false)
        #expect(store.isError == true)
        #expect(store.errorMessage == "Not connected to a relay.")
    }

    @Test("reset clears all state")
    @MainActor
    func testReset() async {
        let store = ModelStore(apiClient: nil, accessTokenProvider: { nil })

        // Trigger an error state
        await store.loadModels()
        #expect(store.isError == true)

        store.reset()

        #expect(store.isLoading == false)
        #expect(store.currentModel == nil)
        #expect(store.isError == false)
        #expect(store.errorMessage == nil)
    }
}
