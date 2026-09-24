// The application: the shelf, a book, the saved catalogs, and one catalog
// being browsed — SwiftUI over `ChapbookAppModel`, which is where every
// decision lives. The screens ask the model and draw.

import Chapbook
import ChapbookAppModel
import SwiftUI
import os

@main
struct ChapbookApplication: App {
    @UIApplicationDelegateAdaptor(AppDelegate.self) private var delegate

    var body: some Scene {
        WindowGroup {
            RootView(container: delegate.container)
                // A file another app wants read. The shelf takes it from here.
                .onOpenURL { url in delegate.container.openRequest = url }
        }
    }
}

/// The process. The engine has no voice until a host gives it one, so
/// logging is installed here, before anything can fail quietly — and the
/// background transfer session is rebuilt here when the system relaunches
/// the app to deliver a download that landed while it was gone.
@MainActor
final class AppDelegate: NSObject, UIApplicationDelegate {
    let container: AppContainer

    override init() {
        // Into the unified log, so `log stream` on a simulator or a
        // sysdiagnose from a device carries the engine's lines in order
        // with the app's own.
        let log = Logger(subsystem: "com.ophymx.chapbook", category: "engine")
        EngineLog.install { level, target, message in
            switch level {
            case .error: log.error("\(target, privacy: .public): \(message, privacy: .public)")
            case .warn: log.warning("\(target, privacy: .public): \(message, privacy: .public)")
            default: log.info("\(target, privacy: .public): \(message, privacy: .public)")
            }
        }
        do {
            container = try AppContainer()
        } catch {
            // No library means no app: the sandbox refused its own
            // Application Support directory, which nothing here can mend.
            fatalError("the library did not open: \(error)")
        }
        super.init()
    }

    func application(
        _ application: UIApplication,
        handleEventsForBackgroundURLSession identifier: String,
        completionHandler: @escaping @Sendable () -> Void
    ) {
        container.downloads.backgroundCompletion = completionHandler
    }
}

/// Where the navigation stack can go.
enum Route: Hashable {
    case reader(Int64)
    case catalogs
    case catalog(String)
}

struct RootView: View {
    @ObservedObject var container: AppContainer
    @State private var path: [Route] = []

    var body: some View {
        NavigationStack(path: $path) {
            ShelfScreen(
                container: container,
                onOpen: { id in path.append(.reader(id)) },
                onCatalogs: { path.append(.catalogs) }
            )
            .navigationDestination(for: Route.self) { route in
                switch route {
                case .reader(let id):
                    ReaderScreen(container: container, bookID: id)
                case .catalogs:
                    CatalogsScreen(container: container, onOpen: { id in path.append(.catalog(id)) })
                case .catalog(let id):
                    CatalogScreen(container: container, catalogID: id)
                }
            }
        }
        .onAppear(perform: takeLaunchArguments)
    }

    /// Two doors a launch command can walk through, for driving the app
    /// from `simctl launch` with no finger on the glass — the shape a
    /// screenshot pass or a dev loop wants:
    ///
    ///     -open <file>      hand a book over, as another app would
    ///     -catalog <url>    add a catalog and browse it; a `user:pass@`
    ///                       in the URL is stored by origin, never kept
    ///                       in the URL
    private func takeLaunchArguments() {
        let arguments = CommandLine.arguments
        for (index, argument) in arguments.enumerated() {
            guard index + 1 < arguments.count else { break }
            switch argument {
            case "-open":
                container.openRequest = URL(fileURLWithPath: arguments[index + 1])
            case "-catalog":
                guard var parts = URLComponents(string: arguments[index + 1]) else { break }
                if let user = parts.user, let origin = parts.url.flatMap(Credentials.origin(of:)) {
                    container.credentials.set(
                        origin, authorization: Credentials.basic(username: user, password: parts.password ?? ""))
                    parts.user = nil
                    parts.password = nil
                }
                guard let url = parts.string else { break }
                path = [.catalogs, .catalog(container.catalogs.add(url: url).id)]
            default:
                break
            }
        }
    }
}
