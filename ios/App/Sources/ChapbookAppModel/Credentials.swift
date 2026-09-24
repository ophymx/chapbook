import Chapbook
import Foundation
import Security

/// The credentials the app holds, keyed the way the engine keys them.
///
/// A credential is an opaque `Authorization` header value — a Basic pair
/// encoded, a bearer token, whatever the catalog took — and the key is
/// the engine's for the origin it is for (`App.credentialKey(for:)`),
/// never a catalog URL, whose path may itself be a secret. The engine
/// stores a sign-in through this class and reads it back before a fetch;
/// the app's own code — the cover loader, a download job — asks the same
/// way. Values sit in the Keychain, one generic-password item per key
/// under this app's service name. Nothing here prompts; a lookup is a
/// lookup, on whatever thread the engine is on.
public final class Credentials: CredentialStore, Sendable {
    private let service: String

    public init(service: String) {
        self.service = service
    }

    private func query(_ origin: String) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: origin,
        ]
    }

    /// The header value for `origin`, or `nil` when none is held.
    public func get(_ origin: String) -> String? {
        var query = query(origin)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var item: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &item) == errSecSuccess,
            let data = item as? Data
        else { return nil }
        return String(data: data, encoding: .utf8)
    }

    public func set(_ origin: String, authorization: String) {
        let data = Data(authorization.utf8)
        let update = [kSecValueData as String: data]
        var status = SecItemUpdate(query(origin) as CFDictionary, update as CFDictionary)
        if status == errSecItemNotFound {
            var add = query(origin)
            add[kSecValueData as String] = data
            add[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlock
            status = SecItemAdd(add as CFDictionary, nil)
        }
        if status != errSecSuccess {
            // A Keychain that refuses is a sign-in that will be asked for
            // again; say so where a bug report can find it. Never the value.
            EngineLog.write("keychain refused a credential for \(origin): \(status)", level: .error, target: "credentials")
        }
    }

    public func forget(_ origin: String) {
        SecItemDelete(query(origin) as CFDictionary)
    }

    /// Every origin a credential is held for.
    public func origins() -> Set<String> {
        var query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecReturnAttributes as String: true,
            kSecMatchLimit as String: kSecMatchLimitAll,
        ]
        var items: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &items) == errSecSuccess,
            let rows = items as? [[String: Any]]
        else {
            query.removeAll()
            return []
        }
        return Set(rows.compactMap { $0[kSecAttrAccount as String] as? String })
    }

    /// The credential a request to `url` should carry, or `nil`.
    public func authorization(for url: URL) -> String? {
        Self.origin(of: url).flatMap(get)
    }

    /// The key a credential for `url` lives under: the engine's, so a
    /// sign-in the engine stored is what a cover request finds. `nil`
    /// for anything that is not a URL with an origin.
    public static func origin(of url: URL) -> String? {
        App.credentialKey(for: url)
    }

    public static func origin(of string: String) -> String? {
        App.credentialKey(for: string)
    }

    /// The Basic scheme's header value for a username and password.
    public static func basic(username: String, password: String) -> String {
        App.basicAuthorization(username: username, password: password)
    }
}
