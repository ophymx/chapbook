import Chapbook
import Foundation

/// One catalog being browsed, on the one thread it is allowed to be on.
///
/// Every `Catalog` call blocks and the handle is one thread's at a time,
/// so a session owns a serial queue and hops every call onto it. The
/// catalog is opened on that queue on first use — through an `App` of
/// this actor's own, so it browses over the app's transport and signs in
/// through the app's credential store, which the engine reads before
/// every fetch. `savedID` is `nil` for a catalog that has no saved row,
/// a pasted URL.
public actor CatalogSession {
    private let platform: Platform
    private let savedID: Int64?
    private let queue = DispatchSerialQueue(label: "chapbook-catalog")
    private var app: App?
    private var catalog: Catalog?

    public nonisolated var unownedExecutor: UnownedSerialExecutor { queue.asUnownedSerialExecutor() }

    public init(platform: Platform, savedID: Int64?) {
        self.platform = platform
        self.savedID = savedID
    }

    /// Run a block over the catalog.
    public func use<T: Sendable>(_ body: @Sendable (Catalog) throws -> T) throws -> T {
        try body(try open())
    }

    private func open() throws -> Catalog {
        if let catalog { return catalog }
        let app = try self.app ?? platform.open()
        self.app = app
        let opened = try app.browse(savedID)
        catalog = opened
        return opened
    }
}
