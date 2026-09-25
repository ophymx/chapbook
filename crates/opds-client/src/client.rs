//! The OPDS flow: fetch/sniff/parse feeds, Basic auth with Authentication
//! Document surfacing, downloads, PSE page urls, OpenSearch. Behavior
//! contract: the crate's INTEROP.md.
//!
//! Everything here is protocol, not networking — bytes arrive through an
//! injected [`HttpClient`] (see `crate::http` for why).

use std::path::Path;

use crate::atom::parse_atom;
use crate::http::{header, settle, HeaderValue, HttpClient, HttpRequest, Request};
use crate::model::{AuthDocument, Feed, Link, MediaType};
use crate::opds2::{parse_opds2, parse_opds2_publication};
use crate::OpdsError;

pub struct OpdsClient {
    http: Box<dyn HttpClient>,
    /// Precomputed `Authorization` header value, scheme word included.
    ///
    /// Opaque on purpose: `Basic ...` today, and a `Bearer ...` token or a
    /// per-user API key is the same field. Nothing here branches on the
    /// scheme, so adding one is a caller-side change and not a change to
    /// this type — which is what keeps the scheme out of the eventual C
    /// ABI. See `chapbook_core::credential`.
    authorization: Option<String>,
}

impl OpdsClient {
    /// Build a client over a caller-supplied transport.
    pub fn new(http: impl HttpClient + 'static) -> Self {
        OpdsClient {
            http: Box::new(http),
            authorization: None,
        }
    }

    /// Build a client over an already-boxed transport, for callers that
    /// choose one at runtime.
    pub fn with_boxed_http(http: Box<dyn HttpClient>) -> Self {
        OpdsClient {
            http,
            authorization: None,
        }
    }

    /// Build a client over the bundled `ureq` transport — the desktop
    /// default, and the only constructor that pulls in a TLS stack.
    #[cfg(feature = "ureq")]
    pub fn with_ureq() -> Self {
        Self::new(crate::UreqHttp::new())
    }

    /// Set the `Authorization` header value sent with every request —
    /// `Basic dXNlcjpwdw==`, `Bearer eyJ...`, whatever the host's scheme
    /// produced. Sent verbatim; this crate never inspects it.
    ///
    /// This is the primitive, and [`set_basic_auth`](Self::set_basic_auth)
    /// is a convenience over it. A scheme that cannot be reduced to one
    /// header — per-request signing, a cookie session — belongs in the
    /// injected [`HttpClient`](crate::HttpClient) instead, which sees the
    /// whole request.
    ///
    /// Any request may 401 mid-flow; the caller prompts (using the
    /// Authentication Document when present) and retries after calling
    /// this.
    pub fn set_authorization(&mut self, value: impl Into<String>) {
        self.authorization = Some(value.into());
    }

    /// Set HTTP Basic credentials for this catalog.
    pub fn set_basic_auth(&mut self, username: &str, password: &str) {
        self.set_authorization(basic_authorization(username, password));
    }

    /// Drop any credentials, so the next request goes out unauthenticated.
    pub fn clear_authorization(&mut self) {
        self.authorization = None;
    }

    /// Fetch and parse a catalog feed (or standalone entry/publication).
    ///
    /// One media type per Accept header — wild servers negotiate by naive
    /// substring match and mis-handle q-values (interop doc §1). The
    /// response is sniffed by Content-Type, not by what we asked for.
    pub fn fetch(&self, url: &str) -> Result<Feed, OpdsError> {
        let (body, content_type) = self.get(url, "application/atom+xml")?;
        parse_payload(&body, &content_type, url)
    }

    /// Explicitly probe the OPDS 2.0 encoding of a catalog.
    pub fn fetch_opds2(&self, url: &str) -> Result<Feed, OpdsError> {
        let (body, content_type) = self.get(url, "application/opds+json")?;
        parse_payload(&body, &content_type, url)
    }

    /// Run a search: prefer the feed's templated/OpenSearch machinery.
    /// 1.x: `rel=search` points at an OpenSearch description document whose
    /// template carries `{searchTerms}`. 2.0: the search href is templated
    /// directly.
    pub fn search(&self, feed: &Feed, base_url: &str, query: &str) -> Result<Feed, OpdsError> {
        let link = feed
            .search()
            .ok_or_else(|| OpdsError::Parse("feed has no search link".into()))?;
        let is_opensearch = link
            .media_type
            .as_ref()
            .is_some_and(|t| t.essence == "application/opensearchdescription+xml");
        let template = if is_opensearch {
            let (body, _) = self.get(&link.href, "application/opensearchdescription+xml")?;
            opensearch_template(&body, &link.href)?
        } else {
            link.href.clone()
        };
        let url = expand_search_template(&template, query)?;
        let _ = base_url; // hrefs were already resolved at parse time
        self.fetch(&url)
    }

    /// Download an acquisition to `dest`, complete or not at all.
    ///
    /// Fetches through [`HttpClient::send`] and streams the body into
    /// a sibling `.part` file, syncs it, then renames — so a reader never
    /// opens a half-written book, and an interrupted download leaves
    /// nothing behind. No Range resume is assumed: an interrupted
    /// download restarts.
    ///
    /// **The transport does not get to override this.** Writing the file
    /// is this crate's job, and a transport that did it instead would be
    /// reimplementing one rename in every language a host is written in,
    /// each with its own chance of leaving a torn file behind.
    ///
    /// This blocks until the transfer settles, holding a thread for the
    /// whole of it, which is right for a desktop process and wrong for a
    /// transfer that has to outlive the app going to the background. For
    /// that, hand the job over instead of making the call:
    /// [`Entry::download_request`](crate::Entry::download_request)
    /// describes the fetch and the host performs it. The
    /// [`download`](crate::download) module has the trade in full.
    pub fn download(&self, url: &str, dest: &Path) -> Result<(), OpdsError> {
        let response = self
            .http
            .send(self.request(url, "*/*")?)
            .map_err(|e| OpdsError::Network(e.to_string()))?;
        let status = response.status().as_u16();
        if status == 401 {
            // No Authentication Document here: a download 401 is a
            // retry-with-credentials signal rather than a login prompt,
            // and the body on this path is not one.
            return Err(OpdsError::AuthRequired(None));
        }
        if !(200..300).contains(&status) {
            return Err(OpdsError::Http(status));
        }
        let mut body = response.into_body();
        let tmp = dest.with_extension("part");
        let copy = (|| -> std::io::Result<()> {
            let mut file = std::fs::File::create(&tmp)?;
            std::io::copy(&mut body, &mut file)?;
            file.sync_all()
        })();
        if let Err(e) = copy {
            let _ = std::fs::remove_file(&tmp);
            return Err(OpdsError::Network(format!("write {}: {e}", tmp.display())));
        }
        std::fs::rename(&tmp, dest).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            OpdsError::Network(format!("rename to {}: {e}", dest.display()))
        })?;
        Ok(())
    }

    /// Fetch one comic page from a PSE stream link (0-based page number;
    /// `pse:lastRead` is 1-based — do not mix them up). Returns the bytes
    /// and the response Content-Type, which is authoritative over the
    /// link's advisory `type`.
    pub fn fetch_pse_page(
        &self,
        stream: &Link,
        page_number: u32,
        max_width: Option<u32>,
    ) -> Result<(Vec<u8>, Option<String>), OpdsError> {
        let url = pse_page_url(stream, page_number, max_width);
        self.get(&url, "image/*")
    }

    /// The transport itself, for the one flow that is not a GET (see
    /// `crate::progression`). Crate-internal: the client owns how requests
    /// are assembled, and handing the transport out publicly would let a
    /// caller route around the credential.
    #[cfg(feature = "progression")]
    pub(crate) fn transport(&self) -> &dyn HttpClient {
        &*self.http
    }

    /// Assemble a GET: one Accept media type, no q-values (interop doc
    /// §1), plus credentials when the caller has set them. A flow that
    /// writes changes the method and adds a body on what comes back.
    ///
    /// `Err` is a URL the `http` crate will not carry — a catalog href
    /// with an unencoded space, say. That was a transport error before
    /// too (every real client parses the URL); now it is the same error
    /// from every transport, and named.
    pub(crate) fn request(&self, url: &str, accept: &str) -> Result<HttpRequest, OpdsError> {
        let mut builder = Request::get(url).header(header::ACCEPT, accept);
        if let Some(auth) = &self.authorization {
            // `from_bytes` rather than a `&str`: a credential is opaque and
            // may carry obs-text a strict parse would refuse.
            let value = HeaderValue::from_bytes(auth.as_bytes())
                .map_err(|e| OpdsError::Network(format!("authorization header: {e}")))?;
            builder = builder.header(header::AUTHORIZATION, value);
        }
        builder
            .body(Vec::new())
            .map_err(|e| OpdsError::Network(format!("cannot request {url}: {e}")))
    }

    pub(crate) fn get(
        &self,
        url: &str,
        accept: &str,
    ) -> Result<(Vec<u8>, Option<String>), OpdsError> {
        let response = self
            .http
            .send(self.request(url, accept)?)
            .map_err(|e| OpdsError::Network(e.to_string()))?;
        let settled =
            settle(response).map_err(|e| OpdsError::Network(format!("read body: {e}")))?;
        let content_type = settled.content_type().map(str::to_string);

        if settled.status == 401 {
            let auth_doc = content_type
                .as_deref()
                .filter(|t| t.contains("opds-authentication"))
                .and_then(|_| serde_json::from_slice::<AuthDocument>(&settled.body).ok());
            return Err(OpdsError::AuthRequired(auth_doc.map(Box::new)));
        }
        if !(200..300).contains(&settled.status) {
            return Err(OpdsError::Http(settled.status));
        }
        Ok((settled.body, content_type))
    }
}

/// Substitute a PSE stream template. `{pageNumber}` is 0-based;
/// `{maxWidth}` is requested but servers may ignore it.
pub fn pse_page_url(stream: &Link, page_number: u32, max_width: Option<u32>) -> String {
    stream
        .href
        .replace("{pageNumber}", &page_number.to_string())
        .replace("{maxWidth}", &max_width.unwrap_or(1600).to_string())
}

fn parse_payload(body: &[u8], content_type: &Option<String>, url: &str) -> Result<Feed, OpdsError> {
    let essence = content_type
        .as_deref()
        .map(MediaType::parse)
        .map(|t| t.essence)
        .unwrap_or_default();
    if essence.contains("json") {
        // A publication document has `metadata` but no feed-shaped members;
        // try feed first, fall back to publication.
        parse_opds2(body, url).or_else(|_| parse_opds2_publication(body, url))
    } else if essence.contains("xml") || essence.is_empty() {
        parse_atom(body, url)
    } else {
        Err(OpdsError::Parse(format!(
            "unexpected content type {essence:?} for catalog at {url}"
        )))
    }
}

/// Extract the Atom-result template from an OpenSearch description document.
pub fn opensearch_template(xml: &[u8], base_url: &str) -> Result<String, OpdsError> {
    let mut reader = quick_xml::Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut fallback: Option<String> = None;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(quick_xml::events::Event::Empty(e)) | Ok(quick_xml::events::Event::Start(e))
                if e.local_name().as_ref() == "Url" =>
            {
                let mut template = None;
                let mut kind = None;
                for attr in e.attributes().flatten() {
                    let key = attr.key.local_name();
                    let value = attr
                        .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                        .map(|v| v.to_string())
                        .unwrap_or_default();
                    match key.as_ref() {
                        "template" => template = Some(value),
                        "type" => kind = Some(value),
                        _ => {}
                    }
                }
                if let Some(template) = template {
                    let resolved = crate::href::resolve_url(base_url, &template);
                    let is_atom = kind.as_deref().is_some_and(|k| k.contains("atom"));
                    if is_atom {
                        return Ok(resolved);
                    }
                    fallback.get_or_insert(resolved);
                }
            }
            Ok(quick_xml::events::Event::Eof) => break,
            Err(e) => return Err(OpdsError::Parse(format!("opensearch: {e}"))),
            _ => {}
        }
        buf.clear();
    }
    fallback.ok_or_else(|| OpdsError::Parse("opensearch document has no Url template".into()))
}

/// Fill an OpenSearch URL template (OpenSearch 1.1 §4.2) with a query.
///
/// Only `{searchTerms}` carries the caller's query. Everything else in a
/// template is a parameter this crate does not supply, and the spec is
/// specific about what that means: an **optional** parameter — one whose
/// name ends in `?` — is replaced with the empty string, and a
/// **required** one the client cannot fill makes the template unusable.
///
/// Leaving an unfilled parameter in the URL is the one thing that must not
/// happen, and is what this function exists to prevent. A template like
/// `?q={searchTerms}&author={atom:author?}` is ordinary — Calibre-Web,
/// COPS and Kavita all emit optional refinement parameters — and sending
/// the literal `author={atom:author?}` is read by the server as a filter
/// on the eleven-character author name `{atom:author?}`. It answers 200
/// with an empty feed, so the failure looks exactly like a search that
/// found nothing.
///
/// Parameter names are matched on their local part, so the namespaced
/// `{os:searchTerms}` some catalogs write is the same parameter as
/// `{searchTerms}`. The spec-defaulted parameters (`startIndex`,
/// `startPage`, `language`, `inputEncoding`, `outputEncoding`) get their
/// documented defaults when required; `count` has no client-side default
/// (the spec leaves it to the server), so a template that requires one
/// is refused.
pub fn expand_search_template(template: &str, query: &str) -> Result<String, OpdsError> {
    let encoded = urlencode(query);
    let mut out = String::with_capacity(template.len() + encoded.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let Some(close) = rest[open..].find('}').map(|i| open + i) else {
            break; // an unbalanced brace is literal text, not a parameter
        };
        let name = &rest[open + 1..close];
        out.push_str(&rest[..open]);
        let (name, optional) = match name.strip_suffix('?') {
            Some(stripped) => (stripped, true),
            None => (name, false),
        };
        let local = name.rsplit(':').next().unwrap_or(name);
        match local {
            "searchTerms" => out.push_str(&encoded),
            "startIndex" | "startPage" if !optional => out.push('1'),
            "language" if !optional => out.push('*'),
            "inputEncoding" | "outputEncoding" if !optional => out.push_str("UTF-8"),
            _ if optional => {}
            _ => {
                return Err(OpdsError::Parse(format!(
                    "search template requires a parameter this client cannot supply: {{{name}}}"
                )))
            }
        }
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// The `Authorization` value for HTTP Basic, ready to hand to
/// [`OpdsClient::set_authorization`] or to any other client that takes an
/// opaque credential.
///
/// Public because the credential is opaque by design and more than one
/// protocol here needs one: a Web Annotation container is reached with the
/// same header as a catalog, and hand-rolling base64 a second time to say
/// so would be silly.
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
