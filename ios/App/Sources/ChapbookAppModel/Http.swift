import Chapbook
import Foundation

/// The app's networking: one session for the catalog and covers, and the
/// engine's transport over it.
///
/// The binding bundles no HTTP stack on iOS, so the trust store is the
/// device's and a catalog behind a user CA or a corporate proxy works
/// because `URLSession` does. Credentials are the app's to attach, per
/// request, by the origin asked for — `URLSession` has no interceptor,
/// so each door does it at the moment it builds a request: the catalog
/// session sets the origin's credential on the engine's catalog before a
/// fetch, a download adds the header when its task is made, and a cover
/// load adds it here. That is what lets a token rotated between queueing
/// a download and running it simply be fresh, and keeps the secret out
/// of anything the transfer system persists.
public final class Http: Sendable {
    public let session: URLSession
    public let credentials: Credentials

    public init(credentials: Credentials, configuration: URLSessionConfiguration = .default) {
        self.credentials = credentials
        configuration.waitsForConnectivity = false
        session = URLSession(configuration: configuration)
    }

    /// The engine's transport, for catalogs. The engine sends plain GETs
    /// through it; the credential rides on the catalog handle.
    public var transport: HTTPTransport { .urlSession(session) }

    /// A request to `url` carrying the origin's credential, when one is
    /// held — what a cover or thumbnail loads through.
    public func request(_ url: URL) -> URLRequest {
        var request = URLRequest(url: url)
        if let authorization = credentials.authorization(for: url) {
            request.setValue(authorization, forHTTPHeaderField: "Authorization")
        }
        return request
    }

    /// Fetch bytes with the origin's credential attached. For images; a
    /// status outside 2xx is `nil`, not an error worth a sentence.
    public func bytes(_ url: URL) async -> Data? {
        guard let (data, response) = try? await session.data(for: request(url)),
            let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode)
        else { return nil }
        return data
    }
}
