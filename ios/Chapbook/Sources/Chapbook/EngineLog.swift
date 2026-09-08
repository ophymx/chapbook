import CChapbook
import Foundation

/// The engine's diagnostics. It is silent until routed somewhere — call
/// [`install(minimum:_:)`] before opening anything, because the failures
/// most worth seeing are the ones during open.
public enum EngineLog {
    public enum Level: Int32, Comparable, Sendable {
        case error = 1
        case warn = 2
        case info = 3
        case debug = 4
        case trace = 5

        public static func < (lhs: Level, rhs: Level) -> Bool {
            lhs.rawValue < rhs.rawValue
        }
    }

    /// One process-wide sink, same as the `log` crate underneath. The
    /// handler **fires on any thread** — the loader thread included — so
    /// it must not touch UI or call back into the engine; hand the line
    /// to `print`, `os_log`, or something the main loop drains.
    public static func install(
        minimum: Level = .info,
        _ handler: @escaping @Sendable (Level, _ target: String, _ message: String) -> Void
    ) {
        sink.replace(handler)
        // A capture-free closure is a C function pointer; the state
        // lives in `sink` because `user` cannot hold a Swift capture.
        _ = cb_set_log_callback(
            { level, target, message, _ in
                guard let handler = EngineLog.sink.current() else { return }
                handler(
                    Level(rawValue: level) ?? .info,
                    target.map { String(cString: $0) } ?? "",
                    message.map { String(cString: $0) } ?? "")
            }, nil, minimum.rawValue)
    }

    /// Stop delivery. The engine keeps working and goes quiet.
    public static func remove() {
        _ = cb_set_log_callback(nil, nil, Level.info.rawValue)
        sink.replace(nil)
    }

    /// Emit one of the app's own lines through whatever sink is
    /// installed.
    ///
    /// Here so an app's messages land in the engine's stream *in order*
    /// with the engine's own, which is the entire reason an interleaved
    /// log is worth having — one path, one ordering, one place to read
    /// when a reader sends a bug report. `target` names the subsystem a
    /// filter can key on, and defaults to `host`.
    public static func write(
        _ message: String, level: Level = .info, target: String? = nil
    ) {
        withOptionalCString(target) { target in
            _ = cb_log(level.rawValue, target, message)
        }
    }

    /// Whether a sink is installed at all — for an app deciding whether
    /// to bother formatting something expensive.
    public static var isEnabled: Bool { cb_log_enabled() }

    fileprivate static let sink = Sink()

    fileprivate final class Sink: @unchecked Sendable {
        private let lock = NSLock()
        private var handler: (@Sendable (Level, String, String) -> Void)?

        func replace(_ new: (@Sendable (Level, String, String) -> Void)?) {
            lock.lock()
            handler = new
            lock.unlock()
        }

        func current() -> (@Sendable (Level, String, String) -> Void)? {
            lock.lock()
            defer { lock.unlock() }
            return handler
        }
    }
}

/// What this build of the engine can actually do — the answer to "which
/// artifact did I link", which a header cannot give.
public struct Capabilities: OptionSet, Sendable {
    public let rawValue: UInt32
    public init(rawValue: UInt32) { self.rawValue = rawValue }

    public static let library = Capabilities(rawValue: 1)
    public static let cbz = Capabilities(rawValue: 2)
    public static let pdf = Capabilities(rawValue: 4)
    public static let opds = Capabilities(rawValue: 8)
    public static let bundledHTTP = Capabilities(rawValue: 16)
    public static let svg = Capabilities(rawValue: 32)
    public static let mathML = Capabilities(rawValue: 64)
    /// Positions and marks reconcile with a book's services. Without it
    /// `SyncWorker` declines to open and the library is read and written
    /// only locally.
    public static let sync = Capabilities(rawValue: 128)

    public static func current() -> Capabilities {
        Capabilities(rawValue: cb_capabilities())
    }
}

/// The linked engine's ABI version, as `major * 10000 + minor * 100 +
/// patch`.
public func engineABIVersion() -> UInt32 {
    cb_abi_version()
}
