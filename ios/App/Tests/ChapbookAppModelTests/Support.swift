import Chapbook
import Foundation
import Testing

@testable import ChapbookAppModel

// The model without a screen, on the macOS slice: the same boundary the
// iOS slices carry, driven the way the view models drive it.

let fixtures = URL(fileURLWithPath: #filePath)
    .deletingLastPathComponent()  // ChapbookAppModelTests
    .deletingLastPathComponent()  // Tests
    .deletingLastPathComponent()  // App
    .deletingLastPathComponent()  // ios
    .deletingLastPathComponent()  // repo root
    .appendingPathComponent("fixtures")

func fixture(_ path: String) throws -> Data {
    try Data(contentsOf: fixtures.appendingPathComponent(path))
}

func scratch(_ name: String) throws -> URL {
    let dir = FileManager.default.temporaryDirectory
        .appendingPathComponent("chapbook-app-test-\(ProcessInfo.processInfo.processIdentifier)-\(name)")
    try? FileManager.default.removeItem(at: dir)
    try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    return dir
}

/// A container over a scratch directory, its own defaults suite, and a
/// transfer session that needs no background — and the bundled faces
/// pointed at the fixtures, since a test has no bundle.
@MainActor
func container(_ name: String, transfers: URLSessionConfiguration = .ephemeral) throws -> (AppContainer, URL) {
    Opener.fontsDirectory = fixtures.appendingPathComponent("fonts")
    let dir = try scratch(name)
    let suite = "chapbook-app-test-\(name)-\(ProcessInfo.processInfo.processIdentifier)"
    let defaults = UserDefaults(suiteName: suite)!
    defaults.removePersistentDomain(forName: suite)
    let container = AppContainer(
        libraryDirectory: dir, defaults: defaults,
        // The Keychain is machine state, not process state: a service
        // name per process keeps one run's sign-in out of the next.
        credentials: Credentials(service: "com.ophymx.chapbook.tests.\(name).\(ProcessInfo.processInfo.processIdentifier)"),
        downloads: transfers, container: dir)
    return (container, dir)
}

/// A book file the way another app would hand one over: in the app's
/// own container, so the opener imports it.
func handed(_ dir: URL, _ name: String, _ path: String = "epub/minimal.epub") throws -> URL {
    let file = dir.appendingPathComponent(name)
    try fixture(path).write(to: file)
    return file
}

/// Wait for a main-actor condition, the way a screen waits on a
/// published value, with a ceiling so a broken flow fails rather than
/// hangs.
@MainActor
func settle(_ seconds: Double = 10, until done: @MainActor () -> Bool) async {
    let deadline = Date().addingTimeInterval(seconds)
    while !done() && Date() < deadline {
        try? await Task.sleep(nanoseconds: 20_000_000)
    }
}

struct TestFailure: Error, CustomStringConvertible {
    let description: String
    init(_ description: String) { self.description = description }
}
