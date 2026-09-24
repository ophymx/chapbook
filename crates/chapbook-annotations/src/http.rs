//! The transport seam: what this crate needs from HTTP, declared here and
//! implemented by whoever calls it.
//!
//! A Web Annotation container is reached with the same networking a
//! catalog is, and a host implements that networking once. This crate
//! therefore names no HTTP library of its own and no sibling crate's
//! either: it declares the one trait it needs, over the `http` crate's
//! [`Request`] and [`Response`] — the types every transport-agnostic Rust
//! HTTP library shares. Any closure from one to the other satisfies it,
//! and the same closure satisfies an OPDS client's trait too, because both
//! are the same function type. That is how one transport serves two
//! independent crates without either depending on the other.
//!
//! `UreqHttp` is one implementation, behind the default `ureq` feature.
//! Turning it off drops `ureq`, `rustls` and the bundled root store, and
//! the crate still does everything except open a socket.

use std::fmt;
use std::io::Read;
use std::sync::Arc;

pub use ::http::{
    header, HeaderMap, HeaderName, HeaderValue, Method, Request, Response, StatusCode, Uri,
};

/// A response body, still unread.
pub type Body = Box<dyn Read + Send>;

/// One outgoing request, method and body included: a container is read
/// with GET and written with POST, PUT and DELETE, and all four go through
/// the one door.
pub type HttpRequest = Request<Vec<u8>>;

/// One response, with its body still unread.
///
/// The headers matter here more than for a catalog: a container carries
/// its whole concurrency story in `ETag` and says where it put a new
/// annotation in `Location`. A transport that discards them makes safe
/// concurrent editing impossible, and the failure looks like sync quietly
/// forgetting marks.
pub type HttpResponse = Response<Body>;

/// A transport failure: the request never produced a response. A server
/// that answered 404, 412 or 500 did *not* fail — that is an
/// [`HttpResponse`], and the status is the protocol talking.
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
/// Blocking by contract: sync runs on a worker thread of its own and an
/// async runtime here would infect the caller. Implementations that wrap
/// an async or callback-driven stack should block internally.
///
/// What an implementation must do: return 4xx and 5xx as responses, not
/// errors; follow redirects; send the headers and the method as given —
/// a transport that only reads refuses a write with an error rather than
/// dropping it, since a dropped write is indistinguishable from a mark
/// that synced; report the response headers, `ETag` and `Location` above
/// all; and never retry on its own.
pub trait HttpClient: Send + Sync {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, HttpError>;
}

/// A function is a transport, whatever error type it reports in.
impl<F, E> HttpClient for F
where
    F: Fn(HttpRequest) -> Result<HttpResponse, E> + Send + Sync,
    E: fmt::Display,
{
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        self(request).map_err(HttpError::new)
    }
}

/// A shared transport is a transport: a host owns one of these.
impl<T: HttpClient + ?Sized> HttpClient for Arc<T> {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        (**self).send(request)
    }
}

/// [`HttpClient`] over a blocking `ureq` agent: the desktop answer, and
/// nearly nothing, since `ureq` 3 speaks the `http` crate's types itself.
#[cfg(feature = "ureq")]
pub struct UreqHttp {
    agent: ureq::Agent,
}

#[cfg(feature = "ureq")]
impl Default for UreqHttp {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "ureq")]
impl UreqHttp {
    pub fn new() -> Self {
        let config = ureq::Agent::config_builder()
            // 4xx come back as responses: a 412 is the protocol's answer,
            // not a failure.
            .http_status_as_error(false)
            .build();
        UreqHttp {
            agent: config.into(),
        }
    }

    /// Wrap an agent the caller configured. `http_status_as_error(false)`
    /// is required of it, per [`HttpClient`]'s contract.
    pub fn with_agent(agent: ureq::Agent) -> Self {
        UreqHttp { agent }
    }
}

#[cfg(feature = "ureq")]
impl HttpClient for UreqHttp {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        // An empty body is no body: a GET or DELETE must not carry
        // `Content-Length: 0`.
        let response = if request.body().is_empty() {
            self.agent.run(request.map(|_| ()))
        } else {
            self.agent.run(request)
        }
        .map_err(HttpError::new)?;
        Ok(response.map(|body| Box::new(body.into_reader()) as Body))
    }
}

/// The `Authorization` value for HTTP Basic, ready for
/// [`AnnotationContainer::set_authorization`](crate::AnnotationContainer::set_authorization).
///
/// Its own twenty lines rather than a sibling crate's, because the seam
/// exists so this crate names none.
pub fn basic_authorization(username: &str, password: &str) -> String {
    format!(
        "Basic {}",
        base64(format!("{username}:{password}").as_bytes())
    )
}

fn base64(input: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::basic_authorization;

    #[test]
    fn basic_is_the_rfc_example() {
        assert_eq!(
            basic_authorization("Aladdin", "open sesame"),
            "Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ=="
        );
    }
}
