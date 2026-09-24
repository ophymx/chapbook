import Chapbook
import Foundation

/// One open catalog, on the one thread it is allowed to be on.
///
/// Every `Catalog` call blocks and the handle is one thread's at a time,
/// so a session owns a serial queue and hops every call onto it. The
/// catalog is opened on that queue on first use, and the origin's
/// credential is set on it before every fetch — read fresh each time, so
/// a sign-in stored while a feed was open reaches the next request.
public actor CatalogSession {
    private let transport: HTTPTransport
    private let credentials: Credentials
    private let queue = DispatchSerialQueue(label: "chapbook-catalog")
    private var catalog: Catalog?

    public nonisolated var unownedExecutor: UnownedSerialExecutor { queue.asUnownedSerialExecutor() }

    public init(transport: HTTPTransport, credentials: Credentials) {
        self.transport = transport
        self.credentials = credentials
    }

    /// Run a block over the catalog, credentialed for `url`'s origin.
    public func use<T: Sendable>(for url: URL, _ body: @Sendable (Catalog) throws -> T) throws -> T {
        let catalog = try open()
        try catalog.setAuthorization(credentials.authorization(for: url))
        return try body(catalog)
    }

    private func open() throws -> Catalog {
        if let catalog { return catalog }
        let opened = try Catalog(transport: transport)
        catalog = opened
        return opened
    }
}
