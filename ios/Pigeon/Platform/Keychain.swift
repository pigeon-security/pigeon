import Foundation
import Security

/// A token pair as the platform issues it.
struct TokenPair: Codable, Equatable {
    var access: String
    var refresh: String
    var expiresAt: Date
}

/// What the keychain holds for this app: the client id and the token pair.
enum Keychain {
    private static let service = "dev.pigeon.app"

    /// The keys this app uses.
    enum Key {
        static let clientId = "client_id"
        static let tokens = "tokens"
    }

    /// Stores `value` under `key`, replacing any old value.
    static func save<T: Encodable>(_ value: T, for key: String) throws {
        let data = try JSONEncoder().encode(value)
        delete(key)
        var query = base(key)
        query[kSecValueData as String] = data
        query[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        let status = SecItemAdd(query as CFDictionary, nil)
        guard status == errSecSuccess else { throw PlatformError.keychain(status) }
    }

    /// The value stored under `key`, or nil when there is none or it cannot be read.
    static func load<T: Decodable>(_ type: T.Type, for key: String) -> T? {
        var query = base(key)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var out: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &out) == errSecSuccess,
            let data = out as? Data
        else { return nil }
        return try? JSONDecoder().decode(type, from: data)
    }

    /// Removes the item stored under `key`.
    static func delete(_ key: String) {
        SecItemDelete(base(key) as CFDictionary)
    }

    private static func base(_ key: String) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: key,
        ]
    }
}
