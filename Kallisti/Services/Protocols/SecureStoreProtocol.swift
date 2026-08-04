import Foundation

@MainActor
protocol SecureStoreProtocol {
    @discardableResult
    func store(key: String, value: String) async -> Bool
    func retrieve(key: String) async -> String?
    func delete(key: String) async
}
