//! An OPDS 1.2 (Atom) and 2.0 (JSON) catalog client: browse, search,
//! paginate, authenticate, and download from ebook catalogs — self-hosted
//! catalog and comic servers, library-lending stacks.
//!
//! **Bring your own HTTP.** The crate is format and protocol knowledge; it
//! opens no sockets of its own. A request is an [`http::Request`] and a
//! response an [`http::Response`] — the `http` crate's types, which every
//! transport-agnostic Rust HTTP library shares — and any function from
//! one to the other is an [`HttpClient`]. Hand the request to whatever
//! owns networking on your platform and the crate does the rest:
//!
//! ```no_run
//! use opds_client::http::{HttpRequest, HttpResponse};
//! use opds_client::OpdsClient;
//!
//! fn host_http(request: HttpRequest) -> Result<HttpResponse, std::io::Error> {
//!     // Hand request.uri(), request.method() and request.headers() to
//!     // URLSession, OkHttp, fetch, or whatever else owns networking here.
//! #   unimplemented!()
//! }
//!
//! let client = OpdsClient::new(host_http);
//! let feed = client.fetch("https://catalog.example.com/opds/")?;
//! for entry in &feed.entries {
//!     println!("{}", entry.title);
//! }
//! # Ok::<(), opds_client::OpdsError>(())
//! ```
//!
//! A type that holds state — a session, a connection pool — implements
//! the trait's one method, `send`, instead.
//!
//! On desktop there is no need to write one: `OpdsClient::with_ureq()`
//! supplies `UreqHttp`, behind the default `ureq` feature. With
//! `default-features = false` the crate has no TLS stack, no bundled root
//! store, and no opinion about how bytes are fetched — which is what makes
//! it usable from an iOS app that must go through `URLSession` for
//! background transfer and system trust, an Android app that wants
//! `WorkManager`, or a WASM build that has only `fetch`. See [`http`] for
//! the contract an implementation has to honor.
//!
//! # Interop
//!
//! Every behavior here is a response to something observed on a real
//! server, and the load-bearing decisions are worth stating so nobody
//! re-litigates them:
//!
//! - **OPDS 1.2 Atom is the canonical dialect**; 2.0 JSON (serde) is a
//!   secondary parser kept honest by fixtures. The two encodings are not
//!   informationally equivalent — never assume a field survives a version
//!   switch.
//! - **Feeds are parsed at the XML level with namespace-aware `quick-xml`**,
//!   NOT `atom_syndication`/`feed-rs`: both silently drop the foreign-
//!   namespace `<link>` attributes (`opds:facetGroup`, `opds:activeFacet`,
//!   `thr:count`, `pse:count`, `pse:lastRead`…) that facets and page
//!   streaming live in.
//! - Every href resolves against the request URL; hrefs are never
//!   strict-URI-parsed (PSE templates contain literal `{pageNumber}`
//!   braces); received feeds are never schema-validated as a gate; media
//!   types compare by parsed essence + parameters, not string equality.
//! - **OpenSearch templates are filled completely**: optional parameters
//!   the client does not supply become empty, per OpenSearch 1.1 §4.2.
//!   A literal `{atom:author?}` left in the URL is read by the server as
//!   an author to filter on, and answered with 200 and an empty feed.
//! - One media type per `Accept` header, no q-values (wild servers match by
//!   substring). Pagination back-rel is `previous`; totals via OpenSearch
//!   elements (1.2) or `numberOfItems`/`currentPage` (2.0). Both RFC 3339
//!   and date-only date shapes parse everywhere.
//! - Auth: an opaque `Authorization` header value the caller sets, with
//!   HTTP Basic as the convenience form, on 401 at *any* point in a flow,
//!   plus the OPDS Authentication Document
//!   (`application/opds-authentication+json`) for a native login dialog.
//!   The scheme is deliberately not modelled here — a bearer token is the
//!   same field — and anything that cannot be one header belongs in the
//!   injected [`HttpClient`]. Catalog URLs may embed per-user API keys:
//!   opaque, never logged or normalized.
//! - No conditional requests and no Range resume are counted on: a download
//!   lands complete or not at all, and redirects (incl. cross-host) are
//!   followed.
//! - OPDS-PSE page streaming: `{pageNumber}` is 0-based, `pse:lastRead` is
//!   1-based, and stream links may be lazy — behind the complete-entry
//!   `alternate` link rather than in the feed.
//!
//! The full contract, with the server survey behind it, is this crate's
//! `INTEROP.md`.
//!
//! # Downloading a book
//!
//! Two doors, and the choice is about who owns the transfer rather than
//! about convenience. [`OpdsClient::download`] fetches an acquisition
//! through the injected transport and returns once the file is on disk,
//! which is right for a desktop process. [`Entry::download_request`]
//! instead describes the fetch and steps aside, so the host can run it
//! under `WorkManager` or a background `URLSession` and have it survive the
//! app being suspended — something no blocking call can do, however the
//! transport is implemented. See the [`download`] module.
//!
//! # Position sync
//!
//! [`progression`] speaks OPDS Progression 1.0 — reading and writing where
//! a reader last was in one publication — behind the **non-default**
//! `progression` feature. The gate is there because the spec is an
//! unreleased draft; see that module for what turning it on means.

mod atom;
mod client;
pub mod download;
mod href;
pub mod http;
mod model;
mod opds2;
#[cfg(feature = "progression")]
pub mod progression;
#[cfg(feature = "ureq")]
mod ureq_transport;

pub use atom::parse_atom;
pub use client::{
    basic_authorization, expand_search_template, opensearch_template, pse_page_url, OpdsClient,
};
pub use download::DownloadRequest;
pub use href::resolve_url;
pub use http::{Body, HttpClient, HttpError, HttpRequest, HttpResponse};
pub use model::{
    AuthDocument, AuthFlow, AuthLink, Entry, Feed, Group, Link, MediaType, OpdsVersion, Price,
    Series, Totals, AUTH_BASIC, REL_ACQ_PREFIX, REL_FACET, REL_IMAGE, REL_PSE_STREAM,
    REL_THUMBNAIL,
};
pub use opds2::{parse_opds2, parse_opds2_publication};
#[cfg(feature = "progression")]
pub use progression::{
    Device, Progression, ProgressionRefusal, ProgressionUpdate, RefusalReason,
    MEDIA_TYPE_PROGRESSION, REL_PROGRESSION,
};
#[cfg(feature = "ureq")]
pub use ureq_transport::UreqHttp;

/// Client/parse errors. `AuthRequired` carries the server's Authentication
/// Document when it sent one — enough to render a native login dialog.
#[derive(Debug, thiserror::Error)]
pub enum OpdsError {
    #[error("authentication required")]
    AuthRequired(Option<Box<AuthDocument>>),
    #[error("HTTP status {0}")]
    Http(u16),
    #[error("network error: {0}")]
    Network(String),
    #[error("parse error: {0}")]
    Parse(String),
}
