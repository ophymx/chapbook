//! A Web Annotation Protocol container: create, read, update, delete, and
//! walk the collection.
//!
//! # Concurrency is entity tags, and they are not optional
//!
//! Every annotation carries an `ETag`. An update or delete sends it back as
//! `If-Match`, and a container answers **412** when the copy on the server
//! moved since. That is not an error path to paper over — it is two devices
//! editing the same highlight, which is the ordinary case sync exists for.
//! So [`ContainerError::Conflict`] carries the server's current copy where
//! the server sent one, and the caller resolves it with something better
//! than last-write-wins.
//!
//! # The transport
//!
//! [`HttpClient`], this crate's own declaration of what it needs, over
//! the `http` crate's request and response — so the closure a host wrote
//! for its catalog client serves here unchanged. A transport that cannot
//! write must refuse rather than pretend; see that trait's docs for why a
//! silently dropped write is the worst outcome available.

use std::io::Read;

use crate::http::{
    header, HeaderMap, HeaderValue, HttpClient, HttpRequest, HttpResponse, Method, Request,
};
use crate::model::{Annotation, MEDIA_TYPE};

/// What went wrong, in the vocabulary the caller has to act on.
#[derive(Debug)]
pub enum ContainerError {
    /// The request never completed.
    Network(String),
    /// A body that is not the annotation it claimed to be.
    Parse(String),
    /// 401/403. The container is behind credentials this client does not
    /// have; the caller sets an `Authorization` and retries.
    Unauthorized,
    /// 404 — including an IRI that was deleted. A tombstoned IRI is never
    /// re-minted, so this is permanent and the local record should stop
    /// pointing at it.
    Gone,
    /// 412: the `If-Match` did not hold. `current` is the server's copy
    /// when it sent one, so a caller can merge instead of guessing.
    Conflict { current: Option<Box<Annotation>> },
    /// Any other non-2xx.
    Http(u16),
}

impl std::fmt::Display for ContainerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ContainerError::Network(message) => write!(f, "network error: {message}"),
            ContainerError::Parse(message) => write!(f, "parse error: {message}"),
            ContainerError::Unauthorized => f.write_str("authentication required"),
            ContainerError::Gone => f.write_str("annotation does not exist"),
            ContainerError::Conflict { .. } => {
                f.write_str("the annotation changed on the server since it was read")
            }
            ContainerError::Http(status) => write!(f, "HTTP status {status}"),
        }
    }
}

impl std::error::Error for ContainerError {}

/// Everything a walk of a container found, and whether that was all of it.
///
/// The completeness is the point of the type. A caller that treats a
/// listing as the whole container is asking "what is in it", and a walk
/// that stopped at `limit` cannot answer that — absence from a short
/// listing means nothing at all, while absence from a complete one means
/// the annotation is gone. Returning a bare `Vec` let those two be
/// confused silently, and they are not the same fact.
#[derive(Debug, Clone, PartialEq)]
pub struct Listing {
    pub items: Vec<StoredAnnotation>,
    /// Whether the `next` chain was followed to its end. `false` when
    /// `limit` stopped the walk, or when a container pointed a page at
    /// itself.
    pub complete: bool,
}

/// An annotation as the container holds it: the document, its IRI, and the
/// entity tag that makes the next write safe.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredAnnotation {
    /// The server-minted IRI. This is the sync identity — persist it.
    pub iri: String,
    /// Send back as `If-Match`. `None` from a container that omitted one,
    /// in which case a write is unguarded and races.
    pub etag: Option<String>,
    pub annotation: Annotation,
}

/// A response with its body already drained — what every request here
/// hands back, since a response body is a reader and each caller wants
/// the same three things out of it.
struct RawResponse {
    status: u16,
    headers: HeaderMap,
    body: Vec<u8>,
}

/// One page of a container listing.
#[derive(Debug, Clone)]
pub struct AnnotationPage {
    pub items: Vec<StoredAnnotation>,
    /// The `next` page IRI, when the container paged the collection.
    pub next: Option<String>,
    /// `total`, when the container stated one.
    pub total: Option<u64>,
}

/// A client for one container.
pub struct AnnotationContainer {
    http: Box<dyn HttpClient>,
    authorization: Option<String>,
}

impl AnnotationContainer {
    pub fn new(http: impl HttpClient + 'static) -> Self {
        AnnotationContainer {
            http: Box::new(http),
            authorization: None,
        }
    }

    pub fn with_boxed_http(http: Box<dyn HttpClient>) -> Self {
        AnnotationContainer {
            http,
            authorization: None,
        }
    }

    /// The bundled desktop transport.
    #[cfg(feature = "ureq")]
    pub fn with_ureq() -> Self {
        Self::new(crate::http::UreqHttp::new())
    }

    /// An opaque `Authorization` header value — the same contract as the
    /// catalog client's: the scheme is never modelled, because a bearer
    /// token and a Basic credential are the same field.
    pub fn set_authorization(&mut self, value: impl Into<String>) {
        self.authorization = Some(value.into());
    }

    /// HTTP Basic, the convenience form of the same opaque header.
    pub fn set_basic_auth(&mut self, username: &str, password: &str) {
        self.set_authorization(crate::http::basic_authorization(username, password));
    }

    pub fn clear_authorization(&mut self) {
        self.authorization = None;
    }

    /// Create an annotation in the container at `container_url`.
    ///
    /// The IRI comes back in `Location`; a container that also returns the
    /// stored document is believed over what we sent, since it may have
    /// normalized or added `created`.
    pub fn create(
        &self,
        container_url: &str,
        annotation: &Annotation,
    ) -> Result<StoredAnnotation, ContainerError> {
        let body = serde_json::to_vec(annotation)
            .map_err(|e| ContainerError::Parse(format!("serialize annotation: {e}")))?;
        let request = self.request(Method::POST, container_url, &[], body)?;
        let response = self.send(request)?;
        let (status, headers, body) = (response.status, response.headers, response.body);
        if !(200..300).contains(&status) {
            return Err(self.classify(status, &body));
        }
        let iri = header(&headers, header::LOCATION)
            .or_else(|| {
                serde_json::from_slice::<Annotation>(&body)
                    .ok()
                    .and_then(|a| a.id)
            })
            .ok_or_else(|| {
                ContainerError::Parse(
                    "the container created an annotation without saying where".into(),
                )
            })?;
        Ok(StoredAnnotation {
            iri: resolve(container_url, &iri),
            etag: header(&headers, header::ETAG),
            annotation: parse_or(&body, annotation),
        })
    }

    /// Read one annotation, with the entity tag needed to write it back.
    pub fn get(&self, iri: &str) -> Result<StoredAnnotation, ContainerError> {
        let request = self.request(Method::GET, iri, &[], Vec::new())?;
        let response = self.send(request)?;
        let (status, headers, body) = (response.status, response.headers, response.body);
        let etag = header(&headers, header::ETAG);
        if !(200..300).contains(&status) {
            return Err(self.classify(status, &body));
        }
        let annotation: Annotation = serde_json::from_slice(&body)
            .map_err(|e| ContainerError::Parse(format!("annotation: {e}")))?;
        Ok(StoredAnnotation {
            iri: annotation.id.clone().unwrap_or_else(|| iri.to_string()),
            etag,
            annotation,
        })
    }

    /// Replace an annotation, guarded by the entity tag it was read with.
    ///
    /// Passing `etag: None` writes unguarded and will clobber a concurrent
    /// edit — do it only when the container gave no tag to begin with.
    pub fn update(
        &self,
        iri: &str,
        annotation: &Annotation,
        etag: Option<&str>,
    ) -> Result<StoredAnnotation, ContainerError> {
        let body = serde_json::to_vec(annotation)
            .map_err(|e| ContainerError::Parse(format!("serialize annotation: {e}")))?;
        let guard: &[(_, &str)] = match etag {
            Some(etag) => &[(header::IF_MATCH, etag)],
            None => &[],
        };
        let request = self.request(Method::PUT, iri, guard, body)?;
        let response = self.send(request)?;
        let (status, headers, body) = (response.status, response.headers, response.body);
        if !(200..300).contains(&status) {
            return Err(self.classify(status, &body));
        }
        Ok(StoredAnnotation {
            iri: iri.to_string(),
            etag: header(&headers, header::ETAG),
            annotation: parse_or(&body, annotation),
        })
    }

    /// Delete an annotation. The IRI is tombstoned by a conforming
    /// container and never re-minted, so this is final.
    ///
    /// A 404 is [`Ok`]: something else already deleted it, and the caller's
    /// intent — that it not be there — holds.
    pub fn delete(&self, iri: &str, etag: Option<&str>) -> Result<(), ContainerError> {
        let guard: &[(_, &str)] = match etag {
            Some(etag) => &[(header::IF_MATCH, etag)],
            None => &[],
        };
        let request = self.request(Method::DELETE, iri, guard, Vec::new())?;
        let response = self.send(request)?;
        if (200..300).contains(&response.status) || response.status == 404 {
            return Ok(());
        }
        Err(self.classify(response.status, &response.body))
    }

    /// One page of the container's collection.
    ///
    /// Pass the container IRI to start; follow [`AnnotationPage::next`]
    /// until it is `None`. A container may answer the container IRI with a
    /// collection whose items live on separate pages, so the first
    /// response's `first` is followed automatically.
    pub fn page(&self, url: &str) -> Result<AnnotationPage, ContainerError> {
        let value = self.fetch_json(url)?;
        // A `Collection` states where its items are; an `AnnotationPage`
        // has them. Follow one hop rather than making the caller know.
        let value = match items_of(&value) {
            Some(_) => value,
            None => match value.get("first") {
                Some(serde_json::Value::String(first)) => self.fetch_json(&resolve(url, first))?,
                // An embedded first page.
                Some(embedded) => embedded.clone(),
                None => value,
            },
        };
        let mut items = Vec::new();
        for item in items_of(&value).map(Vec::as_slice).unwrap_or_default() {
            match item {
                // A container that serves IRIs anyway, despite the
                // `Prefer` on the request. Fetching each is slow, and
                // slow beats reporting an empty container.
                serde_json::Value::String(iri) => {
                    items.push(self.get(&resolve(url, iri))?);
                }
                _ => {
                    let Ok(annotation) = serde_json::from_value::<Annotation>(item.clone()) else {
                        continue;
                    };
                    let Some(iri) = annotation.id.clone() else {
                        continue;
                    };
                    items.push(StoredAnnotation {
                        iri,
                        etag: None,
                        annotation,
                    });
                }
            }
        }
        Ok(AnnotationPage {
            items,
            next: value
                .get("next")
                .and_then(|n| n.as_str().map(|s| resolve(url, s)))
                .or_else(|| {
                    value
                        .get("next")
                        .and_then(|n| n.get("id"))
                        .and_then(|id| id.as_str())
                        .map(|s| resolve(url, s))
                }),
            total: value.get("total").and_then(serde_json::Value::as_u64),
        })
    }

    /// Every annotation in the container, following pages to the end —
    /// and whether the end was reached.
    ///
    /// `limit` caps how many pages are walked, because a container is
    /// somebody else's and a runaway `next` chain should not be an
    /// unbounded loop. `None` means no cap. Hitting the cap is reported
    /// rather than silent: see [`Listing::complete`] for why a caller has
    /// to know.
    pub fn all(
        &self,
        container_url: &str,
        limit: Option<usize>,
    ) -> Result<Listing, ContainerError> {
        let mut items = Vec::new();
        let mut url = container_url.to_string();
        let mut seen = 0usize;
        let complete = loop {
            let page = self.page(&url)?;
            items.extend(page.items);
            seen += 1;
            match page.next {
                Some(next) if limit.is_none_or(|limit| seen < limit) => {
                    // A container that points a page at itself would spin
                    // forever otherwise. Not an end reached, either: what
                    // lies past it was never seen.
                    if next == url {
                        break false;
                    }
                    url = next;
                }
                // More pages than the cap allows.
                Some(_) => break false,
                None => break true,
            }
        };
        Ok(Listing { items, complete })
    }

    // ---- plumbing ----

    /// Container reads ask for the annotations themselves.
    ///
    /// A container may serve its items as bare IRIs instead of
    /// descriptions — the protocol lets it choose, and asking is how a
    /// client says which it can use. Without this a listing can come back
    /// as a page of strings, which is a page of zero annotations to
    /// anything expecting objects, and looks exactly like an empty
    /// container. [`AnnotationContainer::page`] fetches them one by one if
    /// it happens anyway; this is what stops it being needed.
    fn container_request(&self, url: &str) -> Result<HttpRequest, ContainerError> {
        const PREFER: &str = "return=representation; \
                              include=\"http://www.w3.org/ns/oa#PreferContainedDescriptions\"";
        let prefer = http::HeaderName::from_static("prefer");
        self.request(Method::GET, url, &[(prefer, PREFER)], Vec::new())
    }

    /// Assemble one request: `Accept` for the annotation media type, the
    /// credential when one is set, `Content-Type` when there is a body,
    /// and whatever else the flow adds.
    ///
    /// `Err` is an IRI the `http` crate will not carry. A container that
    /// minted one is broken in a way no transport could fix.
    fn request(
        &self,
        method: Method,
        url: &str,
        extra: &[(http::HeaderName, &str)],
        body: Vec<u8>,
    ) -> Result<HttpRequest, ContainerError> {
        let mut builder = Request::builder()
            .method(method)
            .uri(url)
            .header(header::ACCEPT, MEDIA_TYPE);
        if !body.is_empty() {
            builder = builder.header(header::CONTENT_TYPE, MEDIA_TYPE);
        }
        if let Some(authorization) = &self.authorization {
            // `from_bytes`: the credential is opaque and may carry obs-text
            // a strict parse would refuse.
            let value = HeaderValue::from_bytes(authorization.as_bytes())
                .map_err(|e| ContainerError::Network(format!("authorization header: {e}")))?;
            builder = builder.header(header::AUTHORIZATION, value);
        }
        for (name, value) in extra {
            builder = builder.header(name.clone(), *value);
        }
        builder
            .body(body)
            .map_err(|e| ContainerError::Network(format!("cannot request {url}: {e}")))
    }

    fn fetch_json(&self, url: &str) -> Result<serde_json::Value, ContainerError> {
        let response = self.send(self.container_request(url)?)?;
        if !(200..300).contains(&response.status) {
            return Err(self.classify(response.status, &response.body));
        }
        serde_json::from_slice(&response.body)
            .map_err(|e| ContainerError::Parse(format!("container: {e}")))
    }

    fn send(&self, request: HttpRequest) -> Result<RawResponse, ContainerError> {
        let response: HttpResponse = self
            .http
            .send(request)
            .map_err(|e| ContainerError::Network(e.to_string()))?;
        let (parts, mut reader) = response.into_parts();
        let mut body = Vec::new();
        reader
            .read_to_end(&mut body)
            .map_err(|e| ContainerError::Network(format!("read body: {e}")))?;
        Ok(RawResponse {
            status: parts.status.as_u16(),
            headers: parts.headers,
            body,
        })
    }

    fn classify(&self, status: u16, body: &[u8]) -> ContainerError {
        match status {
            401 | 403 => ContainerError::Unauthorized,
            404 | 410 => ContainerError::Gone,
            412 => ContainerError::Conflict {
                current: serde_json::from_slice::<Annotation>(body)
                    .ok()
                    .map(Box::new),
            },
            other => ContainerError::Http(other),
        }
    }
}

fn items_of(value: &serde_json::Value) -> Option<&Vec<serde_json::Value>> {
    value
        .get("items")
        .or_else(|| value.get("first").and_then(|f| f.get("items")))
        .and_then(serde_json::Value::as_array)
}

fn parse_or(body: &[u8], sent: &Annotation) -> Annotation {
    serde_json::from_slice(body).unwrap_or_else(|_| sent.clone())
}

/// A response header as text. `None` when absent or not text — a value
/// this crate cannot use is read as not there.
fn header(headers: &HeaderMap, name: http::HeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

/// Resolve an IRI a container handed back against the URL it came from.
/// Containers are allowed relative `Location` and `next` values; a value
/// neither side can parse is passed through as given, which is at least
/// the same string the container will recognise.
fn resolve(base: &str, href: &str) -> String {
    match url::Url::parse(base).and_then(|base| base.join(href)) {
        Ok(url) => url.to_string(),
        Err(_) => href.to_string(),
    }
}
