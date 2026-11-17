import Foundation

@MainActor
@Observable
final class MockSecureStore: SecureStoreProtocol {
    private var store: [String: String] = [:]

    @discardableResult
    func store(key: String, value: String) async -> Bool {
        store[key] = value
        return true
    }

    func retrieve(key: String) async -> String? {
        store[key]
    }

    func delete(key: String) async {
        store.removeValue(forKey: key)
    }
}
