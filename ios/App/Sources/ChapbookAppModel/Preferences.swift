import Foundation

/// What the reader's progress readout says, beside the whole-book bar.
public enum ProgressLabel: String, CaseIterable, Sendable {
    /// "34%" of the whole book.
    case percent
    /// "6 left in chapter" — how many pages remain in this unit.
    case pagesLeft
    /// "unit 6/20 · page 2/11" — the raw indices.
    case chapterPage
}

/// The app's display preferences.
///
/// These are the shell's, not the engine's: how the reader *shows* a
/// thing, not how it lays a page out. So they live in defaults here
/// rather than in `ReadingSettings`, which crosses the engine boundary
/// and drives layout. Nothing here is a secret.
@MainActor
public final class Preferences: ObservableObject {
    private let defaults: UserDefaults
    private static let progressKey = "progress_label"

    @Published public private(set) var progressLabel: ProgressLabel

    public init(defaults: UserDefaults) {
        self.defaults = defaults
        progressLabel = defaults.string(forKey: Self.progressKey).flatMap(ProgressLabel.init(rawValue:)) ?? .percent
    }

    public func setProgressLabel(_ label: ProgressLabel) {
        defaults.set(label.rawValue, forKey: Self.progressKey)
        progressLabel = label
    }
}
