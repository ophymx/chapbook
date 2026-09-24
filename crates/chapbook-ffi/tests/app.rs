//! The application layer driven through the C ABI the way a phone
//! drives it: a platform assembled from callbacks — a credential store,
//! a transport — and the decisions read back as codes and numbers.
//!
//! What `chapbook-app`'s own tests prove is the policy. What is proven
//! here is the crossing: a grant's bytes come back the bytes they went
//! in, a login's sign-in reaches the host's store under the engine's key,
//! a search walk yields per unit, a sync report names the book.

use std::collections::HashMap;
use std::ffi::{c_char, c_void, CStr, CString};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chapbook_ffi::*;

const HOST: &str = "https://catalog.example.test";

fn fixture(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(rel)
}

fn cstr(value: &str) -> CString {
    CString::new(value).expect("no interior NUL")
}

fn last_error() -> String {
    read_string(|buf, cap, needed| unsafe { cb_last_error_message(buf, cap, needed) })
        .unwrap_or_else(|_| "<no message>".into())
}

fn read_string(
    mut call: impl FnMut(*mut c_char, usize, *mut usize) -> cb_status,
) -> Result<String, cb_status> {
    let mut needed: usize = 0;
    let probe = call(std::ptr::null_mut(), 0, &mut needed);
    if probe != cb_status::CB_ERR_BUFFER_TOO_SMALL {
        return Err(probe);
    }
    let mut buf = vec![0u8; needed];
    let rc = call(buf.as_mut_ptr() as *mut c_char, buf.len(), &mut needed);
    if rc != cb_status::CB_OK {
        return Err(rc);
    }
    assert_eq!(buf.pop(), Some(0), "the callee NUL-terminates");
    Ok(String::from_utf8(buf).expect("UTF-8 out"))
}

fn library_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("chapbook-ffi-app-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("library dir is creatable");
    dir
}

fn fonts() -> *mut cb_font_source {
    let dir = cstr(&fixture("fonts").to_string_lossy());
    let family = cstr("Crimson Text");
    let fonts = unsafe { cb_font_source_embedded(dir.as_ptr(), family.as_ptr()) };
    assert!(!fonts.is_null(), "{}", last_error());
    fonts
}

// ---- The host's store and transport, as callbacks ----

/// What the host keeps: secrets by key, and what it saw asked.
#[derive(Default)]
struct Host {
    secrets: Mutex<HashMap<String, String>>,
    stored: Mutex<Vec<String>>,
    /// Every request's URL and whether it carried an `Authorization`.
    seen: Mutex<Vec<(String, bool)>>,
    dead: bool,
}

unsafe extern "C" fn host_get(
    key: *const c_char,
    response: *mut cb_credential_response,
    user: *mut c_void,
) {
    let host = unsafe { &*(user as *const Host) };
    let key = unsafe { CStr::from_ptr(key) }.to_str().unwrap();
    if let Some(value) = host.secrets.lock().unwrap().get(key) {
        let value = cstr(value);
        assert_eq!(
            unsafe { cb_credential_response_found(response, value.as_ptr()) },
            cb_status::CB_OK
        );
    }
}

unsafe extern "C" fn host_store(
    key: *const c_char,
    authorization: *const c_char,
    user: *mut c_void,
) -> cb_status {
    let host = unsafe { &*(user as *const Host) };
    let key = unsafe { CStr::from_ptr(key) }.to_str().unwrap().to_string();
    let value = unsafe { CStr::from_ptr(authorization) }
        .to_str()
        .unwrap()
        .to_string();
    host.stored.lock().unwrap().push(key.clone());
    host.secrets.lock().unwrap().insert(key, value);
    cb_status::CB_OK
}

unsafe extern "C" fn host_forget(key: *const c_char, user: *mut c_void) -> cb_status {
    let host = unsafe { &*(user as *const Host) };
    let key = unsafe { CStr::from_ptr(key) }.to_str().unwrap();
    host.secrets.lock().unwrap().remove(key);
    cb_status::CB_OK
}

unsafe extern "C" fn host_release(user: *mut c_void) {
    // The same `Arc<Host>` is handed to the store and the transport, so
    // each finalizer drops its own count.
    drop(unsafe { Arc::from_raw(user as *const Host) });
}

fn feed() -> String {
    format!(
        r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom" xmlns:opds="http://opds-spec.org/2010/catalog">
  <id>urn:root</id><title>Test Shelf</title>
  <link rel="next" href="{HOST}/page2" type="application/atom+xml;profile=opds-catalog"/>
  <link rel="http://opds-spec.org/facet" href="{HOST}/en" title="English" opds:facetGroup="Language" opds:activeFacet="true"/>
  <entry><id>urn:shelf</id><title>A Section</title>
    <link rel="subsection" href="{HOST}/section" type="application/atom+xml;profile=opds-catalog;kind=acquisition"/>
  </entry>
  <entry><id>urn:book:1</id><title>Minimal</title>
    <link rel="http://opds-spec.org/acquisition/open-access" href="{HOST}/books/minimal.epub" type="application/epub+zip"/>
    <link rel="http://opds-spec.org/progression" href="/progress/1" type="application/json"/>
  </entry>
</feed>"#
    )
}

fn page2() -> String {
    format!(
        r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <id>urn:page2</id><title>Test Shelf</title>
  <entry><id>urn:more</id><title>More</title>
    <link rel="subsection" href="{HOST}/more" type="application/atom+xml;profile=opds-catalog;kind=acquisition"/>
  </entry>
</feed>"#
    )
}

fn respond(response: *mut cb_http_response, status: u16, content_type: &str, body: &[u8]) {
    let content_type = cstr(content_type);
    unsafe {
        assert_eq!(
            cb_http_response_set_status(response, status),
            cb_status::CB_OK
        );
        assert_eq!(
            cb_http_response_set_content_type(response, content_type.as_ptr()),
            cb_status::CB_OK
        );
        assert_eq!(
            cb_http_response_append_body(response, body.as_ptr(), body.len()),
            cb_status::CB_OK
        );
    }
}

/// The canned catalog: refuses until an `Authorization` arrives.
unsafe extern "C" fn host_http_get(
    request: *const cb_http_request,
    response: *mut cb_http_response,
    user: *mut c_void,
) {
    let host = unsafe { &*(user as *const Host) };
    let url = unsafe { CStr::from_ptr((*request).url) }
        .to_str()
        .unwrap()
        .to_string();
    let headers =
        unsafe { std::slice::from_raw_parts((*request).headers, (*request).header_count) };
    let authorized = headers.iter().any(|header| {
        unsafe { CStr::from_ptr(header.name) }
            .to_str()
            .unwrap()
            .eq_ignore_ascii_case("authorization")
    });
    host.seen.lock().unwrap().push((url.clone(), authorized));
    if host.dead {
        let message = cstr("nobody home");
        unsafe { cb_http_response_fail(response, message.as_ptr()) };
        return;
    }
    if !authorized {
        respond(
            response,
            401,
            "application/opds-authentication+json",
            format!(
                r#"{{"id":"{HOST}/auth","title":"Test Shelf Login","authentication":[{{"type":"http://opds-spec.org/auth/basic"}}]}}"#
            )
            .as_bytes(),
        );
        return;
    }
    let atom = "application/atom+xml;profile=opds-catalog";
    match url.strip_prefix(HOST).unwrap_or("") {
        "/page2" => respond(response, 200, atom, page2().as_bytes()),
        "/section" | "/en" => respond(response, 200, atom, b"<?xml version=\"1.0\"?><feed xmlns=\"http://www.w3.org/2005/Atom\"><id>urn:s</id><title>The Section</title></feed>"),
        _ => respond(response, 200, atom, feed().as_bytes()),
    }
}

unsafe extern "C" fn host_http_send(
    _method: *const c_char,
    request: *const cb_http_request,
    _body: *const u8,
    _len: usize,
    response: *mut cb_http_response,
    user: *mut c_void,
) {
    unsafe { host_http_get(request, response, user) }
}

/// A config over a fresh library directory, with the host's store and
/// transport installed. The `Host` is shared with the caller so it can
/// look at what crossed.
fn platform(name: &str, dead: bool) -> (*mut cb_config, Arc<Host>, PathBuf) {
    let host = Arc::new(Host {
        dead,
        ..Host::default()
    });
    let config = unsafe { cb_config_new(fonts()) };
    assert!(!config.is_null());
    let dir = library_dir(name);
    let dir_c = cstr(&dir.to_string_lossy());
    assert_eq!(
        unsafe { cb_config_set_library_dir(config, dir_c.as_ptr()) },
        cb_status::CB_OK
    );
    assert_eq!(
        unsafe {
            cb_config_set_credential_store(
                config,
                Some(host_get),
                Some(host_store),
                Some(host_forget),
                Some(host_release),
                Arc::into_raw(host.clone()) as *mut c_void,
            )
        },
        cb_status::CB_OK,
        "{}",
        last_error()
    );
    assert_eq!(
        unsafe {
            cb_config_set_http_transport_full(
                config,
                Some(host_http_get),
                Some(host_http_send),
                Some(host_release),
                Arc::into_raw(host.clone()) as *mut c_void,
            )
        },
        cb_status::CB_OK,
        "{}",
        last_error()
    );
    (config, host, dir)
}

fn app(name: &str, dead: bool) -> (*mut cb_app, Arc<Host>, PathBuf) {
    let (config, host, dir) = platform(name, dead);
    let mut app: *mut cb_app = std::ptr::null_mut();
    let device = cstr("test phone");
    assert_eq!(
        unsafe { cb_app_open(config, device.as_ptr(), &mut app) },
        cb_status::CB_OK,
        "{}",
        last_error()
    );
    assert!(!app.is_null());
    (app, host, dir)
}

fn import(app: *mut cb_app, rel: &str) -> i64 {
    let path = cstr(&fixture(rel).to_string_lossy());
    let mut id = 0i64;
    assert_eq!(
        unsafe { cb_app_import(app, path.as_ptr(), &mut id) },
        cb_status::CB_OK,
        "{}",
        last_error()
    );
    assert!(id > 0);
    id
}

/// Open a library-owned row and lay it out.
fn open_laid_out(app: *mut cb_app, book: i64) -> *mut cb_session {
    let mut session: *mut cb_session = std::ptr::null_mut();
    let mut how = cb_opened::CB_OPENED_MISSING;
    assert_eq!(
        unsafe { cb_app_open_book(app, book, &mut session, &mut how) },
        cb_status::CB_OK,
        "{}",
        last_error()
    );
    assert_eq!(how, cb_opened::CB_OPENED_SESSION);
    assert!(!session.is_null());
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
    let mut count = 0usize;
    assert_eq!(
        unsafe { cb_session_page_count(session, &mut count) },
        cb_status::CB_OK
    );
    session
}

fn has_app() -> bool {
    if cb_capabilities() & cb_capability::CB_CAP_APP as u32 == 0 {
        eprintln!("skipped: this build has no application layer");
        return false;
    }
    true
}

// ---- Custody ----

#[test]
fn an_imported_book_opens_as_a_session_and_reports_its_place() {
    if !has_app() {
        return;
    }
    let (app, _host, dir) = app("import", false);
    let book = import(app, "epub/long.epub");
    let session = open_laid_out(app, book);

    let mut place = cb_place {
        spine: 0,
        spine_len: 0,
        page: 0,
        page_count: 0,
        book_fraction: 0.0,
        pages_left: 0,
        can_go_back: false,
    };
    assert_eq!(
        unsafe { cb_reader_place(session, &mut place) },
        cb_status::CB_OK,
        "{}",
        last_error()
    );
    assert_eq!(place.book_fraction, 0.0);
    assert!(place.spine_len > 1 && place.page_count > 0);
    assert_eq!(place.pages_left, place.page_count - 1);
    assert!(!place.can_go_back);

    let mut moved = false;
    assert_eq!(
        unsafe { cb_session_next_unit(session, &mut moved) },
        cb_status::CB_OK
    );
    assert!(moved);
    assert_eq!(
        unsafe { cb_reader_place(session, &mut place) },
        cb_status::CB_OK
    );
    assert!((place.book_fraction - 1.0 / place.spine_len as f64).abs() < 0.001);

    unsafe { cb_session_close(session) };
    unsafe { cb_app_close(app) };
    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(unix)]
#[test]
fn a_descriptor_is_adopted_and_comes_back_as_the_grant_it_went_in_with() {
    use std::os::fd::IntoRawFd;
    if !has_app() {
        return;
    }
    let (app, _host, dir) = app("adopt", false);
    let file = std::fs::File::open(fixture("epub/minimal.epub")).expect("fixture");
    // Bytes, not text: a bookmark is binary and may hold NULs.
    let grant: &[u8] = &[
        0x63, 0x6f, 0x6e, 0x74, 0x65, 0x6e, 0x74, 0x00, 0xff, 0x2f, 0x34, 0x32,
    ];
    let mut book = 0i64;
    assert_eq!(
        unsafe {
            cb_app_adopt_fd(
                app,
                file.into_raw_fd(),
                cb_format::CB_FORMAT_GUESS,
                grant.as_ptr(),
                grant.len(),
                &mut book,
            )
        },
        cb_status::CB_OK,
        "{}",
        last_error()
    );
    assert!(book > 0);

    // Opening says: the platform's file, go resolve the grant.
    let mut session: *mut cb_session = std::ptr::null_mut();
    let mut how = cb_opened::CB_OPENED_SESSION;
    assert_eq!(
        unsafe { cb_app_open_book(app, book, &mut session, &mut how) },
        cb_status::CB_OK,
        "{}",
        last_error()
    );
    assert_eq!(how, cb_opened::CB_OPENED_ADOPTED);
    assert!(session.is_null());

    // The row's fingerprint is the key; the grant comes back byte for
    // byte through the two-call idiom.
    let dir_c = cstr(&dir.to_string_lossy());
    let mut library: *mut cb_library = std::ptr::null_mut();
    assert_eq!(
        unsafe { cb_library_open(dir_c.as_ptr(), &mut library) },
        cb_status::CB_OK
    );
    let query = cb_book_query {
        search: std::ptr::null(),
        series: std::ptr::null(),
        collection: 0,
        state: cb_reading_state::CB_STATE_ANY,
        sort: cb_sort::CB_SORT_ADDED,
        limit: 0,
        offset: 0,
    };
    let mut shelf: *mut cb_shelf = std::ptr::null_mut();
    assert_eq!(
        unsafe { cb_library_query(library, &query, &mut shelf) },
        cb_status::CB_OK
    );
    let fingerprint =
        read_string(|buf, cap, needed| unsafe { cb_shelf_fingerprint(shelf, 0, buf, cap, needed) })
            .expect("a fingerprint");
    assert_eq!(
        read_string(|buf, cap, needed| unsafe { cb_shelf_file_path(shelf, 0, buf, cap, needed) })
            .unwrap_or_default(),
        "",
        "an adopted book has no copy"
    );
    unsafe { cb_shelf_free(shelf) };
    unsafe { cb_library_close(library) };

    let fingerprint_c = cstr(&fingerprint);
    let mut needed = 0usize;
    assert_eq!(
        unsafe {
            cb_app_grant(
                app,
                fingerprint_c.as_ptr(),
                std::ptr::null_mut(),
                0,
                &mut needed,
            )
        },
        cb_status::CB_ERR_BUFFER_TOO_SMALL
    );
    assert_eq!(needed, grant.len());
    let mut back = vec![0u8; needed];
    assert_eq!(
        unsafe {
            cb_app_grant(
                app,
                fingerprint_c.as_ptr(),
                back.as_mut_ptr(),
                back.len(),
                &mut needed,
            )
        },
        cb_status::CB_OK
    );
    assert_eq!(back, grant);

    // Forgotten, the book is out of reach rather than an error.
    assert_eq!(
        unsafe { cb_app_forget_grant(app, fingerprint_c.as_ptr()) },
        cb_status::CB_OK
    );
    assert_eq!(
        unsafe { cb_app_open_book(app, book, &mut session, &mut how) },
        cb_status::CB_OK
    );
    assert_eq!(how, cb_opened::CB_OPENED_MISSING);
    assert_eq!(
        unsafe {
            cb_app_grant(
                app,
                fingerprint_c.as_ptr(),
                std::ptr::null_mut(),
                0,
                &mut needed,
            )
        },
        cb_status::CB_ERR_UNAVAILABLE
    );

    // Opening over a descriptor with the app's own config reaches the
    // same row, and remembering a grant on that session works too.
    let config = unsafe { cb_app_session_config(app) };
    assert!(!config.is_null());
    let file = std::fs::File::open(fixture("epub/minimal.epub")).expect("fixture");
    let session =
        unsafe { cb_session_open_fd(file.into_raw_fd(), cb_format::CB_FORMAT_GUESS, config) };
    assert!(!session.is_null(), "{}", last_error());
    let mut again = 0i64;
    assert_eq!(
        unsafe { cb_app_adopt(app, session, b"again".as_ptr(), 5, &mut again) },
        cb_status::CB_OK,
        "{}",
        last_error()
    );
    assert_eq!(again, book, "the same bytes are the same row");
    unsafe { cb_session_close(session) };

    unsafe { cb_app_close(app) };
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_row_that_left_the_shelf_is_an_error_and_a_lost_copy_is_missing() {
    if !has_app() {
        return;
    }
    let (app, _host, dir) = app("gone", false);
    let mut session: *mut cb_session = std::ptr::null_mut();
    let mut how = cb_opened::CB_OPENED_SESSION;
    assert_eq!(
        unsafe { cb_app_open_book(app, 4242, &mut session, &mut how) },
        cb_status::CB_ERR_LIBRARY
    );
    let book = import(app, "epub/minimal.epub");
    for entry in std::fs::read_dir(dir.join("books")).expect("the library's copies") {
        std::fs::remove_file(entry.unwrap().path()).expect("take the copy away");
    }
    assert_eq!(
        unsafe { cb_app_open_book(app, book, &mut session, &mut how) },
        cb_status::CB_OK,
        "{}",
        last_error()
    );
    assert_eq!(how, cb_opened::CB_OPENED_MISSING);
    unsafe { cb_app_close(app) };
    let _ = std::fs::remove_dir_all(dir);
}

// ---- The reader's policy ----

#[test]
fn memory_search_and_marks_cross_as_decisions() {
    if !has_app() {
        return;
    }
    let (app, _host, dir) = app("reader", false);
    let book = import(app, "epub/long.epub");
    let session = open_laid_out(app, book);

    // The budget is a quarter of the platform's figure, and a warning
    // halves it.
    assert_eq!(cb_reader_cache_budget_for(256 << 20), 64 << 20);
    assert_eq!(
        unsafe { cb_session_set_cache_budget(session, 64 << 20) },
        cb_status::CB_OK
    );
    let mut budget = 0usize;
    assert_eq!(
        unsafe { cb_reader_after_memory_warning(session, &mut budget) },
        cb_status::CB_OK
    );
    assert_eq!(budget, 32 << 20);

    // A word the page shows, so the hit is not a guess.
    let speakable = read_string(|buf, cap, needed| unsafe {
        cb_session_page_speakable_text(session, buf, cap, needed)
    })
    .expect("a page of text");
    let word = speakable
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
        .find(|w| w.chars().count() >= 4)
        .expect("a word")
        .to_string();

    let blank = cstr("   ");
    let mut walk: *mut cb_search_walk = std::ptr::null_mut();
    assert_eq!(
        unsafe { cb_search_walk_open(blank.as_ptr(), &mut walk) },
        cb_status::CB_ERR_INVALID_ARGUMENT,
        "a blank query clears rather than searches"
    );
    let query = cstr(&word);
    assert_eq!(
        unsafe { cb_search_walk_open(query.as_ptr(), &mut walk) },
        cb_status::CB_OK
    );
    let mut steps = 0;
    loop {
        let mut more = false;
        assert_eq!(
            unsafe { cb_search_walk_step(walk, session, &mut more) },
            cb_status::CB_OK
        );
        steps += 1;
        if !more {
            break;
        }
    }
    let mut units = 0usize;
    assert_eq!(
        unsafe { cb_session_spine_len(session, &mut units) },
        cb_status::CB_OK
    );
    assert_eq!(steps, units, "one step per unit");
    let mut count = 0usize;
    assert_eq!(
        unsafe { cb_search_walk_hit_count(walk, &mut count) },
        cb_status::CB_OK
    );
    assert!(count > 0, "found {word}");
    let mut hit = cb_search_hit {
        spine: 0,
        start: 0,
        end: 0,
        match_start: 0,
        match_end: 0,
    };
    assert_eq!(
        unsafe { cb_search_walk_hit(walk, 0, &mut hit) },
        cb_status::CB_OK
    );
    let context = read_string(|buf, cap, needed| unsafe {
        cb_search_walk_context(walk, 0, buf, cap, needed)
    })
    .expect("a context");
    assert!(context.to_lowercase().contains(&word.to_lowercase()));
    unsafe { cb_search_walk_close(walk) };

    // Showing a hit selects it; the selection becomes a highlight and goes.
    let mut moved = false;
    assert_eq!(
        unsafe { cb_reader_show_hit(session, &hit, &mut moved) },
        cb_status::CB_OK
    );
    let selected = read_string(|buf, cap, needed| unsafe {
        cb_session_selected_text(session, buf, cap, needed)
    })
    .expect("the hit is selected");
    assert_eq!(selected.trim().to_lowercase(), word.to_lowercase());
    let mut id = 0i64;
    assert_eq!(
        unsafe { cb_reader_highlight_selection(session, &mut id) },
        cb_status::CB_OK,
        "{}",
        last_error()
    );
    assert!(id > 0);
    assert_eq!(
        unsafe { cb_reader_highlight_selection(session, &mut id) },
        cb_status::CB_ERR_UNAVAILABLE,
        "the selection went with the highlight"
    );
    let body = cstr("a thought");
    assert_eq!(
        unsafe { cb_reader_note_on_selection(session, body.as_ptr(), &mut id) },
        cb_status::CB_ERR_UNAVAILABLE
    );
    let mut marks = 0usize;
    assert_eq!(
        unsafe { cb_session_annotation_count(session, &mut marks) },
        cb_status::CB_OK
    );
    assert_eq!(marks, 1);

    unsafe { cb_session_close(session) };
    unsafe { cb_app_close(app) };
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_readout_is_a_preference_that_survives_a_relaunch() {
    if !has_app() {
        return;
    }
    let (app, _host, dir) = app("prefs", false);
    let mut label = cb_progress_label::CB_PROGRESS_CHAPTER_PAGE;
    assert_eq!(
        unsafe { cb_app_progress_label(app, &mut label) },
        cb_status::CB_OK
    );
    assert_eq!(label, cb_progress_label::CB_PROGRESS_PERCENT);
    assert_eq!(
        unsafe { cb_app_set_progress_label(app, cb_progress_label::CB_PROGRESS_PAGES_LEFT) },
        cb_status::CB_OK
    );
    unsafe { cb_app_close(app) };

    // The same directory, a new app, no store and no transport: the
    // preference is the library's, not the platform's.
    let config = unsafe { cb_config_new(fonts()) };
    let dir_c = cstr(&dir.to_string_lossy());
    assert_eq!(
        unsafe { cb_config_set_library_dir(config, dir_c.as_ptr()) },
        cb_status::CB_OK
    );
    let mut again: *mut cb_app = std::ptr::null_mut();
    let device = cstr("test phone");
    assert_eq!(
        unsafe { cb_app_open(config, device.as_ptr(), &mut again) },
        cb_status::CB_OK,
        "{}",
        last_error()
    );
    assert_eq!(
        unsafe { cb_app_progress_label(again, &mut label) },
        cb_status::CB_OK
    );
    assert_eq!(label, cb_progress_label::CB_PROGRESS_PAGES_LEFT);
    unsafe { cb_app_close(again) };
    let _ = std::fs::remove_dir_all(dir);
}

// ---- Catalogs ----

fn has_opds() -> bool {
    if cb_capabilities() & cb_capability::CB_CAP_OPDS as u32 == 0 {
        eprintln!("skipped: this build has no OPDS");
        return false;
    }
    true
}

fn browse_state(catalog: *const cb_catalog) -> cb_browse_state {
    let mut state = cb_browse_state::CB_BROWSE_OPENING;
    assert_eq!(
        unsafe { cb_catalog_state(catalog, &mut state) },
        cb_status::CB_OK
    );
    state
}

fn browse_text(catalog: *const cb_catalog, field: cb_browse_field) -> Result<String, cb_status> {
    read_string(|buf, cap, needed| unsafe {
        cb_catalog_browse_text(catalog, field, buf, cap, needed)
    })
}

fn entry_count(catalog: *const cb_catalog) -> usize {
    let mut count = 0usize;
    assert_eq!(
        unsafe { cb_catalog_entry_count(catalog, &mut count) },
        cb_status::CB_OK,
        "{}",
        last_error()
    );
    count
}

#[test]
fn a_saved_catalog_is_browsed_refused_signed_into_and_paged() {
    if !has_app() || !has_opds() {
        return;
    }
    let (app, host, dir) = app("browse", false);

    // Saved catalogs: added, listed in order, renamed, removed.
    let mut count = 0usize;
    assert_eq!(
        unsafe { cb_app_catalog_count(app, &mut count) },
        cb_status::CB_OK
    );
    assert_eq!(count, 0);
    let url = cstr(&format!(" {HOST}/opds/ "));
    let empty = cstr("");
    let mut saved = 0i64;
    assert_eq!(
        unsafe { cb_app_add_catalog(app, url.as_ptr(), empty.as_ptr(), &mut saved) },
        cb_status::CB_OK,
        "{}",
        last_error()
    );
    let other = cstr("https://b.test/opds/");
    let named = cstr("B");
    let mut second = 0i64;
    assert_eq!(
        unsafe { cb_app_add_catalog(app, other.as_ptr(), named.as_ptr(), &mut second) },
        cb_status::CB_OK
    );
    assert_eq!(
        unsafe { cb_app_catalog_count(app, &mut count) },
        cb_status::CB_OK
    );
    assert_eq!(count, 2);
    let mut id = 0i64;
    assert_eq!(
        unsafe { cb_app_catalog_id(app, 0, &mut id) },
        cb_status::CB_OK
    );
    assert_eq!(id, saved);
    assert_eq!(
        read_string(|buf, cap, needed| unsafe {
            cb_app_catalog_text(
                app,
                0,
                cb_saved_catalog_field::CB_SAVED_CATALOG_URL,
                buf,
                cap,
                needed,
            )
        })
        .unwrap(),
        format!("{HOST}/opds/"),
        "whitespace is the reader's typing"
    );
    let title = cstr("Shelf");
    assert_eq!(
        unsafe { cb_app_rename_catalog(app, saved, title.as_ptr()) },
        cb_status::CB_OK
    );
    assert_eq!(
        unsafe { cb_app_remove_catalog(app, second) },
        cb_status::CB_OK
    );
    assert_eq!(
        unsafe { cb_app_remove_catalog(app, second) },
        cb_status::CB_ERR_UNAVAILABLE,
        "already gone"
    );

    // Browse: a 401 is a login, drawn from the authentication document.
    let mut catalog: *mut cb_catalog = std::ptr::null_mut();
    assert_eq!(
        unsafe { cb_app_browse(app, saved, &mut catalog) },
        cb_status::CB_OK,
        "{}",
        last_error()
    );
    assert_eq!(browse_state(catalog), cb_browse_state::CB_BROWSE_OPENING);
    assert_eq!(
        browse_text(catalog, cb_browse_field::CB_BROWSE_TITLE).unwrap(),
        "Shelf",
        "the saved title until the feed says"
    );
    let root = cstr(&format!("{HOST}/opds/"));
    assert_eq!(
        unsafe { cb_catalog_go(catalog, root.as_ptr()) },
        cb_status::CB_ERR_AUTH_REQUIRED
    );
    assert_eq!(browse_state(catalog), cb_browse_state::CB_BROWSE_LOGIN);
    assert_eq!(
        browse_text(catalog, cb_browse_field::CB_BROWSE_LOGIN_TITLE).unwrap(),
        "Test Shelf Login"
    );
    assert_eq!(
        browse_text(catalog, cb_browse_field::CB_BROWSE_RETRY_URL).unwrap(),
        format!("{HOST}/opds/")
    );
    assert_eq!(
        browse_text(catalog, cb_browse_field::CB_BROWSE_FAILURE_URL),
        Err(cb_status::CB_ERR_UNAVAILABLE),
        "a login carries no failure"
    );
    let mut basic = false;
    assert_eq!(
        unsafe { cb_catalog_auth_offers_basic(catalog, &mut basic) },
        cb_status::CB_OK
    );
    assert!(basic);

    // Signing in stores by the engine's key in the host's store, and
    // fetches again.
    let (user, pass) = (cstr("reader"), cstr("secret"));
    assert_eq!(
        unsafe { cb_catalog_sign_in(catalog, user.as_ptr(), pass.as_ptr()) },
        cb_status::CB_OK,
        "{}",
        last_error()
    );
    assert_eq!(browse_state(catalog), cb_browse_state::CB_BROWSE_FEED);
    assert_eq!(
        browse_text(catalog, cb_browse_field::CB_BROWSE_TITLE).unwrap(),
        "Test Shelf"
    );
    let key = read_string(|buf, cap, needed| unsafe {
        cb_credential_key(root.as_ptr(), buf, cap, needed)
    })
    .expect("an origin");
    assert_eq!(key, format!("opds/origin/{HOST}"));
    assert_eq!(
        host.stored.lock().unwrap().as_slice(),
        std::slice::from_ref(&key)
    );
    let expected = read_string(|buf, cap, needed| unsafe {
        cb_basic_authorization(user.as_ptr(), pass.as_ptr(), buf, cap, needed)
    })
    .unwrap();
    assert_eq!(host.secrets.lock().unwrap().get(&key), Some(&expected));
    assert_eq!(expected, "Basic cmVhZGVyOnNlY3JldA==");

    // Paging appends; the row from page one still names its own book.
    assert_eq!(entry_count(catalog), 2);
    let mut appended = false;
    assert_eq!(
        unsafe { cb_catalog_load_more(catalog, &mut appended) },
        cb_status::CB_OK,
        "{}",
        last_error()
    );
    assert!(appended);
    assert_eq!(entry_count(catalog), 3);
    assert_eq!(
        unsafe { cb_catalog_load_more(catalog, &mut appended) },
        cb_status::CB_OK
    );
    assert!(!appended, "page two is the last");
    let progression = read_string(|buf, cap, needed| unsafe {
        cb_catalog_entry_text(
            catalog,
            1,
            cb_entry_field::CB_ENTRY_PROGRESSION_URL,
            buf,
            cap,
            needed,
        )
    })
    .expect("the entry advertises progression");
    assert_eq!(
        progression,
        format!("{HOST}/progress/1"),
        "resolved, not relative"
    );

    // A facet is a crumb; Back walks it; the root is the last crumb.
    assert_eq!(
        unsafe { cb_catalog_apply_facet(catalog, 0) },
        cb_status::CB_OK,
        "{}",
        last_error()
    );
    assert_eq!(
        browse_text(catalog, cb_browse_field::CB_BROWSE_URL).unwrap(),
        format!("{HOST}/en")
    );
    let mut stayed = false;
    assert_eq!(
        unsafe { cb_catalog_back(catalog, &mut stayed) },
        cb_status::CB_OK
    );
    assert!(stayed);
    assert_eq!(
        browse_text(catalog, cb_browse_field::CB_BROWSE_URL).unwrap(),
        format!("{HOST}/opds/")
    );
    assert_eq!(
        unsafe { cb_catalog_back(catalog, &mut stayed) },
        cb_status::CB_OK
    );
    assert!(!stayed, "the root is the last crumb");
    assert_eq!(
        unsafe { cb_catalog_apply_facet(catalog, 9) },
        cb_status::CB_ERR_INVALID_ARGUMENT
    );

    // Every fetch after the sign-in carried the credential, from the
    // store, with no host interceptor.
    let seen = host.seen.lock().unwrap();
    assert!(!seen[0].1, "the first fetch had nothing to send");
    assert!(
        seen[1..].iter().all(|(_, authorized)| *authorized),
        "{seen:?}"
    );
    drop(seen);

    unsafe { cb_catalog_close(catalog) };
    unsafe { cb_app_close(app) };
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_dead_host_is_a_failure_the_screen_can_name() {
    if !has_app() || !has_opds() {
        return;
    }
    let (app, _host, dir) = app("dead-browse", true);
    let mut catalog: *mut cb_catalog = std::ptr::null_mut();
    assert_eq!(
        unsafe { cb_app_browse(app, 0, &mut catalog) },
        cb_status::CB_OK,
        "{}",
        last_error()
    );
    let url = cstr("https://down.test/");
    assert_eq!(
        unsafe { cb_catalog_go(catalog, url.as_ptr()) },
        cb_status::CB_ERR_NETWORK
    );
    assert_eq!(browse_state(catalog), cb_browse_state::CB_BROWSE_FAILED);
    assert_eq!(
        browse_text(catalog, cb_browse_field::CB_BROWSE_FAILURE_URL).unwrap(),
        "https://down.test/"
    );
    assert!(
        browse_text(catalog, cb_browse_field::CB_BROWSE_FAILURE_REASON)
            .unwrap()
            .contains("nobody home")
    );
    let (user, pass) = (cstr("a"), cstr("b"));
    assert_eq!(
        unsafe { cb_catalog_sign_in(catalog, user.as_ptr(), pass.as_ptr()) },
        cb_status::CB_ERR_UNAVAILABLE,
        "nothing asked for a login"
    );
    unsafe { cb_catalog_close(catalog) };
    unsafe { cb_app_close(app) };
    let _ = std::fs::remove_dir_all(dir);
}

// ---- Downloads and sync ----

#[test]
fn a_landed_download_is_shelved_and_a_dead_service_reports_per_book() {
    if !has_app() || !has_opds() {
        return;
    }
    assert_eq!(
        cb_download_outcome_of_status(200),
        cb_download_outcome::CB_DOWNLOAD_LANDED
    );
    assert_eq!(
        cb_download_outcome_of_status(401),
        cb_download_outcome::CB_DOWNLOAD_REFUSED
    );
    assert_eq!(
        cb_download_outcome_of_status(404),
        cb_download_outcome::CB_DOWNLOAD_GONE
    );
    assert_eq!(
        cb_download_outcome_of_status(503),
        cb_download_outcome::CB_DOWNLOAD_AGAIN
    );
    assert_eq!(
        cb_download_outcome_of_status(0),
        cb_download_outcome::CB_DOWNLOAD_AGAIN
    );

    let (app, _host, dir) = app("land", true);
    let landed = dir.join("download-1");
    std::fs::copy(fixture("epub/minimal.epub"), &landed).expect("stage");
    let file = cstr(&landed.to_string_lossy());
    let progression = cstr("https://down.test/progress/1");
    let mut book = 0i64;
    assert_eq!(
        unsafe {
            cb_app_land_download(
                app,
                file.as_ptr(),
                progression.as_ptr(),
                std::ptr::null(),
                &mut book,
            )
        },
        cb_status::CB_OK,
        "{}",
        last_error()
    );
    assert!(landed.exists(), "the file is the platform's to remove");
    let mut again = 0i64;
    assert_eq!(
        unsafe {
            cb_app_land_download(
                app,
                file.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                &mut again,
            )
        },
        cb_status::CB_OK
    );
    assert_eq!(again, book, "a retried job is the same row");

    // Move and save, so the position owes the (dead) service a push.
    let session = open_laid_out(app, book);
    let mut moved = false;
    assert_eq!(
        unsafe { cb_session_next_page(session, &mut moved) },
        cb_status::CB_OK
    );
    assert_eq!(
        unsafe { cb_session_save_position(session) },
        cb_status::CB_OK
    );
    unsafe { cb_session_close(session) };

    let mut started = false;
    assert_eq!(
        unsafe { cb_app_sync_all(app, None, std::ptr::null_mut(), &mut started) },
        cb_status::CB_OK,
        "{}",
        last_error()
    );
    assert!(started, "a book with a service is something to sync");

    let mut report = cb_sync_report {
        kind: cb_sync_kind::CB_SYNC_FINISHED,
        book: 0,
        position: cb_sync_position::CB_SYNC_POSITION_IDLE,
        detail: std::ptr::null(),
        marks_created: 0,
        marks_updated: 0,
        marks_deleted: 0,
        marks_adopted: 0,
        marks_refreshed: 0,
        marks_withdrawn: 0,
        marks_merged: 0,
        marks_conflicts: 0,
        listing_truncated: false,
        marks_error: std::ptr::null(),
        books: 0,
    };
    let mut kinds = Vec::new();
    for _ in 0..500 {
        match unsafe { cb_app_sync_next(app, &mut report) } {
            cb_status::CB_OK => {
                kinds.push(report.kind);
                if report.kind == cb_sync_kind::CB_SYNC_BOOK {
                    assert_eq!(report.book, book);
                    assert_eq!(report.position, cb_sync_position::CB_SYNC_POSITION_FAILED);
                    assert!(!report.detail.is_null(), "a failure says why");
                }
                if report.kind == cb_sync_kind::CB_SYNC_FINISHED {
                    assert_eq!(report.books, 1);
                    break;
                }
            }
            cb_status::CB_ERR_UNAVAILABLE => {
                std::thread::sleep(std::time::Duration::from_millis(10))
            }
            other => panic!("cb_app_sync_next: {other:?}: {}", last_error()),
        }
    }
    assert_eq!(
        kinds,
        [cb_sync_kind::CB_SYNC_BOOK, cb_sync_kind::CB_SYNC_FINISHED]
    );

    unsafe { cb_app_close(app) };
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn app_calls_tolerate_null_the_way_the_rest_of_the_abi_does() {
    let mut id = 0i64;
    let path = cstr("x");
    assert_eq!(
        unsafe { cb_app_import(std::ptr::null_mut(), path.as_ptr(), &mut id) },
        if has_app() {
            cb_status::CB_ERR_NULL_ARGUMENT
        } else {
            cb_status::CB_ERR_FORMAT_NOT_BUILT
        }
    );
    unsafe { cb_app_close(std::ptr::null_mut()) };
    unsafe { cb_search_walk_close(std::ptr::null_mut()) };
    assert!(unsafe { cb_app_session_config(std::ptr::null()) }.is_null());
    let mut app: *mut cb_app = std::ptr::null_mut();
    let device = cstr("d");
    assert_eq!(
        unsafe { cb_app_open(std::ptr::null_mut(), device.as_ptr(), &mut app) },
        cb_status::CB_ERR_NULL_ARGUMENT
    );
    // An app needs somewhere to persist: a config without a library
    // directory is refused, and consumed either way.
    if has_app() {
        let config = unsafe { cb_config_new(fonts()) };
        assert_eq!(
            unsafe { cb_app_open(config, device.as_ptr(), &mut app) },
            cb_status::CB_ERR_INVALID_ARGUMENT
        );
    }
}
