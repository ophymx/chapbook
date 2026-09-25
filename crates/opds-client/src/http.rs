//! The transport seam: everything this crate knows about *how* bytes are
//! fetched, which is deliberately almost nothing.
//!
//! OPDS is a format, not a networking stack. Bundling one costs more than it
//! saves as soon as the caller is not a desktop process: an iOS app that
//! reaches the network outside `URLSession` gives up background transfer,
//! the system trust store, App Transport Security, per-app VPN and the
//! cellular-data toggle; an Android app gives up WorkManager; a WASM build
//! has no sockets at all and must use `fetch`. So the caller supplies an
//! [`HttpClient`] and the crate supplies the OPDS.
//!
//! `UreqHttp` is one implementation, behind the default `ureq`
//! feature. Turning that feature off drops `ureq`, `rustls` and the bundled
//! root store entirely, and the crate still does everything except open a
//! socket.
//!
//! # The types are the `http` crate's
//!
//! A request is [`http::Request`] and a response is [`http::Response`],
//! not shapes of this crate's own. That is the convention every
//! transport-agnostic Rust HTTP library has settled on, and it is what
//! lets two such libraries share one transport without either naming the
//! other: a closure that maps an `http` request to an `http` response
//! satisfies this trait *and* any sibling crate's, because both are the
//! same function type. [`HttpRequest`] and [`HttpResponse`] are aliases,
//! kept so the seam reads as one thing at the call site.
//!
//! The request body is bytes: every write this crate makes is a small
//! JSON document. The response body is a reader, because an acquisition
//! download streams through it to disk and a book does not belong in
//! memory twice.

use std::fmt;
use std::io::Read;
use std::sync::Arc;

pub use ::http::{
    header, HeaderMap, HeaderName, HeaderValue, Method, Request, Response, StatusCode, Uri,
};

/// A response body, still unread.
pub type Body = Box<dyn Read + Send>;

/// One outgoing request. The method is in it — GET for everything a
/// catalog browser does, PUT for a position, and whatever a sibling
/// protocol needs — so a transport implements one door, not one per verb.
///
/// Headers must be sent as given: `Accept` carries exactly one media type
/// and never a q-value, because real catalog servers negotiate by naive
/// substring match (interop doc §1). Rewriting or merging headers will
/// break servers in the wild.
///
/// Owned rather than borrowed on purpose: an implementation is as likely
/// to be a thin shim over a foreign runtime — `URLSession`, OkHttp,
/// `fetch` — as it is to be Rust all the way down, and lifetimes do not
/// survive that trip.
pub type HttpRequest = Request<Vec<u8>>;

/// One response, with its body still unread.
///
/// The status is the real one, 4xx and 5xx included — see
/// [`HttpClient::send`]. `Content-Type` is authoritative over whatever the
/// caller asked for or a link advertised. The other headers matter to the
/// flows that write: a Web Annotation container carries its whole
/// concurrency story in `ETag` and says where it put a new annotation in
/// `Location`, so a transport that discards headers makes safe concurrent
/// editing impossible. A transport may pass all headers or only the ones
/// it can cheaply enumerate; a missing header is read as absent, never as
/// empty.
pub type HttpResponse = Response<Body>;

/// A transport failure: the request never produced a response. A server that
/// answered with 404 or 500 did *not* fail — that is an [`HttpResponse`].
#[derive(Debug)]
pub struct HttpError(String);

impl HttpError {
    pub fn new(message: impl fmt::Display) -> Self {
        HttpError(message.to_string())
    }
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for HttpError {}

impl From<::http::Error> for HttpError {
    fn from(error: ::http::Error) -> Self {
        HttpError::new(error)
    }
}

/// A blocking HTTP transport.
///
/// Blocking by contract, not by accident: OPDS page streaming backs a
/// `Publication` whose reads happen on a loader thread, and an async
/// runtime here would infect that. Implementations that wrap an async or
/// callback-driven stack should block internally.
///
/// `Send + Sync` because a streamed comic is shared across threads.
///
/// Any closure from [`HttpRequest`] to [`HttpResponse`] is one of these,
/// so a host that already owns a client injects a lambda rather than
/// naming a type; a shared [`Arc`] of one is one too, because a host that
/// owns its networking owns *one* of it.
///
/// ## What an implementation must do
///
/// - **Return 4xx and 5xx as responses, not errors.** A 401's body is the
///   OPDS Authentication Document, which is what lets a shell put up a
///   native login dialog instead of a generic failure. A transport that
///   collapses error statuses makes that impossible.
/// - **Follow redirects, including cross-host ones.** Catalogs relocate
///   acquisitions onto CDNs.
/// - **Send the given headers unaltered**, per [`HttpRequest`].
/// - **Send the method it is given.** A transport that only reads — a
///   catalog browser, a WASM build that only downloads — refuses anything
///   but GET with an error rather than pretending: a write that vanishes
///   looks to a reader exactly like a position that syncs and is never
///   stored.
/// - **Not retry on its own.** Auth retry is the caller's flow.
///
/// ## What belongs here rather than in the credential
///
/// [`OpdsClient::set_authorization`](crate::OpdsClient::set_authorization)
/// covers every scheme that reduces to one constant header — Basic, bearer
/// tokens, per-user API keys. The two that do not are **per-request
/// signing**, where the value depends on the method, path or body, and
/// **cookie sessions**, where the state is a jar rather than a header. Both
/// need to see the whole request, so both are this trait's job, not the
/// credential store's. Keeping that line means the credential stays opaque
/// bytes all the way out to a host.
pub trait HttpClient: Send + Sync {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, HttpError>;
}

/// A function is a transport.
///
/// This is the impl that makes the seam cheap for a host: whatever error
/// its own client produces is carried as the message, so no host has to
/// learn this crate's error type to hand a request to `URLSession`.
impl<F, E> HttpClient for F
where
    F: Fn(HttpRequest) -> Result<HttpResponse, E> + Send + Sync,
    E: fmt::Display,
{
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        self(request).map_err(HttpError::new)
    }
}

/// A shared transport is a transport.
///
/// The reason this exists rather than being an inconvenience the caller
/// works around: a host that owns its networking owns *one* of it. An iOS
/// app has a single background `URLSession` whose whole value is that
/// transfers outlive the process; handing out clones of it is wrong and
/// handing out a second one is worse. Meanwhile chapbook builds a fresh
/// `OpdsClient` per authentication attempt, so without this the retry path
/// would have to construct a second transport to re-send one request.
impl<T: HttpClient + ?Sized> HttpClient for Arc<T> {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        (**self).send(request)
    }
}

/// A response header as text, by name. `None` when the header is absent
/// or its bytes are not text — a value this crate has no way to use is
/// read as not there, which beats a lossy conversion.
pub fn header_str(headers: &HeaderMap, name: HeaderName) -> Option<&str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

/// A response with its body drained: what every flow here wants, since
/// the body is a reader and each caller wants the same three things out
/// of it.
pub struct Settled {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl Settled {
    /// The `Content-Type` verbatim, parameters and all.
    pub fn content_type(&self) -> Option<&str> {
        header_str(&self.headers, header::CONTENT_TYPE)
    }
}

/// Read a response to the end.
pub fn settle(response: HttpResponse) -> std::io::Result<Settled> {
    let (parts, mut reader) = response.into_parts();
    let mut body = Vec::new();
    reader.read_to_end(&mut body)?;
    Ok(Settled {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body,
    })
}
