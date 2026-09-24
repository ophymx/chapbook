import Chapbook
import Foundation

/// What the reader's progress readout says — the engine's choice set,
/// the app's words.
public typealias ProgressLabel = Chapbook.ProgressLabel

/// The app's display preferences, kept by the engine beside the shelf.
///
/// These are the shell's, not the reader's: how the reader *shows* a
/// thing, not how it lays a page out. The value is mirrored here so a
/// screen can observe it; a change is shown at once and written through.
@MainActor
public final class Preferences: ObservableObject {
    private let app: App

    @Published public private(set) var progressLabel: ProgressLabel

    public init(app: App) {
        self.app = app
        progressLabel = app.progressLabel
    }

    public func setProgressLabel(_ label: ProgressLabel) {
        app.progressLabel = label
        progressLabel = label
    }
}
