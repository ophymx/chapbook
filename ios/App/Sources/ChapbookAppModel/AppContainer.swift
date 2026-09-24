import Chapbook
import Foundation

/// Everything the screens ask that is not a widget, built once per
/// process.
///
/// This is the app's model in the sense `chapbook-app` is the desktop's:
/// which books the shelf shows, how a book is opened and how it is found
/// again, and the threads the engine's rules require. Nothing in this
/// module imports UIKit, so all of it runs under `swift test` with no
/// screen.
@MainActor
public final class AppContainer: ObservableObject {
    /// Where positions, marks, settings and the library's copies live:
    /// `Library/Application Support/chapbook`, the sandbox's own answer
    /// to a question the engine deliberately does not answer for it.
    public let libraryDirectory: URL

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
    public convenience init(downloadsIdentifier: String = "com.ophymx.chapbook.downloads") {
        let support = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        let library = support.appendingPathComponent("chapbook", isDirectory: true)
        try? FileManager.default.createDirectory(at: library, withIntermediateDirectories: true)
        let configuration = URLSessionConfiguration.background(withIdentifier: downloadsIdentifier)
        configuration.isDiscretionary = false
        configuration.sessionSendsLaunchEvents = true
        self.init(
            libraryDirectory: library,
            defaults: .standard,
            credentials: Credentials(service: "com.ophymx.chapbook.credentials"),
            downloads: configuration)
    }

    /// Every piece named, for a test that wants a scratch directory, its
    /// own defaults and a transfer session that needs no background.
    public init(
        libraryDirectory: URL,
        defaults: UserDefaults,
        credentials: Credentials,
        downloads configuration: URLSessionConfiguration,
        container: URL? = nil
    ) {
        self.libraryDirectory = libraryDirectory
        self.credentials = credentials
        shelf = Shelf(directory: libraryDirectory)
        grants = Grants(defaults: defaults)
        opener = Opener(libraryDirectory: libraryDirectory, shelf: shelf, grants: grants, container: container)
        http = Http(credentials: credentials)
        catalogs = Catalogs(defaults: defaults)
        preferences = Preferences(defaults: defaults)
        self.downloads = Downloads(
            configuration: configuration, credentials: credentials, shelf: shelf,
            staging: libraryDirectory.appendingPathComponent("downloads", isDirectory: true))
    }
}
