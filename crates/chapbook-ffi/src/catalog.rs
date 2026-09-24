//! Browsing a catalog and getting a book out of it.
//!
//! `opds-client` is a full OPDS 1.2/2.0 implementation — feeds, groups,
//! facets, indirect acquisition chains, prices, page streaming — and
//! none of it crossed here, so a host that was not Rust could open a
//! book from a URL it already had and nothing else. That is a worse gap
//! on a phone than on a desk: a mobile reader's whole supply of books is
//! a catalog screen.
//!
//! **This is a task surface, not the object model.** The client's types
//! are deliberately *not* mirrored. `opds-client` is the one crate here
//! expected to leave this repository (`docs/STABILITY.md` says so), and
//! freezing `Feed`, `Group`, `Price` and `MediaType` into a Contract-tier
//! header would tie its shape to a promise it never agreed to. What
//! crosses instead is what an app *does*: point me at a URL, tell me
//! what is there, narrow it, search it, and put that book on my shelf.
//!
//! Four decisions worth reading before writing a host against it:
//!
//! **The download does the whole job.** [`cb_catalog_download`] fetches
//! the acquisition, imports it into the library, *and records the sync
//! services the entry advertises*. That last part is the point: those
//! services live in the catalog entry and nowhere else, so before this
//! call a host could store sync targets it had no way to learn. A book
//! added through this call arrives ready to reconcile.
//!
//! **A 401 is an answer.** [`cb_catalog_fetch`] reports
//! `CB_ERR_AUTH_REQUIRED` and keeps the authentication document; read
//! it, put up a native login, call [`cb_catalog_set_basic_auth`], fetch
//! again. Base64 is the engine's chore rather than the host's, because
//! the credential's shape is the protocol's business.
//!
//! **Every call here blocks**, including the download. A phone has
//! better concurrency than this ABI could invent — coroutines, an async
//! context, a `URLSession` — so the calls stay simple and the host runs
//! them off its main thread, which is also where its own cancellation
//! belongs. A transfer that must survive the app being suspended is not
//! served by this call at all — no callback can be, since every one of
//! them blocks until it settles. Take the download apart instead:
//! [`CB_ENTRY_DOWNLOAD_URL`](cb_entry_field::CB_ENTRY_DOWNLOAD_URL) and
//! the fields beside it, your own transfer, then
//! [`cb_library_import_file`](crate::cb_library_import_file).
//!
//! **Images cross as URLs, never as bytes.** A cover grid is what a
//! platform image loader is *for* — caching, cancellation, decode
//! sizing, prefetch. Fetch them with whatever your platform already has,
//! and send the same `Authorization` you set here if the catalog wants
//! one.

use std::ffi::{c_char, c_void};

use crate::error::{cb_status, fail, guard};

#[cfg(feature = "opds")]
use crate::abi::{str_in, str_out};
#[cfg(feature = "opds")]
use crate::error::clear_last_error;

/// An open catalog, holding the last feed it fetched — and, since the
/// application layer arrived, the crumb trail that led there and the
/// login it was last refused with; see [`cb_catalog_go`](crate::cb_catalog_go)
/// and its neighbours in the `app` module. Opaque.
///
/// Not thread-safe, like every other handle here: it belongs to one
/// thread at a time, and may move between them.
pub struct cb_catalog {
    #[cfg(feature = "opds")]
    pub(crate) inner: chapbook_app::Catalog,
}

/// What an entry is, which decides what tapping it does.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum cb_entry_kind {
    /// A place to go: a shelf, a section, another feed. Tapping it
    /// fetches [`cb_catalog_entry_href`].
    CB_ENTRY_NAVIGATION = 0,
    /// A book. Tapping it downloads through [`cb_catalog_download`].
    CB_ENTRY_PUBLICATION = 1,
}

/// One row of a catalog listing. Strings travel on their own calls.
#[repr(C)]
pub struct cb_entry {
    pub kind: cb_entry_kind,
    /// How many authors [`cb_catalog_entry_author`] will answer for.
    pub author_count: usize,
    /// Whether this entry can be downloaded at all. A commercial entry
    /// that offers only a purchase link answers false, and a host should
    /// send the reader to [`cb_catalog_entry_href`] in a browser rather
    /// than pretending it can acquire it.
    pub can_download: bool,
    /// Freely downloadable, as against borrowed or bought. What tells a
    /// "Get" button from a "Buy" one.
    pub is_open_access: bool,
    pub has_thumbnail: bool,
    pub has_cover: bool,
    pub has_summary: bool,
    pub has_series: bool,
    /// Where in its series, when the entry says. Meaningful only with
    /// `has_series` *and* `has_series_position`.
    pub series_position: f64,
    pub has_series_position: bool,
    /// Whether the entry advertises a position-sync service, an
    /// annotation container, or both. A host does not have to act on
    /// these — [`cb_catalog_download`] records them itself — but a
    /// catalog that syncs is worth saying so on the row.
    pub syncs_position: bool,
    pub syncs_annotations: bool,
}

/// One facet: a way to narrow the current feed, as the catalog offers
/// it. Its label and href travel on their own calls.
#[repr(C)]
pub struct cb_facet {
    /// Which group it belongs to — "Language", "Sort by". Facets in a
    /// group are alternatives; a host draws one control per group.
    pub group: usize,
    /// Whether this facet is the one currently in force.
    pub active: bool,
    /// How many entries it would show, when the catalog says.
    pub count: u64,
    pub has_count: bool,
}

/// Which way through a paged feed.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum cb_catalog_page {
    CB_PAGE_NEXT = 0,
    CB_PAGE_PREVIOUS = 1,
}

macro_rules! with_opds {
    (($($unused:ident),* $(,)?) $body:block) => {{
        #[cfg(feature = "opds")]
        $body
        #[cfg(not(feature = "opds"))]
        {
            $(let _ = $unused;)*
            fail(
                cb_status::CB_ERR_FORMAT_NOT_BUILT,
                "this build has no OPDS support, so there is no catalog to browse",
            )
        }
    }};
}

#[cfg(feature = "opds")]
macro_rules! catalog_ref {
    ($catalog:expr) => {
        // SAFETY: a handle from `cb_catalog_open`, not yet closed.
        match unsafe { $catalog.as_ref() } {
            Some(catalog) => catalog,
            None => return fail(cb_status::CB_ERR_NULL_ARGUMENT, "catalog is null"),
        }
    };
}

#[cfg(feature = "opds")]
macro_rules! catalog_mut {
    ($catalog:expr) => {
        // SAFETY: a handle from `cb_catalog_open`, not yet closed.
        match unsafe { $catalog.as_mut() } {
            Some(catalog) => catalog,
            None => return fail(cb_status::CB_ERR_NULL_ARGUMENT, "catalog is null"),
        }
    };
}

/// The entry at `index`, or a named failure.
#[cfg(feature = "opds")]
fn entry_at(
    catalog: &cb_catalog,
    index: usize,
) -> Result<&chapbook_reader::chapbook_opds::Entry, cb_status> {
    if catalog.inner.feed().is_none() {
        return Err(fail(
            cb_status::CB_ERR_UNAVAILABLE,
            "nothing has been fetched yet",
        ));
    }
    let entries = catalog.inner.entries();
    entries.get(index).ok_or_else(|| {
        fail(
            cb_status::CB_ERR_INVALID_ARGUMENT,
            format!("entry index {index} out of {}", entries.len()),
        )
    })
}

/// Open a catalog client.
///
/// The transport is the host's, on the same terms as everywhere else:
/// pass `get`, or pass null to use the bundled one where this build has
/// it. Fetching bytes is all a transport ever does here — writing a
/// downloaded file is the engine's job, and a transfer that has to
/// outlive the process is the host's own, taken apart through
/// [`CB_ENTRY_DOWNLOAD_URL`](cb_entry_field::CB_ENTRY_DOWNLOAD_URL)
/// rather than handed to a callback that could not survive it either.
/// `finalize` releases `user` exactly once,
/// including on every failure path of this call.
#[no_mangle]
pub unsafe extern "C" fn cb_catalog_open(
    get: crate::http::cb_http_get_fn,
    finalize: crate::http::cb_http_finalize_fn,
    user: *mut c_void,
    out: *mut *mut cb_catalog,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        let decline = |code, message: &str| {
            if let Some(finalize) = finalize {
                // SAFETY: the host's own finalizer with the host's own
                // pointer, called exactly once.
                unsafe { finalize(user) };
            }
            fail(code, message)
        };
        #[cfg(feature = "opds")]
        {
            use chapbook_reader::chapbook_opds::OpdsClient;
            clear_last_error();
            if out.is_null() {
                return decline(cb_status::CB_ERR_NULL_ARGUMENT, "out is null");
            }
            let client = match get {
                Some(get) => OpdsClient::new(crate::http::host::HostTransport {
                    get,
                    send: None,
                    finalize,
                    user: user as usize,
                }),
                None => {
                    if let Some(finalize) = finalize {
                        // SAFETY: as above, exactly once.
                        unsafe { finalize(user) };
                    }
                    #[cfg(feature = "ureq")]
                    {
                        OpdsClient::with_ureq()
                    }
                    #[cfg(not(feature = "ureq"))]
                    {
                        return fail(
                            cb_status::CB_ERR_FORMAT_NOT_BUILT,
                            "this build bundles no transport; pass a get callback",
                        );
                    }
                }
            };
            // No store and no saved row: a host that opens a catalog this
            // way manages the credential itself, through
            // `cb_catalog_set_authorization`. `cb_app_browse` is the door
            // with a store behind it.
            let handle = Box::new(cb_catalog {
                inner: chapbook_app::Catalog::new(
                    client,
                    std::sync::Arc::new(chapbook_reader::chapbook_core::NoCredentials),
                    String::new(),
                ),
            });
            // SAFETY: checked non-null above.
            unsafe { *out = Box::into_raw(handle) };
            cb_status::CB_OK
        }
        #[cfg(not(feature = "opds"))]
        {
            let _ = (get, out);
            decline(
                cb_status::CB_ERR_FORMAT_NOT_BUILT,
                "this build has no OPDS support",
            )
        }
    })
}

/// Close a catalog. Accepts null.
#[no_mangle]
pub unsafe extern "C" fn cb_catalog_close(catalog: *mut cb_catalog) {
    guard((), || {
        if !catalog.is_null() {
            // SAFETY: a handle from `cb_catalog_open`, freed once.
            drop(unsafe { Box::from_raw(catalog) });
        }
    })
}

/// Send this `Authorization` header value with every request — a bearer
/// token, or whatever the catalog's own scheme wants. Null clears it.
///
/// The value is opaque and never parsed. Key any store you keep it in by
/// *origin*, not by the catalog URL: a catalog URL's path can itself be
/// a secret.
#[no_mangle]
pub unsafe extern "C" fn cb_catalog_set_authorization(
    catalog: *mut cb_catalog,
    value: *const c_char,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_opds!((catalog, value) {
            clear_last_error();
            let catalog = catalog_mut!(catalog);
            if value.is_null() {
                catalog.inner.client().clear_authorization();
                return cb_status::CB_OK;
            }
            // SAFETY: the header's contract.
            let Some(value) = (unsafe { str_in(value, "value") }) else {
                return cb_status::CB_ERR_INVALID_UTF8;
            };
            catalog.inner.client().set_authorization(value);
            cb_status::CB_OK
        })
    })
}

/// Sign in with a username and password — the HTTP Basic flow, which is
/// what an OPDS authentication document offers when it offers anything.
///
/// The encoding is done here on purpose: base64 is a chore in C and a
/// hazard in every language that has to guess whether the credential is
/// UTF-8 first.
#[no_mangle]
pub unsafe extern "C" fn cb_catalog_set_basic_auth(
    catalog: *mut cb_catalog,
    username: *const c_char,
    password: *const c_char,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_opds!((catalog, username, password) {
            clear_last_error();
            let catalog = catalog_mut!(catalog);
            // SAFETY: the header's contract for both.
            let (Some(username), Some(password)) = (unsafe { str_in(username, "username") }, unsafe {
                str_in(password, "password")
            }) else {
                return cb_status::CB_ERR_NULL_ARGUMENT;
            };
            catalog.inner.client().set_basic_auth(username, password);
            cb_status::CB_OK
        })
    })
}

/// Fetch what is at `url` and hold it — a catalog root, a section a
/// navigation entry pointed at, a facet's narrowing, a page of a long
/// feed. Whatever was held before is replaced.
///
/// **Blocking.** Run it off the thread that draws.
///
/// `CB_ERR_AUTH_REQUIRED` means the catalog wants credentials and said
/// so properly; the authentication document is held for the
/// `cb_catalog_auth_*` calls.
#[no_mangle]
pub unsafe extern "C" fn cb_catalog_fetch(
    catalog: *mut cb_catalog,
    url: *const c_char,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_opds!((catalog, url) {
            clear_last_error();
            let catalog = catalog_mut!(catalog);
            // SAFETY: the header's contract.
            let Some(url) = (unsafe { str_in(url, "url") }) else {
                return cb_status::CB_ERR_NULL_ARGUMENT;
            };
            match catalog.inner.fetch(url) {
                Ok(()) => cb_status::CB_OK,
                Err(e) => opds_failure(e),
            }
        })
    })
}

/// Search the catalog that is currently held. The results replace it,
/// so browsing and searching are the same screen.
///
/// `CB_ERR_UNAVAILABLE` when this catalog offers no search — worth
/// asking before drawing a search box. Blocking, like the fetch.
#[no_mangle]
pub unsafe extern "C" fn cb_catalog_search(
    catalog: *mut cb_catalog,
    query: *const c_char,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_opds!((catalog, query) {
            clear_last_error();
            let catalog = catalog_mut!(catalog);
            // SAFETY: the header's contract.
            let Some(query) = (unsafe { str_in(query, "query") }) else {
                return cb_status::CB_ERR_NULL_ARGUMENT;
            };
            if catalog.inner.feed().is_none() {
                return fail(cb_status::CB_ERR_UNAVAILABLE, "nothing has been fetched yet");
            }
            if !catalog.inner.has_search() {
                return fail(
                    cb_status::CB_ERR_UNAVAILABLE,
                    "this catalog offers no search",
                );
            }
            match catalog.inner.search(query) {
                Ok(()) => cb_status::CB_OK,
                Err(e) => opds_failure(e),
            }
        })
    })
}

/// Turn an OPDS error into a status. The authentication document, when
/// there was one, is already held by the catalog for the `cb_catalog_auth_*`
/// calls.
#[cfg(feature = "opds")]
pub(crate) fn opds_failure(error: chapbook_reader::chapbook_opds::OpdsError) -> cb_status {
    use chapbook_reader::chapbook_opds::OpdsError;
    match error {
        OpdsError::AuthRequired(_) => fail(
            cb_status::CB_ERR_AUTH_REQUIRED,
            "the catalog wants credentials",
        ),
        OpdsError::Network(message) => fail(cb_status::CB_ERR_NETWORK, message),
        OpdsError::Http(status) => fail(
            cb_status::CB_ERR_OPDS,
            format!("the catalog answered HTTP {status}"),
        ),
        OpdsError::Parse(message) => fail(cb_status::CB_ERR_PARSE, message),
    }
}

/// The held feed's title — what a browse screen puts at the top.
#[no_mangle]
pub unsafe extern "C" fn cb_catalog_feed_title(
    catalog: *const cb_catalog,
    buf: *mut c_char,
    cap: usize,
    needed: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_opds!((catalog, buf, cap, needed) {
            clear_last_error();
            let catalog = catalog_ref!(catalog);
            let Some(feed) = catalog.inner.feed() else {
                return fail(cb_status::CB_ERR_UNAVAILABLE, "nothing has been fetched yet");
            };
            // SAFETY: the header's contract for the buffer triple.
            unsafe { str_out(&feed.title, buf, cap, needed) }
        })
    })
}

/// How many entries the held feed offers.
#[no_mangle]
pub unsafe extern "C" fn cb_catalog_entry_count(
    catalog: *const cb_catalog,
    count: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_opds!((catalog, count) {
            clear_last_error();
            let catalog = catalog_ref!(catalog);
            if catalog.inner.feed().is_none() {
                return fail(cb_status::CB_ERR_UNAVAILABLE, "nothing has been fetched yet");
            }
            if count.is_null() {
                return fail(cb_status::CB_ERR_NULL_ARGUMENT, "count out-pointer is null");
            }
            // SAFETY: checked non-null just above.
            unsafe { *count = catalog.inner.entries().len() };
            cb_status::CB_OK
        })
    })
}

/// One entry's plain data.
#[no_mangle]
pub unsafe extern "C" fn cb_catalog_entry(
    catalog: *const cb_catalog,
    index: usize,
    out: *mut cb_entry,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_opds!((catalog, index, out) {
            clear_last_error();
            let catalog = catalog_ref!(catalog);
            if out.is_null() {
                return fail(cb_status::CB_ERR_NULL_ARGUMENT, "out is null");
            }
            let entry = match entry_at(catalog, index) {
                Ok(entry) => entry,
                Err(code) => return code,
            };
            let acquisition = entry.acquisitions().next();
            let (progression, container) = {
                let (p, c) = chapbook_app::chapbook_sync::targets_of(entry);
                (p.is_some(), c.is_some())
            };
            let filled = cb_entry {
                // An entry with something to acquire is a book; one with
                // only a plain link is a place to go.
                kind: if acquisition.is_some() {
                    cb_entry_kind::CB_ENTRY_PUBLICATION
                } else {
                    cb_entry_kind::CB_ENTRY_NAVIGATION
                },
                author_count: entry.authors.len(),
                can_download: acquisition.is_some(),
                is_open_access: entry.acquisitions().any(|link| link.is_open_access()),
                has_thumbnail: entry.thumbnail().is_some(),
                has_cover: entry.cover().is_some(),
                has_summary: entry.summary.is_some() || entry.content_html.is_some(),
                has_series: entry.series.is_some(),
                series_position: entry
                    .series
                    .as_ref()
                    .and_then(|series| series.position)
                    .unwrap_or(0.0),
                has_series_position: entry
                    .series
                    .as_ref()
                    .is_some_and(|series| series.position.is_some()),
                syncs_position: progression,
                syncs_annotations: container,
            };
            // SAFETY: checked non-null above.
            unsafe { *out = filled };
            cb_status::CB_OK
        })
    })
}

/// Which string an entry accessor should answer with.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum cb_entry_field {
    /// The title, as shown on a row.
    CB_ENTRY_TITLE = 0,
    /// Plain-text summary. HTML descriptions are deliberately not
    /// offered: a host would have to sanitize what it did not parse.
    CB_ENTRY_SUMMARY = 1,
    /// The publisher.
    CB_ENTRY_PUBLISHER = 2,
    /// The language tag the catalog states.
    CB_ENTRY_LANGUAGE = 3,
    /// The series name.
    CB_ENTRY_SERIES = 4,
    /// A thumbnail's absolute URL — fetch it with the platform's own
    /// image loader, sending the same `Authorization` if the catalog
    /// wants one.
    CB_ENTRY_THUMBNAIL_URL = 5,
    /// A full cover's absolute URL, for a detail screen.
    CB_ENTRY_COVER_URL = 6,
    /// Where tapping goes: the feed a navigation entry points at, or a
    /// publication's acquisition. A commercial entry whose only link is
    /// a purchase page answers with that page, which a host opens in a
    /// browser rather than downloading.
    CB_ENTRY_HREF = 7,
    /// The entry's OPDS id, opaque — comic-server ids contain slashes
    /// and dots. A stable key for a host's own record of a download it
    /// enqueued; never normalize it, and never build a path out of it.
    CB_ENTRY_ID = 8,
    /// The acquisition to fetch, for a host performing the download
    /// itself. Unlike [`CB_ENTRY_HREF`](cb_entry_field::CB_ENTRY_HREF)
    /// this never falls back to another kind of link, so a navigation
    /// row answers `CB_ERR_UNAVAILABLE` rather than handing back a feed
    /// URL that would download as a book.
    ///
    /// Send it with `Accept` set to the wildcard media range — type
    /// `*`, subtype `*` — and nothing else of the engine's. The header
    /// list is not crossed here because there is exactly one and it is
    /// constant. (Spelled out rather than written literally: the
    /// two-character form would close this comment in the generated C
    /// header, which `tests/header.rs` compiles.)
    CB_ENTRY_DOWNLOAD_URL = 9,
    /// A filename for the user's benefit: one safe path component, with
    /// no separators or control characters, and an extension guessed
    /// from the advertised type.
    ///
    /// Advice, not instruction. It is not unique, it means nothing to
    /// the engine — format is decided by sniffing bytes — and a host
    /// should validate it against its own filesystem's rules. Renaming
    /// the file is expected and costs nothing.
    CB_ENTRY_DOWNLOAD_FILENAME = 10,
    /// The acquisition link's advertised type, verbatim. A hint for the
    /// host's own UI; do not trust it to decide how to read the file.
    CB_ENTRY_DOWNLOAD_MEDIA_TYPE = 11,
    /// The position-sync service this entry advertises, absolute.
    ///
    /// **Read it before the download, not after.** It lives in the
    /// catalog entry and nowhere else, so a host whose transfer outlives
    /// the feed must persist it alongside the job and hand it to
    /// [`cb_library_set_sync_targets`](crate::cb_library_set_sync_targets)
    /// once the file lands. `CB_ERR_UNAVAILABLE` where the entry
    /// advertises none — and in a build without sync, which reports no
    /// targets exactly as the row flags do.
    CB_ENTRY_PROGRESSION_URL = 12,
    /// The Web Annotation container this entry advertises, absolute.
    /// Everything said of
    /// [`CB_ENTRY_PROGRESSION_URL`](cb_entry_field::CB_ENTRY_PROGRESSION_URL)
    /// applies.
    CB_ENTRY_ANNOTATION_CONTAINER = 13,
}

/// One of an entry's strings. `CB_ERR_UNAVAILABLE` where the entry
/// carries none — which the flags on [`cb_catalog_entry`] predict for
/// the ones a row draws.
#[no_mangle]
pub unsafe extern "C" fn cb_catalog_entry_text(
    catalog: *const cb_catalog,
    index: usize,
    field: cb_entry_field,
    buf: *mut c_char,
    cap: usize,
    needed: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_opds!((catalog, index, field, buf, cap, needed) {
            use chapbook_reader::chapbook_opds::resolve_url;
            clear_last_error();
            let catalog = catalog_ref!(catalog);
            let entry = match entry_at(catalog, index) {
                Ok(entry) => entry,
                Err(code) => return code,
            };
            let base = catalog.inner.base();
            let value: Option<String> = match field {
                cb_entry_field::CB_ENTRY_TITLE => Some(entry.title.clone()),
                cb_entry_field::CB_ENTRY_SUMMARY => entry.summary.clone(),
                cb_entry_field::CB_ENTRY_PUBLISHER => entry.publisher.clone(),
                cb_entry_field::CB_ENTRY_LANGUAGE => entry.language.clone(),
                cb_entry_field::CB_ENTRY_SERIES => {
                    entry.series.as_ref().map(|series| series.name.clone())
                }
                cb_entry_field::CB_ENTRY_THUMBNAIL_URL => entry
                    .thumbnail()
                    .map(|link| resolve_url(base, &link.href)),
                cb_entry_field::CB_ENTRY_COVER_URL => entry
                    .cover()
                    .map(|link| resolve_url(base, &link.href)),
                cb_entry_field::CB_ENTRY_HREF => entry
                    .acquisitions()
                    .next()
                    .or_else(|| entry.links.first())
                    .map(|link| resolve_url(base, &link.href)),
                cb_entry_field::CB_ENTRY_ID => Some(entry.id.clone()),
                cb_entry_field::CB_ENTRY_DOWNLOAD_URL => entry
                    .download_request()
                    .map(|request| resolve_url(base, &request.url)),
                cb_entry_field::CB_ENTRY_DOWNLOAD_FILENAME => entry
                    .download_request()
                    .map(|request| request.suggested_filename),
                cb_entry_field::CB_ENTRY_DOWNLOAD_MEDIA_TYPE => {
                    entry.download_request().and_then(|request| request.media_type)
                }
                // Resolved by the application layer, even though the
                // parser already did it against the request URL: a no-op
                // on an absolute href, and what stops a root-relative
                // service path — which real catalogs do serve — reaching
                // the library as a path.
                cb_entry_field::CB_ENTRY_PROGRESSION_URL => catalog
                    .inner
                    .download(index)
                    .and_then(|download| download.progression_url),
                cb_entry_field::CB_ENTRY_ANNOTATION_CONTAINER => catalog
                    .inner
                    .download(index)
                    .and_then(|download| download.annotation_container),
            };
            let Some(value) = value else {
                return fail(
                    cb_status::CB_ERR_UNAVAILABLE,
                    "this entry carries no such value",
                );
            };
            // SAFETY: the header's contract for the buffer triple.
            unsafe { str_out(&value, buf, cap, needed) }
        })
    })
}

/// One of an entry's authors, by index into its `author_count`.
#[no_mangle]
pub unsafe extern "C" fn cb_catalog_entry_author(
    catalog: *const cb_catalog,
    index: usize,
    author: usize,
    buf: *mut c_char,
    cap: usize,
    needed: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_opds!((catalog, index, author, buf, cap, needed) {
            clear_last_error();
            let catalog = catalog_ref!(catalog);
            let entry = match entry_at(catalog, index) {
                Ok(entry) => entry,
                Err(code) => return code,
            };
            let Some(name) = entry.authors.get(author) else {
                return fail(
                    cb_status::CB_ERR_INVALID_ARGUMENT,
                    format!("author {author} out of {}", entry.authors.len()),
                );
            };
            // SAFETY: the header's contract for the buffer triple.
            unsafe { str_out(name, buf, cap, needed) }
        })
    })
}

/// How many facets the held feed offers. Zero is ordinary.
#[no_mangle]
pub unsafe extern "C" fn cb_catalog_facet_count(
    catalog: *const cb_catalog,
    count: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_opds!((catalog, count) {
            clear_last_error();
            let catalog = catalog_ref!(catalog);
            if count.is_null() {
                return fail(cb_status::CB_ERR_NULL_ARGUMENT, "count out-pointer is null");
            }
            // SAFETY: checked non-null just above.
            unsafe { *count = facets(catalog).len() };
            cb_status::CB_OK
        })
    })
}

/// The feed's facets, flattened with their group index — the same
/// flattening the contents use, and for the same reason.
#[cfg(feature = "opds")]
fn facets(catalog: &cb_catalog) -> Vec<chapbook_app::Facet> {
    catalog.inner.facets()
}

/// One facet's plain data.
#[no_mangle]
pub unsafe extern "C" fn cb_catalog_facet(
    catalog: *const cb_catalog,
    index: usize,
    out: *mut cb_facet,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_opds!((catalog, index, out) {
            clear_last_error();
            let catalog = catalog_ref!(catalog);
            if out.is_null() {
                return fail(cb_status::CB_ERR_NULL_ARGUMENT, "out is null");
            }
            let all = facets(catalog);
            let Some(facet) = all.get(index) else {
                return fail(
                    cb_status::CB_ERR_INVALID_ARGUMENT,
                    format!("facet index {index} out of {}", all.len()),
                );
            };
            let filled = cb_facet {
                group: facet.group_index,
                active: facet.active,
                count: facet.count.unwrap_or(0),
                has_count: facet.count.is_some(),
            };
            // SAFETY: checked non-null above.
            unsafe { *out = filled };
            cb_status::CB_OK
        })
    })
}

/// Which of a facet's strings to read.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum cb_facet_field {
    /// The facet's own label — "English", "By title".
    CB_FACET_LABEL = 0,
    /// Its group's name — "Language", "Sort by".
    CB_FACET_GROUP = 1,
    /// The URL that applies it; hand it to [`cb_catalog_fetch`].
    CB_FACET_HREF = 2,
}

/// One of a facet's strings.
#[no_mangle]
pub unsafe extern "C" fn cb_catalog_facet_text(
    catalog: *const cb_catalog,
    index: usize,
    field: cb_facet_field,
    buf: *mut c_char,
    cap: usize,
    needed: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_opds!((catalog, index, field, buf, cap, needed) {
            clear_last_error();
            let catalog = catalog_ref!(catalog);
            let all = facets(catalog);
            let Some(facet) = all.get(index) else {
                return fail(
                    cb_status::CB_ERR_INVALID_ARGUMENT,
                    format!("facet index {index} out of {}", all.len()),
                );
            };
            let value = match field {
                cb_facet_field::CB_FACET_LABEL => facet.label.clone(),
                cb_facet_field::CB_FACET_GROUP => facet.group.clone(),
                cb_facet_field::CB_FACET_HREF => facet.href.clone(),
            };
            // SAFETY: the header's contract for the buffer triple.
            unsafe { str_out(&value, buf, cap, needed) }
        })
    })
}

/// The URL of the next or previous page of a long feed, for the
/// infinite scroll a phone browses with. `CB_ERR_UNAVAILABLE` at the
/// end, which is how a host knows to stop asking.
#[no_mangle]
pub unsafe extern "C" fn cb_catalog_page_href(
    catalog: *const cb_catalog,
    direction: cb_catalog_page,
    buf: *mut c_char,
    cap: usize,
    needed: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_opds!((catalog, direction, buf, cap, needed) {
            clear_last_error();
            let catalog = catalog_ref!(catalog);
            if catalog.inner.feed().is_none() {
                return fail(cb_status::CB_ERR_UNAVAILABLE, "nothing has been fetched yet");
            }
            let url = match direction {
                cb_catalog_page::CB_PAGE_NEXT => catalog.inner.next_page(),
                cb_catalog_page::CB_PAGE_PREVIOUS => catalog.inner.previous_page(),
            };
            let Some(url) = url else {
                return fail(cb_status::CB_ERR_UNAVAILABLE, "no page that way");
            };
            // SAFETY: the header's contract for the buffer triple.
            unsafe { str_out(&url, buf, cap, needed) }
        })
    })
}

/// Whether this catalog offers a search — what decides if a search box
/// is drawn at all.
#[no_mangle]
pub unsafe extern "C" fn cb_catalog_has_search(
    catalog: *const cb_catalog,
    has: *mut bool,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_opds!((catalog, has) {
            clear_last_error();
            let catalog = catalog_ref!(catalog);
            if has.is_null() {
                return fail(cb_status::CB_ERR_NULL_ARGUMENT, "has out-pointer is null");
            }
            let offered = catalog.inner.has_search();
            // SAFETY: checked non-null just above.
            unsafe { *has = offered };
            cb_status::CB_OK
        })
    })
}

/// Put an entry on the shelf: fetch it, import it into the library at
/// `library_dir`, record the sync services it advertises, and answer
/// with the library row it became.
///
/// **This is the call the whole module exists for.** A book's position
/// and annotation services live in its catalog entry and nowhere else,
/// so a host that downloaded by hand could store sync targets it had no
/// way to learn — and a book added any other way is a book that will
/// never reconcile. Everything else here is how a reader finds the entry
/// to hand to this.
///
/// **Blocking, and the slowest call in this ABI**: it is a whole book
/// over the network. Run it off the thread that draws — and if the
/// transfer has to survive the app being suspended, do not use this call
/// at all. No callback rescues it: every callback in this ABI blocks
/// until the transfer settles, which is the one thing a background
/// transfer does not do.
///
/// For that, take the same job apart and own it. Read
/// [`CB_ENTRY_DOWNLOAD_URL`](cb_entry_field::CB_ENTRY_DOWNLOAD_URL) and
/// the two sync-service fields beside it, fetch with `WorkManager` or a
/// background `URLSession`, then call
/// [`cb_library_import_file`](crate::cb_library_import_file) and
/// [`cb_library_set_sync_targets`](crate::cb_library_set_sync_targets)
/// when the file lands. This call is exactly those pieces run back to
/// back on one thread, which is why the sync services have to be read
/// *before* a transfer that will outlive the feed.
///
/// `CB_ERR_UNAVAILABLE` for an entry with nothing to acquire — a
/// navigation row, or a purchase-only entry whose
/// [`cb_catalog_entry_href`](cb_catalog_entry_text) belongs in a
/// browser. The staging file is removed whatever happens; the library
/// keeps its own copy.
#[no_mangle]
pub unsafe extern "C" fn cb_catalog_download(
    catalog: *mut cb_catalog,
    index: usize,
    library_dir: *const c_char,
    book_id: *mut i64,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_opds!((catalog, index, library_dir, book_id) {
            #[cfg(not(feature = "library"))]
            {
                let _ = (catalog, index, library_dir, book_id);
                return fail(
                    cb_status::CB_ERR_FORMAT_NOT_BUILT,
                    "this build has no library, so there is no shelf to download onto",
                );
            }
            #[cfg(feature = "library")]
            {
                clear_last_error();
                let catalog = catalog_mut!(catalog);
                // SAFETY: the header's contract.
                let Some(dir) = (unsafe { str_in(library_dir, "library_dir") }) else {
                    return cb_status::CB_ERR_NULL_ARGUMENT;
                };
                if book_id.is_null() {
                    return fail(cb_status::CB_ERR_NULL_ARGUMENT, "book_id out-pointer is null");
                }
                if let Err(code) = entry_at(catalog, index) {
                    return code;
                }
                if catalog.inner.download(index).is_none() {
                    return fail(
                        cb_status::CB_ERR_UNAVAILABLE,
                        "this entry has nothing to download",
                    );
                }
                match catalog
                    .inner
                    .download_to_library(index, std::path::Path::new(dir))
                {
                    Ok(id) => {
                        // SAFETY: checked non-null above.
                        unsafe { *book_id = id.0 };
                        cb_status::CB_OK
                    }
                    Err(e) => crate::error::from_error(&e),
                }
            }
        })
    })
}

// ---- The authentication document ----

/// The title of the authentication document from the last
/// `CB_ERR_AUTH_REQUIRED` — the catalog's own name for itself, which
/// belongs at the top of a login sheet.
/// `CB_ERR_UNAVAILABLE` when no fetch has been refused.
#[no_mangle]
pub unsafe extern "C" fn cb_catalog_auth_title(
    catalog: *const cb_catalog,
    buf: *mut c_char,
    cap: usize,
    needed: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_opds!((catalog, buf, cap, needed) {
            clear_last_error();
            let catalog = catalog_ref!(catalog);
            let Some(title) = catalog.inner.auth_title() else {
                return fail(
                    cb_status::CB_ERR_UNAVAILABLE,
                    "no authentication document was offered",
                );
            };
            // SAFETY: the header's contract for the buffer triple.
            unsafe { str_out(title, buf, cap, needed) }
        })
    })
}

/// Whether the refused catalog offers the username-and-password flow —
/// the one [`cb_catalog_set_basic_auth`] speaks, and the only one OPDS
/// defines that a reader can complete without a browser.
///
/// False means the catalog wants something else (OAuth, SAML); a host
/// should say so plainly rather than showing a login that cannot work.
#[no_mangle]
pub unsafe extern "C" fn cb_catalog_auth_offers_basic(
    catalog: *const cb_catalog,
    offers: *mut bool,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_opds!((catalog, offers) {
            clear_last_error();
            let catalog = catalog_ref!(catalog);
            if offers.is_null() {
                return fail(cb_status::CB_ERR_NULL_ARGUMENT, "offers out-pointer is null");
            }
            let basic = catalog.inner.auth_offers_basic();
            // SAFETY: checked non-null just above.
            unsafe { *offers = basic };
            cb_status::CB_OK
        })
    })
}
