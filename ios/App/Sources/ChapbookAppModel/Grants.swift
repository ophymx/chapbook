import Foundation

/// How an adopted book is found again.
///
/// A book picked from another provider reaches the library by content:
/// it is recorded under its fingerprint, and the library keeps no copy.
/// Reopening the *file* next launch is the app's job, and the engine
/// keeps the map that makes it possible — fingerprint to the
/// security-scoped bookmark, as opaque bytes beside the shelf. This is
/// that map as the shelf's actor answers it.
public final class Grants: Sendable {
    private let shelf: Shelf

    public init(shelf: Shelf) {
        self.shelf = shelf
    }

    public func bookmark(for fingerprint: String) async throws -> Data? {
        try await shelf.grant(for: fingerprint)
    }

    public func remember(_ bookmark: Data, for fingerprint: String) async throws {
        try await shelf.remember(grant: bookmark, for: fingerprint)
    }

    public func forget(_ fingerprint: String) async throws {
        try await shelf.forgetGrant(for: fingerprint)
    }
}
