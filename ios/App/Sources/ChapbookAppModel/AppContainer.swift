import Chapbook
import Foundation

/// The platform, as the engine's application layer wants it named: where
/// the library is, where fonts come from, which store keeps secrets,
/// which transport reaches the network, what to call this device. One
/// value, `Sendable`, so any isolation domain that needs an `App` handle
/// of its own — the shelf's actor, a catalog's, a one-off open — can
/// make one here rather than share one across threads.
public struct Platform: Sendable {
    public let libraryDirectory: URL
    public let fonts: FontSource
    public let credentials: Credentials
    public let transport: HTTPTransport
    public let deviceName: String

    public init(
        libraryDirectory: URL, fonts: FontSource, credentials: Credentials,
        transport: HTTPTransport, deviceName: String
    ) {
        self.libraryDirectory = libraryDirectory
        self.fonts = fonts
        self.credentials = credentials
        self.transport = transport
        self.deviceName = deviceName
    }

    /// An `App` over this platform, for the caller's own thread.
    public func open() throws -> App {
        try App(
            libraryDirectory: libraryDirectory, fonts: fonts, credentials: credentials,
            transport: transport, deviceName: deviceName)
    }
}

/// Everything the screens ask that is not a widget, built once per
/// process.
///
/// The decisions live in the engine's application layer (`Chapbook.App`);
/// what this module holds is the platform's half — the Keychain behind
/// the credential store, `URLSession` behind the transport and the
/// transfer, the security-scoped bookmark that is a grant — and the
/// isolation the engine's rules require. Nothing in this module imports
/// UIKit, so all of it runs under `swift test` with no screen.
@MainActor
public final class AppContainer: ObservableObject {
    /// Where positions, marks, settings, grants, preferences and the
    /// library's copies live: `Library/Application Support/chapbook`,
    /// the sandbox's own answer to a question the engine deliberately
    /// does not answer for it.
    public let libraryDirectory: URL

    public let platform: Platform
    /// The main actor's own handle, for the small synchronous questions
    /// a screen asks directly: the saved catalogs, a preference.
    public let app: App

    public let shelf: Shelf
    public let grants: Grants
    public let opener: Opener
    public let credentials: Credentials
    public let http: Http
    public let catalogs: Catalogs
    public let downloads: Downloads
    public let preferences: Preferences

    /// A file another app asked us to open, waiting for the shelf to take
    /// it. Set from `onOpenURL`, cleared by whoever handles it.
    @Published public var openRequest: URL?

    /// Built over the app's own container, with a background transfer
    /// session under `downloadsIdentifier`.
    public convenience init(downloadsIdentifier: String = "com.ophymx.chapbook.downloads") throws {
        let support = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        let library = support.appendingPathComponent("chapbook", isDirectory: true)
        try? FileManager.default.createDirectory(at: library, withIntermediateDirectories: true)
        let configuration = URLSessionConfiguration.background(withIdentifier: downloadsIdentifier)
        configuration.isDiscretionary = false
        configuration.sessionSendsLaunchEvents = true
        try self.init(
            libraryDirectory: library,
            defaults: .standard,
            credentials: Credentials(service: "com.ophymx.chapbook.credentials"),
            downloads: configuration)
    }

    /// Every piece named, for a test that wants a scratch directory, a
    /// transfer session that needs no background, and a stubbed `http`
    /// session for the catalog to browse through. `defaults` is kept for
    /// the signature's sake: nothing the model keeps lives there any
    /// more, since the engine keeps it beside the shelf.
    public init(
        libraryDirectory: URL,
        defaults: UserDefaults,
        credentials: Credentials,
        downloads configuration: URLSessionConfiguration,
        http httpConfiguration: URLSessionConfiguration = .default,
        container: URL? = nil
    ) throws {
        _ = defaults
        self.libraryDirectory = libraryDirectory
        self.credentials = credentials
        http = Http(credentials: credentials, configuration: httpConfiguration)
        platform = Platform(
            libraryDirectory: libraryDirectory, fonts: Opener.fonts, credentials: credentials,
            transport: http.transport, deviceName: Self.deviceName)
        app = try platform.open()
        shelf = Shelf(platform: platform)
        grants = Grants(shelf: shelf)
        opener = Opener(platform: platform, shelf: shelf, container: container)
        catalogs = Catalogs(app: app)
        preferences = Preferences(app: app)
        self.downloads = Downloads(
            configuration: configuration, credentials: credentials, shelf: shelf,
            staging: libraryDirectory.appendingPathComponent("downloads", isDirectory: true))
    }

    /// What a progression service shows beside this device's position.
    static var deviceName: String {
        #if os(iOS)
            return "iPhone"
        #else
            return ProcessInfo.processInfo.hostName
        #endif
    }
}
