import CChapbook
import Foundation

/// How a session reaches the network for catalogs and streamed pages.
///
/// The engine bundles a Rust transport (`ureq`) that is right for a
/// desktop process and wrong on iOS, where reaching the network outside
/// `URLSession` gives up background transfer, the system trust store, App
/// Transport Security, per-app VPN and the cellular-data toggle. So the
/// default is per-platform: `URLSession` on iOS, the bundled transport on
/// macOS. Most apps never name this type; an app with its own configured
/// session — an ephemeral one, a pinned one — passes it explicitly.
public struct HTTPTransport: Sendable {
    enum Kind: Sendable {
        case bundled
        case urlSession(URLSession)
    }

    let kind: Kind

    /// The engine's bundled Rust transport. The macOS default.
    public static let bundled = HTTPTransport(kind: .bundled)

    /// Fetch through the given session. Its configuration is honored
    /// wholesale — timeouts, caching, proxies, `waitsForConnectivity` —
    /// because the engine sends plain GETs and touches nothing else.
    public static func urlSession(_ session: URLSession) -> HTTPTransport {
        HTTPTransport(kind: .urlSession(session))
    }

    /// `URLSession.shared` everywhere the platform owns networking;
    /// [`bundled`](HTTPTransport.bundled) on macOS, where a plain process
    /// socket is the ordinary thing.
    public static var platformDefault: HTTPTransport {
        #if os(macOS)
            return .bundled
        #else
            return .urlSession(.shared)
        #endif
    }

    /// Install into a C config the caller still owns. On failure the
    /// engine has already run the finalizer — its ownership rule — so
    /// there is nothing to release here.
    func install(into config: OpaquePointer) throws {
        guard case .urlSession(let session) = kind else { return }
        let box = Unmanaged.passRetained(URLSessionTransport(session: session))
        try check(
            cb_config_set_http_transport(
                config, transportGet, transportDownload, transportFinalize,
                box.toOpaque()))
    }
}

/// The retained object behind the C `user` pointer. The engine promises
/// its finalizer runs exactly once — when the last session holding the
/// transport closes, or when installation fails — which is what makes
/// `passRetained`/`release` balance without this class counting anything.
final class URLSessionTransport: Sendable {
    let session: URLSession

    init(session: URLSession) {
        self.session = session
    }

    /// One blocking transfer — a session's GET, or the POST/PUT/DELETE a
    /// sync worker turns on; the method and body already ride on the
    /// request. The engine calls this on its loader or sync thread and
    /// expects the transfer settled on return; the semaphore bridges
    /// `URLSession`'s callback world to that contract. (A catalog *open*
    /// is therefore synchronous network — construct catalog sessions off
    /// the main actor.)
    func perform(_ request: URLRequest) -> Fetched {
        let slot = Slot<Fetched>()
        let done = DispatchSemaphore(value: 0)
        session.dataTask(with: request) { data, response, error in
            if let error {
                slot.value = .failure(error.localizedDescription)
            } else if let response = response as? HTTPURLResponse {
                slot.value = .response(response, data ?? Data())
            } else {
                slot.value = .failure("no HTTP response")
            }
            done.signal()
        }.resume()
        done.wait()
        return slot.value ?? .failure("the transfer never completed")
    }

    /// One blocking download, keeping the engine's promise: `dest` either
    /// ends up complete or is not created. `URLSession` lands the bytes
    /// in its own temp file; the hop to a `.part` sibling may cross
    /// volumes, but the final step is a same-directory rename, so no
    /// partial file ever carries the destination's name. A non-2xx status
    /// writes nothing — its body is not the book.
    func download(_ request: URLRequest, toPath dest: String) -> Fetched {
        let slot = Slot<Fetched>()
        let done = DispatchSemaphore(value: 0)
        session.downloadTask(with: request) { temp, response, error in
            defer { done.signal() }
            if let error {
                slot.value = .failure(error.localizedDescription)
                return
            }
            guard let response = response as? HTTPURLResponse else {
                slot.value = .failure("no HTTP response")
                return
            }
            guard (200..<300).contains(response.statusCode) else {
                slot.value = .response(response, Data())
                return
            }
            guard let temp else {
                slot.value = .failure("the platform delivered no file")
                return
            }
            // Inside the handler by necessity: the temp file dies when it
            // returns.
            let files = FileManager.default
            let part = dest + ".part"
            do {
                try? files.removeItem(atPath: part)
                try files.moveItem(atPath: temp.path, toPath: part)
                try files.moveItem(atPath: part, toPath: dest)
                slot.value = .response(response, Data())
            } catch {
                try? files.removeItem(atPath: part)
                slot.value = .failure("landing the download: \(error.localizedDescription)")
            }
        }.resume()
        done.wait()
        return slot.value ?? .failure("the transfer never completed")
    }

    enum Fetched {
        case response(HTTPURLResponse, Data)
        case failure(String)
    }

    /// Crosses the completion handler once: written before the semaphore
    /// signals, read after the wait, and the semaphore is the
    /// happens-before between them.
    private final class Slot<T>: @unchecked Sendable {
        var value: T?
    }
}

/// Rebuild the engine's request as a `URLRequest`, headers verbatim.
private func urlRequest(from request: cb_http_request) -> URLRequest? {
    guard let url = URL(string: String(cString: request.url)) else { return nil }
    var built = URLRequest(url: url)
    if let headers = request.headers {
        for header in UnsafeBufferPointer(start: headers, count: request.header_count) {
            built.setValue(
                String(cString: header.value), forHTTPHeaderField: String(cString: header.name))
        }
    }
    return built
}

/// Feed a fetch's outcome to the engine's response builder. 4xx and 5xx
/// go through as responses — a 401's body is the Authentication Document
/// the login flow needs — and only a transfer that produced nothing
/// reports as a failure. Every header crosses: a sync worker turns on
/// `ETag` and `Location`, and `URLSession` can enumerate them all as
/// cheaply as those two.
private func report(_ outcome: URLSessionTransport.Fetched, into response: OpaquePointer?) {
    switch outcome {
    case .failure(let message):
        _ = cb_http_response_fail(response, message)
    case .response(let http, let body):
        _ = cb_http_response_set_status(response, UInt16(clamping: http.statusCode))
        if let contentType = http.value(forHTTPHeaderField: "Content-Type") {
            _ = cb_http_response_set_content_type(response, contentType)
        }
        for (name, value) in http.allHeaderFields {
            guard let name = name as? String, let value = value as? String,
                name.caseInsensitiveCompare("Content-Type") != .orderedSame
            else { continue }
            _ = cb_http_response_add_header(response, name, value)
        }
        body.withUnsafeBytes { buffer in
            _ = cb_http_response_append_body(
                response, buffer.bindMemory(to: UInt8.self).baseAddress, buffer.count)
        }
    }
}

// The C entry points. Plain functions, not closures, so they carry no
// context — everything they need rides in `user`. Internal rather than
// private because `cb_sync_open` takes the same `get` and `finalize`,
// and `cb_catalog_open` those plus `download`.

func transportGet(
    request: UnsafePointer<cb_http_request>?,
    response: OpaquePointer?,
    user: UnsafeMutableRawPointer?
) {
    guard let request = request?.pointee, let user else { return }
    guard let built = urlRequest(from: request) else {
        _ = cb_http_response_fail(response, "the request URL did not parse")
        return
    }
    let transport = Unmanaged<URLSessionTransport>.fromOpaque(user).takeUnretainedValue()
    report(transport.perform(built), into: response)
}

/// The write half — `cb_http_send_fn`. Only a sync worker installs it:
/// reconciling marks means POST, PUT and DELETE against a Web Annotation
/// container, and a position PUT against a progression service.
func transportSend(
    method: UnsafePointer<CChar>?,
    request: UnsafePointer<cb_http_request>?,
    body: UnsafePointer<UInt8>?,
    bodyLen: Int,
    response: OpaquePointer?,
    user: UnsafeMutableRawPointer?
) {
    guard let method, let request = request?.pointee, let user else { return }
    guard var built = urlRequest(from: request) else {
        _ = cb_http_response_fail(response, "the request URL did not parse")
        return
    }
    built.httpMethod = String(cString: method)
    if let body, bodyLen > 0 {
        // Copied here by necessity: the engine's bytes die when this
        // callback returns, and the data task outlives the call frame.
        built.httpBody = Data(bytes: body, count: bodyLen)
    }
    let transport = Unmanaged<URLSessionTransport>.fromOpaque(user).takeUnretainedValue()
    report(transport.perform(built), into: response)
}

func transportDownload(
    request: UnsafePointer<cb_http_request>?,
    dest: UnsafePointer<CChar>?,
    response: OpaquePointer?,
    user: UnsafeMutableRawPointer?
) {
    guard let request = request?.pointee, let dest, let user else { return }
    guard let built = urlRequest(from: request) else {
        _ = cb_http_response_fail(response, "the request URL did not parse")
        return
    }
    let transport = Unmanaged<URLSessionTransport>.fromOpaque(user).takeUnretainedValue()
    report(transport.download(built, toPath: String(cString: dest)), into: response)
}

func transportFinalize(user: UnsafeMutableRawPointer?) {
    guard let user else { return }
    Unmanaged<URLSessionTransport>.fromOpaque(user).release()
}
