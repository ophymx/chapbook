//! The host-transport seam, driven through the C ABI the way a shell
//! drives it: callbacks installed with `cb_config_set_http_transport`, a
//! catalog opened with `cb_session_open_url`, and no socket anywhere.
//!
//! This is the FFI's half of the bring-your-own-HTTP promise. The Rust
//! half — that an injected `HttpClient` carries the whole OPDS flow — is
//! `opds-client`'s `injected_transport.rs`; what is being proven here is
//! the crossing itself: requests marshal out with their headers, the
//! response builder marshals bytes back in, failure text reaches
//! `cb_last_error_message`, and the finalizer runs exactly once no matter
//! which path consumed the transport.

use std::ffi::{c_char, c_void, CStr, CString};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use chapbook_ffi::*;

const HOST: &str = "https://shelf.example.com";

fn lazy_feed() -> Vec<u8> {
    format!(
        r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <id>urn:cat:comics</id><title>Comics</title>
  <entry><id>urn:c1</id><title>Test Comic</title>
    <link rel="alternate" href="{HOST}/entry" type="application/atom+xml;type=entry;profile=opds-catalog"/>
  </entry>
</feed>"#
    )
    .into_bytes()
}

fn complete_entry() -> Vec<u8> {
    format!(
        r#"<?xml version="1.0"?>
<entry xmlns="http://www.w3.org/2005/Atom" xmlns:pse="http://vaemendis.net/opds-pse/ns">
  <id>urn:c1</id><title>Test Comic</title>
  <link rel="http://vaemendis.net/opds-pse/stream"
        href="{HOST}/pages?page={{pageNumber}}&amp;width={{maxWidth}}"
        type="image/jpeg" pse:count="3"/>
</entry>"#
    )
    .into_bytes()
}

/// A publication feed carrying one acquisition and both sync services —
/// the shape a host takes apart when it means to run the transfer itself.
///
/// The service hrefs are deliberately root-relative, which is what real
/// catalogs serve, so reading them back proves they resolve rather than
/// reaching the library as paths.
fn shelf_feed() -> Vec<u8> {
    format!(
        r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom" xmlns:oa="http://www.w3.org/ns/oa#">
  <id>urn:cat:books</id><title>Books</title>
  <entry><id>urn:book/1: odd</id><title>Dune: Part One</title>
    <link rel="http://opds-spec.org/acquisition" href="{HOST}/get/1" type="application/epub+zip"/>
    <link rel="http://opds-spec.org/progression" href="/progression/1"/>
    <link rel="http://www.w3.org/ns/oa#annotationService" href="/marks/1"/>
  </entry>
</feed>"#
    )
    .into_bytes()
}

/// What the test observes from outside: requests that crossed the seam,
/// and whether the host's context was released.
#[derive(Default)]
struct Observed {
    requests: AtomicUsize,
    finalized: AtomicUsize,
    /// Set by the loader thread once it is inside a page fetch.
    fetching: std::sync::atomic::AtomicBool,
}

/// The `user` pointer's referent — dropped by the finalizer, so the drop
/// count *is* the finalize count.
struct HostContext {
    observed: Arc<Observed>,
}

impl Drop for HostContext {
    fn drop(&mut self) {
        self.observed.finalized.fetch_add(1, Ordering::SeqCst);
    }
}

unsafe extern "C" fn finalize(user: *mut c_void) {
    drop(unsafe { Box::from_raw(user as *mut HostContext) });
}

/// A canned catalog server, as the get callback a host would write.
unsafe extern "C" fn serve(
    request: *const cb_http_request,
    response: *mut cb_http_response,
    user: *mut c_void,
) {
    let context = unsafe { &*(user as *const HostContext) };
    context.observed.requests.fetch_add(1, Ordering::SeqCst);

    let url = unsafe { CStr::from_ptr((*request).url) }.to_str().unwrap();
    let path = url.strip_prefix(HOST).unwrap_or("/");
    let (status, content_type, body): (u16, &str, Vec<u8>) = if path.starts_with("/feed") {
        (
            200,
            "application/atom+xml;profile=opds-catalog",
            lazy_feed(),
        )
    } else if path.starts_with("/shelf") {
        (
            200,
            "application/atom+xml;profile=opds-catalog",
            shelf_feed(),
        )
    } else if path.starts_with("/entry") {
        (200, "application/atom+xml;type=entry", complete_entry())
    } else if path.starts_with("/pages") {
        let page = path
            .split("page=")
            .nth(1)
            .and_then(|s| s.split('&').next())
            .unwrap_or("?");
        (200, "image/jpeg", format!("JPEGDATA:{page}").into_bytes())
    } else {
        (404, "text/plain", b"not found".to_vec())
    };

    let content_type = CString::new(content_type).unwrap();
    unsafe {
        assert_eq!(
            cb_http_response_set_status(response, status),
            cb_status::CB_OK
        );
        assert_eq!(
            cb_http_response_set_content_type(response, content_type.as_ptr()),
            cb_status::CB_OK
        );
        // Two calls, to prove chunked appends concatenate.
        let (head, tail) = body.split_at(body.len() / 2);
        assert_eq!(
            cb_http_response_append_body(response, head.as_ptr(), head.len()),
            cb_status::CB_OK
        );
        assert_eq!(
            cb_http_response_append_body(response, tail.as_ptr(), tail.len()),
            cb_status::CB_OK
        );
    }
}

/// Like [`serve`], but a page fetch announces itself and then takes its
/// time — so a test can close the session while the loader thread is
/// provably inside the host's callback.
///
/// Without this, the loader is parked in `recv` at close and exits almost
/// at once, so whether the finalizer beats the assertion is scheduling
/// luck: the detached and the joined implementation both pass on an idle
/// machine and only the detached one fails under load. That is the bug
/// this transport exists to make deterministic.
unsafe extern "C" fn serve_slow_pages(
    request: *const cb_http_request,
    response: *mut cb_http_response,
    user: *mut c_void,
) {
    let url = unsafe { CStr::from_ptr((*request).url) }.to_str().unwrap();
    if url.contains("/pages") {
        let context = unsafe { &*(user as *const HostContext) };
        context.observed.fetching.store(true, Ordering::SeqCst);
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    unsafe { serve(request, response, user) };
}

/// A transport whose network is down, reporting the way the header says.
unsafe extern "C" fn refuse(
    _request: *const cb_http_request,
    response: *mut cb_http_response,
    _user: *mut c_void,
) {
    let message = CString::new("the cable is unplugged").unwrap();
    unsafe {
        assert_eq!(
            cb_http_response_fail(response, message.as_ptr()),
            cb_status::CB_OK
        );
    }
}

/// A buggy transport that reports nothing at all.
unsafe extern "C" fn shrug(
    _request: *const cb_http_request,
    _response: *mut cb_http_response,
    _user: *mut c_void,
) {
}

fn cstr(value: &str) -> CString {
    CString::new(value).expect("no interior NUL")
}

fn fonts() -> *mut cb_font_source {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/fonts");
    let dir = cstr(&dir.to_string_lossy());
    let family = cstr("Crimson Text");
    let fonts = unsafe { cb_font_source_embedded(dir.as_ptr(), family.as_ptr()) };
    assert!(!fonts.is_null());
    fonts
}

/// A config with its own library dir — the page stream requires one — and
/// the given callbacks installed over a fresh [`HostContext`].
fn config_with_transport(
    name: &str,
    get: cb_http_get_fn,
    observed: &Arc<Observed>,
) -> *mut cb_config {
    let config = unsafe { cb_config_new(fonts()) };
    assert!(!config.is_null());
    let dir = std::env::temp_dir().join(format!("chapbook-ffi-http-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("library dir is creatable");
    let dir = cstr(&dir.to_string_lossy());
    assert_eq!(
        unsafe { cb_config_set_library_dir(config, dir.as_ptr()) },
        cb_status::CB_OK
    );

    let context = Box::new(HostContext {
        observed: Arc::clone(observed),
    });
    assert_eq!(
        unsafe {
            cb_config_set_http_transport(
                config,
                get,
                None,
                Some(finalize),
                Box::into_raw(context) as *mut c_void,
            )
        },
        cb_status::CB_OK
    );
    config
}

/// A string accessor read the way the header says: probe for the size,
/// then fill. `None` where the accessor declines, which for these fields
/// is the ordinary answer rather than a fault.
fn read_string(
    mut call: impl FnMut(*mut c_char, usize, *mut usize) -> cb_status,
) -> Option<String> {
    let mut needed: usize = 0;
    if call(std::ptr::null_mut(), 0, &mut needed) != cb_status::CB_ERR_BUFFER_TOO_SMALL {
        return None;
    }
    let mut buf = vec![0u8; needed];
    if call(buf.as_mut_ptr() as *mut c_char, buf.len(), &mut needed) != cb_status::CB_OK {
        return None;
    }
    assert_eq!(buf.pop(), Some(0), "the callee NUL-terminates");
    Some(String::from_utf8(buf).expect("UTF-8 out"))
}

fn last_error() -> String {
    let mut needed: usize = 0;
    unsafe { cb_last_error_message(std::ptr::null_mut(), 0, &mut needed) };
    let mut buf = vec![0u8; needed.max(1)];
    let rc =
        unsafe { cb_last_error_message(buf.as_mut_ptr() as *mut c_char, buf.len(), &mut needed) };
    assert_eq!(rc, cb_status::CB_OK);
    buf.pop();
    String::from_utf8(buf).expect("UTF-8 out")
}

#[test]
fn a_streamed_comic_opens_through_an_injected_transport() {
    let observed = Arc::new(Observed::default());
    let config = config_with_transport("opens", Some(serve), &observed);

    let url = cstr(&format!("{HOST}/feed"));
    let session = unsafe { cb_session_open_url(url.as_ptr(), config) };
    assert!(!session.is_null(), "open failed: {}", last_error());

    // The feed and its complete entry both crossed the host's callback.
    assert!(observed.requests.load(Ordering::SeqCst) >= 2);

    let mut needed: usize = 0;
    unsafe { cb_session_title(session, std::ptr::null_mut(), 0, &mut needed) };
    let mut buf = vec![0u8; needed];
    assert_eq!(
        unsafe {
            cb_session_title(
                session,
                buf.as_mut_ptr() as *mut c_char,
                buf.len(),
                &mut needed,
            )
        },
        cb_status::CB_OK
    );
    buf.pop();
    assert_eq!(String::from_utf8(buf).unwrap(), "Test Comic");

    let mut spine = 0usize;
    assert_eq!(
        unsafe { cb_session_spine_len(session, &mut spine) },
        cb_status::CB_OK
    );
    assert_eq!(spine, 3, "pse:count crossed intact");

    assert_eq!(
        observed.finalized.load(Ordering::SeqCst),
        0,
        "the transport outlives the open — sessions keep fetching pages"
    );
    unsafe { cb_session_close(session) };
    assert_eq!(
        observed.finalized.load(Ordering::SeqCst),
        1,
        "closing the last holder releases the host context, once"
    );
}

#[test]
fn a_transport_failure_reaches_the_caller_as_its_own_sentence() {
    let observed = Arc::new(Observed::default());
    let config = config_with_transport("refused", Some(refuse), &observed);

    let url = cstr(&format!("{HOST}/feed"));
    let session = unsafe { cb_session_open_url(url.as_ptr(), config) };
    assert!(session.is_null());
    assert!(
        last_error().contains("the cable is unplugged"),
        "the host's message should survive the crossing: {}",
        last_error()
    );
    // The failed open consumed the config, and the config the transport.
    assert_eq!(observed.finalized.load(Ordering::SeqCst), 1);
}

#[test]
fn a_transport_that_reports_nothing_is_named_as_the_bug() {
    let observed = Arc::new(Observed::default());
    let config = config_with_transport("silent", Some(shrug), &observed);

    let url = cstr(&format!("{HOST}/feed"));
    let session = unsafe { cb_session_open_url(url.as_ptr(), config) };
    assert!(session.is_null());
    assert!(
        last_error().contains("without reporting a status or a failure"),
        "got: {}",
        last_error()
    );
}

#[test]
fn a_declined_install_still_runs_the_finalizer() {
    // The ownership rule — "the transport owns `user` from this call on" —
    // must hold on the failure paths too, or a host leaks its context
    // exactly when things are already going wrong.
    let observed = Arc::new(Observed::default());
    let context = Box::new(HostContext {
        observed: Arc::clone(&observed),
    });
    let rc = unsafe {
        cb_config_set_http_transport(
            std::ptr::null_mut(),
            Some(serve),
            None,
            Some(finalize),
            Box::into_raw(context) as *mut c_void,
        )
    };
    assert_eq!(rc, cb_status::CB_ERR_NULL_ARGUMENT);
    assert_eq!(observed.finalized.load(Ordering::SeqCst), 1);

    // And a null get callback declines the same way.
    let observed = Arc::new(Observed::default());
    let config = unsafe { cb_config_new(fonts()) };
    let context = Box::new(HostContext {
        observed: Arc::clone(&observed),
    });
    let rc = unsafe {
        cb_config_set_http_transport(
            config,
            None,
            None,
            Some(finalize),
            Box::into_raw(context) as *mut c_void,
        )
    };
    assert_eq!(rc, cb_status::CB_ERR_NULL_ARGUMENT);
    assert_eq!(observed.finalized.load(Ordering::SeqCst), 1);
    unsafe { cb_config_free(config) };
}

#[test]
fn a_url_that_is_not_one_is_refused_before_the_network() {
    let observed = Arc::new(Observed::default());
    let config = config_with_transport("not-a-url", Some(serve), &observed);

    let url = cstr("ftp://shelf.example.com/feed");
    let session = unsafe { cb_session_open_url(url.as_ptr(), config) };
    assert!(session.is_null());
    assert!(
        last_error().contains("http:// or https://"),
        "{}",
        last_error()
    );
    assert_eq!(observed.requests.load(Ordering::SeqCst), 0);
    // Consumed-either-way applies here too.
    assert_eq!(observed.finalized.load(Ordering::SeqCst), 1);
}

/// Closing a session waits for the page it was fetching.
///
/// The loader thread owns the publication, which owns the transport, which
/// owns the host's context and runs its `finalize` on drop. If close does
/// not wait for that thread, it returns to a host whose context is still
/// alive on a thread the host cannot see — and a host that frees it there,
/// which is what the header's wording invites, has a use-after-free while
/// a page fetch is still running through the transport it just tore down.
///
/// The fetch is slow on purpose so the close lands on top of it. That is
/// the only arrangement that tells a joined implementation from a detached
/// one: with the thread idle, both pass.
#[test]
fn closing_a_session_waits_for_the_page_it_was_fetching() {
    let observed = Arc::new(Observed::default());
    let config = config_with_transport("in-flight", Some(serve_slow_pages), &observed);

    let url = cstr(&format!("{HOST}/feed"));
    let session = unsafe { cb_session_open_url(url.as_ptr(), config) };
    assert!(!session.is_null(), "open failed: {}", last_error());

    let metrics = cb_metrics {
        width: 600.0,
        height: 800.0,
        margin_top: 40.0,
        margin_right: 40.0,
        margin_bottom: 40.0,
        margin_left: 40.0,
        dpi_scale: 1.0,
        rotation: cb_rotation::CB_ROTATION_NONE,
    };
    assert_eq!(
        unsafe { cb_session_set_metrics(session, metrics) },
        cb_status::CB_OK
    );

    // Rendering queues the first page on the loader thread and comes back
    // with a placeholder; the fetch is still running.
    let (mut w, mut h) = (0u32, 0u32);
    assert_eq!(
        unsafe { cb_session_render_size(session, &mut w, &mut h) },
        cb_status::CB_OK
    );
    let mut surface = vec![0u8; (w as usize) * (h as usize) * 4];
    let stride = w as usize * 4;
    assert_eq!(
        unsafe {
            cb_session_render_into(session, surface.as_mut_ptr(), surface.len(), w, h, stride)
        },
        cb_status::CB_OK
    );

    // Wait until the host callback says it is in the fetch, so the close
    // below is genuinely concurrent with it rather than after it.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !observed.fetching.load(Ordering::SeqCst) {
        assert!(
            std::time::Instant::now() < deadline,
            "the loader never reached the page fetch"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(
        observed.finalized.load(Ordering::SeqCst),
        0,
        "the transport is in use; nothing should have been released"
    );

    unsafe { cb_session_close(session) };
    assert_eq!(
        observed.finalized.load(Ordering::SeqCst),
        1,
        "close returned while the loader still held the host's context"
    );
}

/// A download taken apart, so a host can run the transfer itself.
///
/// Everything a background job needs comes off the entry *before* the
/// transfer starts, because by the time one lands the feed is usually
/// gone: where to fetch from, what to call the file, and — the part that
/// exists nowhere else — the two sync services the catalog advertises.
#[test]
fn an_entry_describes_a_download_the_host_will_run_itself() {
    if cb_capabilities() & cb_capability::CB_CAP_OPDS as u32 == 0 {
        eprintln!("skipped: this build has no OPDS");
        return;
    }
    let observed = Arc::new(Observed::default());
    let context = Box::new(HostContext {
        observed: Arc::clone(&observed),
    });
    let mut catalog: *mut cb_catalog = std::ptr::null_mut();
    assert_eq!(
        unsafe {
            cb_catalog_open(
                Some(serve),
                None,
                Some(finalize),
                Box::into_raw(context) as *mut c_void,
                &mut catalog,
            )
        },
        cb_status::CB_OK,
        "{}",
        last_error()
    );

    let url = cstr(&format!("{HOST}/shelf"));
    assert_eq!(
        unsafe { cb_catalog_fetch(catalog, url.as_ptr()) },
        cb_status::CB_OK,
        "{}",
        last_error()
    );

    let field = |which| {
        read_string(|buf, cap, needed| unsafe {
            cb_catalog_entry_text(catalog, 0, which, buf, cap, needed)
        })
    };

    assert_eq!(
        field(cb_entry_field::CB_ENTRY_DOWNLOAD_URL).as_deref(),
        Some(format!("{HOST}/get/1").as_str()),
        "the acquisition, absolute"
    );
    assert_eq!(
        field(cb_entry_field::CB_ENTRY_DOWNLOAD_MEDIA_TYPE).as_deref(),
        Some("application/epub+zip")
    );
    // One safe path component: the colon and the space are gone, and the
    // extension follows the advertised type. The host may still rename it.
    assert_eq!(
        field(cb_entry_field::CB_ENTRY_DOWNLOAD_FILENAME).as_deref(),
        Some("Dune_Part_One.epub")
    );
    // Opaque, and this one holds both a slash and a colon — an id is a
    // key, never a filename.
    assert_eq!(
        field(cb_entry_field::CB_ENTRY_ID).as_deref(),
        Some("urn:book/1: odd")
    );

    // The services, resolved from the root-relative hrefs the feed served.
    assert_eq!(
        field(cb_entry_field::CB_ENTRY_PROGRESSION_URL).as_deref(),
        Some(format!("{HOST}/progression/1").as_str()),
        "a service href must not reach the library as a path"
    );
    assert_eq!(
        field(cb_entry_field::CB_ENTRY_ANNOTATION_CONTAINER).as_deref(),
        Some(format!("{HOST}/marks/1").as_str())
    );

    unsafe { cb_catalog_close(catalog) };
    assert_eq!(
        observed.finalized.load(Ordering::SeqCst),
        1,
        "the transport is released once"
    );
}

/// A navigation row has nothing to fetch, and must not answer with the
/// feed it points at — which `CB_ENTRY_HREF` deliberately does.
#[test]
fn a_row_with_nothing_to_acquire_offers_no_download() {
    if cb_capabilities() & cb_capability::CB_CAP_OPDS as u32 == 0 {
        eprintln!("skipped: this build has no OPDS");
        return;
    }
    let observed = Arc::new(Observed::default());
    let context = Box::new(HostContext {
        observed: Arc::clone(&observed),
    });
    let mut catalog: *mut cb_catalog = std::ptr::null_mut();
    assert_eq!(
        unsafe {
            cb_catalog_open(
                Some(serve),
                None,
                Some(finalize),
                Box::into_raw(context) as *mut c_void,
                &mut catalog,
            )
        },
        cb_status::CB_OK
    );
    let url = cstr(&format!("{HOST}/feed"));
    assert_eq!(
        unsafe { cb_catalog_fetch(catalog, url.as_ptr()) },
        cb_status::CB_OK,
        "{}",
        last_error()
    );

    let mut needed = 0usize;
    assert_eq!(
        unsafe {
            cb_catalog_entry_text(
                catalog,
                0,
                cb_entry_field::CB_ENTRY_DOWNLOAD_URL,
                std::ptr::null_mut(),
                0,
                &mut needed,
            )
        },
        cb_status::CB_ERR_UNAVAILABLE,
        "a section is not a book"
    );
    // The same row does point somewhere: that is what HREF is for.
    assert!(read_string(|buf, cap, needed| unsafe {
        cb_catalog_entry_text(catalog, 0, cb_entry_field::CB_ENTRY_HREF, buf, cap, needed)
    })
    .is_some());

    unsafe { cb_catalog_close(catalog) };
}
