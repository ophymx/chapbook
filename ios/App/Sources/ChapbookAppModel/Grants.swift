import Foundation

/// How an adopted book is found again.
///
/// A book picked from another provider reaches the library by content:
/// it is recorded under its fingerprint, and the library keeps no copy.
/// Reopening the *file* next launch is the app's job, and this is the
/// map that makes it possible — fingerprint to the security-scoped
/// bookmark. Keyed by fingerprint rather than row id because the
/// fingerprint survives a reinstall; the id is the reader's history of
/// the book. A bookmark is not a secret, so defaults hold it.
// `UserDefaults` is documented thread-safe and not marked so; the claim is
// made here, once.
public final class Grants: @unchecked Sendable {
    private let defaults: UserDefaults

    public init(defaults: UserDefaults) {
        self.defaults = defaults
    }

    private func key(_ fingerprint: String) -> String { "grant:\(fingerprint)" }

    public func bookmark(for fingerprint: String) -> Data? {
        defaults.data(forKey: key(fingerprint))
    }

    public func remember(_ bookmark: Data, for fingerprint: String) {
        defaults.set(bookmark, forKey: key(fingerprint))
    }

    public func forget(_ fingerprint: String) {
        defaults.removeObject(forKey: key(fingerprint))
    }
}
