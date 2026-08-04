import Foundation
import Security

@MainActor
final class KeychainSecureStore: SecureStoreProtocol {
    private let serviceName: String

    init(serviceName: String) {
        self.serviceName = serviceName
    }

    @discardableResult
    func store(key: String, value: String) async -> Bool {
        let data = Data(value.utf8)
        let query = baseQuery(for: key)

        // Never update accessibility via SecItemUpdate — SecItemUpdate can't change
        // kSecAttrAccessible, so we only use it to overwrite the value. Pre-existing
        // items created without accessibility set will be migrated on first store:
        // we delete and re-add so the new item inherits ThisDeviceOnly.
        let updateStatus = SecItemUpdate(
            query as CFDictionary,
            [kSecValueData as String: data] as CFDictionary
        )
        if updateStatus == errSecSuccess {
            return true
        }
        if updateStatus != errSecItemNotFound {
            return false
        }

        var insertQuery = query
        insertQuery[kSecValueData as String] = data
        // kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly:
        //  - `ThisDeviceOnly` excludes the item from iCloud Keychain backups and
        //    iCloud/Finder device restores, so push grants and session keys can't
        //    be carried to another device.
        //  - `AfterFirstUnlock` permits background reads (notification extensions,
        //    BG fetches) once the device has been unlocked since boot.
        insertQuery[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        let addStatus = SecItemAdd(insertQuery as CFDictionary, nil)
        return addStatus == errSecSuccess
    }

    func retrieve(key: String) async -> String? {
        var query = baseQuery(for: key)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne

        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        guard
            status == errSecSuccess,
            let data = item as? Data
        else {
            return nil
        }

        return String(data: data, encoding: .utf8)
    }

    func delete(key: String) async {
        SecItemDelete(baseQuery(for: key) as CFDictionary)
    }

    private func baseQuery(for key: String) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: serviceName,
            kSecAttrAccount as String: key,
        ]
    }
}
