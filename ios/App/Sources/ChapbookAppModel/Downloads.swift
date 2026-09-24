import Chapbook
import Foundation

/// Downloads as jobs, the way a transfer that has to outlive its screen
/// runs on a phone.
///
/// The catalog describes a fetch (`Catalog.DownloadRequest`) and steps
/// aside; the request rides in the task's `taskDescription` — everything
/// but a credential, which is read from `Credentials` by origin when the
/// task is made, so no secret sits in the transfer system's store and a
/// token rotated meanwhile is simply fresh. When the file lands the job
/// hands it to the engine's landing, which shelves it and records the
/// sync services the entry carried — they live in the entry and nowhere
/// else, the reason the request captured them before the transfer rather
/// than after. How a status is read is the engine's table too
/// (`DownloadOutcome`), so this delegate and the Android worker cannot
/// disagree about a 403.
///
/// A background `URLSession` wants a delegate rather than a completion
/// handler precisely because a transfer that survives suspension is a
/// job: the process may be gone when the file lands, and the system
/// relaunches it into `handleEventsForBackgroundURLSession`, where the
/// app builds this object again under the same identifier and the
/// delegate picks the landing up. `jobs` is rebuilt from the session's
/// own task list on construction for the same reason.
@MainActor
public final class Downloads: ObservableObject {
    /// One transfer, as the shelf shows it arriving.
    public struct Job: Identifiable, Sendable {
        public enum State: Hashable, Sendable {
            case running
            case landed(book: Int64)
            case failed(String)
        }

        public let id: Int
        public let request: Catalog.DownloadRequest
        public var progress: Double?
        public var state: State

        public var isFinished: Bool {
            if case .running = state { return false }
            return true
        }
    }

    @Published public private(set) var jobs: [Job] = []
    /// Bumped every time a book lands, so a shelf can follow it without
    /// reading the job list.
    @Published public private(set) var landings = 0

    /// What the system handed the app on relaunch for this session; called
    /// once the delegate has seen the last event.
    public var backgroundCompletion: (@Sendable () -> Void)?

    private let session: URLSession
    private let credentials: Credentials
    private let shelf: Shelf
    private let staging: URL
    private let bridge: Bridge

    public init(configuration: URLSessionConfiguration, credentials: Credentials, shelf: Shelf, staging: URL) {
        self.credentials = credentials
        self.shelf = shelf
        self.staging = staging
        try? FileManager.default.createDirectory(at: staging, withIntermediateDirectories: true)
        bridge = Bridge()
        session = URLSession(configuration: configuration, delegate: bridge, delegateQueue: nil)
        bridge.owner = self
        // Transfers that outlived a previous process are still running;
        // they are jobs from the first frame, not on their completion.
        session.getAllTasks { [weak self] tasks in
            let running = tasks.compactMap { task -> Job? in
                guard let request = Self.request(of: task) else { return nil }
                return Job(id: task.taskIdentifier, request: request, progress: nil, state: .running)
            }
            Task { @MainActor [weak self] in
                guard let self else { return }
                for job in running where !self.jobs.contains(where: { $0.id == job.id }) {
                    self.jobs.append(job)
                }
            }
        }
    }

    /// Start a transfer, or do nothing when one for this entry is already
    /// running: tapping twice does not fetch twice, and a job system that
    /// retries meets an import that is idempotent.
    public func enqueue(_ request: Catalog.DownloadRequest) {
        if jobs.contains(where: { $0.request.entryID == request.entryID && !$0.isFinished }) { return }
        var urlRequest = request.urlRequest
        // `Authorization` is the app's to add, by origin, at this moment.
        if let authorization = credentials.authorization(for: request.url) {
            urlRequest.setValue(authorization, forHTTPHeaderField: "Authorization")
        }
        let task = session.downloadTask(with: urlRequest)
        task.taskDescription = Self.describe(request)
        jobs.append(Job(id: task.taskIdentifier, request: request, progress: nil, state: .running))
        task.resume()
    }

    /// How many transfers are running, for a shelf badge.
    public var active: Int { jobs.filter { !$0.isFinished }.count }

    /// Take a finished job off the list once the reader has seen it.
    public func dismiss(_ id: Int) {
        jobs.removeAll { $0.id == id && $0.isFinished }
    }

    // MARK: The request, persisted

    nonisolated static func describe(_ request: Catalog.DownloadRequest) -> String? {
        (try? JSONEncoder().encode(request)).map { String(decoding: $0, as: UTF8.self) }
    }

    nonisolated static func request(of task: URLSessionTask) -> Catalog.DownloadRequest? {
        guard let description = task.taskDescription else { return nil }
        return try? JSONDecoder().decode(Catalog.DownloadRequest.self, from: Data(description.utf8))
    }

    // MARK: What the delegate reports

    fileprivate func progressed(_ id: Int, fraction: Double) {
        guard let index = jobs.firstIndex(where: { $0.id == id }) else { return }
        jobs[index].progress = fraction
    }

    /// The file is ours, moved out of the system's temp dir already. Shelve
    /// it, record where it syncs, and tidy up — whatever happens to the
    /// import, the staging file goes.
    fileprivate func landed(_ id: Int, request: Catalog.DownloadRequest, file: URL) async {
        defer { try? FileManager.default.removeItem(at: file) }
        do {
            let book = try await shelf.landDownload(
                at: file, progressionURL: request.progressionURL,
                annotationContainer: request.annotationContainer)
            settle(id, request: request, state: .landed(book: book))
            landings += 1
        } catch {
            settle(id, request: request, state: .failed("not a book: \(error)"))
        }
    }

    fileprivate func failed(_ id: Int, request: Catalog.DownloadRequest?, reason: String) {
        settle(id, request: request, state: .failed(reason))
    }

    private func settle(_ id: Int, request: Catalog.DownloadRequest?, state: Job.State) {
        if let index = jobs.firstIndex(where: { $0.id == id }) {
            jobs[index].state = state
            jobs[index].progress = nil
        } else if let request {
            // A task from a previous process, landing before the job list
            // caught up with it.
            jobs.append(Job(id: id, request: request, progress: nil, state: state))
        }
    }

    fileprivate func finishedEvents() {
        let completion = backgroundCompletion
        backgroundCompletion = nil
        completion?()
    }

    /// Off the main actor on purpose: the delegate runs on the session's
    /// queue and the system's temp file dies when its callback returns.
    nonisolated fileprivate func stage(_ location: URL) -> URL? {
        let staged = staging.appendingPathComponent(UUID().uuidString)
        do {
            try FileManager.default.moveItem(at: location, to: staged)
            return staged
        } catch {
            return nil
        }
    }

    /// The delegate: nonisolated by the platform's contract, so it holds
    /// nothing but a reference and hops to the owner for every fact.
    private final class Bridge: NSObject, URLSessionDownloadDelegate, @unchecked Sendable {
        weak var owner: Downloads?

        func urlSession(
            _ session: URLSession, downloadTask: URLSessionDownloadTask, didWriteData bytesWritten: Int64,
            totalBytesWritten: Int64, totalBytesExpectedToWrite: Int64
        ) {
            guard totalBytesExpectedToWrite > 0 else { return }
            let id = downloadTask.taskIdentifier
            let fraction = Double(totalBytesWritten) / Double(totalBytesExpectedToWrite)
            Task { @MainActor [owner] in owner?.progressed(id, fraction: fraction) }
        }

        func urlSession(
            _ session: URLSession, downloadTask: URLSessionDownloadTask, didFinishDownloadingTo location: URL
        ) {
            let id = downloadTask.taskIdentifier
            let request = Downloads.request(of: downloadTask)
            // A refusal lands as a file too — the error page — so the
            // status decides before the bytes are believed, read the way
            // every front end reads it.
            let status = (downloadTask.response as? HTTPURLResponse)?.statusCode ?? 200
            switch DownloadOutcome.of(status: status) {
            case .landed:
                break
            case .refused:
                Task { @MainActor [owner] in owner?.failed(id, request: request, reason: "refused") }
                return
            case .gone, .again:
                Task { @MainActor [owner] in owner?.failed(id, request: request, reason: "gone (\(status))") }
                return
            }
            guard let request else {
                Task { @MainActor [owner] in owner?.failed(id, request: nil, reason: "no request on the task") }
                return
            }
            // The temp file dies when this returns; the move has to be now.
            guard let staged = owner?.stage(location) else {
                Task { @MainActor [owner] in owner?.failed(id, request: request, reason: "could not keep the file") }
                return
            }
            Task { @MainActor [owner] in await owner?.landed(id, request: request, file: staged) }
        }

        func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: Error?) {
            guard let error else { return }
            let id = task.taskIdentifier
            let request = Downloads.request(of: task)
            let reason = error.localizedDescription
            Task { @MainActor [owner] in owner?.failed(id, request: request, reason: reason) }
        }

        func urlSessionDidFinishEvents(forBackgroundURLSession session: URLSession) {
            Task { @MainActor [owner] in owner?.finishedEvents() }
        }
    }
}
