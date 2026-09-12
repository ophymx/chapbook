//! Drive the C ABI the way a host does — through its own `extern "C"`
//! entry points, with raw pointers, out-parameters and status codes.
//!
//! It exists because of a hole the Android spike left: `chapbook-jni`'s
//! binding is `#[cfg(target_os = "android")]`, so **nothing in `cargo test`
//! ever compiled it**, and its two failure classes — a symbol that does not
//! exist and a signature that does not match — were caught by a shell
//! script after a device build or not at all. A C ABI that only CI on a
//! phone can exercise has the same problem. This has no emulator, no
//! device, and no second language in it.
//!
//! What it deliberately does *not* do is call the Rust API and compare.
//! Every call below goes through the boundary, because the boundary is what
//! is being tested.

use std::ffi::{c_char, CString};
use std::path::PathBuf;

use chapbook_ffi::*;

fn fixture(rel: &str) -> CString {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(rel);
    CString::new(path.to_string_lossy().into_owned()).expect("fixture path has no interior NUL")
}

fn cstr(value: &str) -> CString {
    CString::new(value).expect("no interior NUL")
}

/// The vendored faces, all three axes pinned, so this suite means the same
/// thing on any machine — and so it does not depend on the host having
/// fonts at all.
fn fonts() -> *mut cb_font_source {
    let dir = fixture("fonts");
    let family = cstr("Crimson Text");
    let fonts = unsafe { cb_font_source_embedded(dir.as_ptr(), family.as_ptr()) };
    assert!(!fonts.is_null(), "embedded font source: {}", last_error());
    fonts
}

/// A library directory of this test's own, so nothing here reads or
/// writes the machine's real library. Cleared on the way in, since the
/// name is derived and a previous run's rows would otherwise show up.
fn library_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("chapbook-ffi-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("library dir is creatable");
    dir
}

/// A config pointed at one of those.
fn config(name: &str) -> *mut cb_config {
    let config = unsafe { cb_config_new(fonts()) };
    assert!(!config.is_null(), "config: {}", last_error());
    let dir = cstr(&library_dir(name).to_string_lossy());
    assert_eq!(
        unsafe { cb_config_set_library_dir(config, dir.as_ptr()) },
        cb_status::CB_OK
    );
    config
}

fn open(name: &str, rel: &str) -> *mut cb_session {
    let path = fixture(rel);
    let session = unsafe { cb_session_open_path(path.as_ptr(), config(name)) };
    assert!(!session.is_null(), "open {rel}: {}", last_error());
    session
}

/// The two-call string idiom, exercised as a host would have to write it.
fn read_string(
    mut call: impl FnMut(*mut c_char, usize, *mut usize) -> cb_status,
) -> Result<String, cb_status> {
    let mut needed: usize = 0;
    let probe = call(std::ptr::null_mut(), 0, &mut needed);
    assert_eq!(
        probe,
        cb_status::CB_ERR_BUFFER_TOO_SMALL,
        "a zero-capacity probe must report the size it wanted"
    );
    assert!(needed >= 1, "needed always counts the NUL");

    let mut buf = vec![0u8; needed];
    let rc = call(buf.as_mut_ptr() as *mut c_char, buf.len(), &mut needed);
    if rc != cb_status::CB_OK {
        return Err(rc);
    }
    assert_eq!(buf.pop(), Some(0), "the callee NUL-terminates");
    Ok(String::from_utf8(buf).expect("UTF-8 out"))
}

/// The same idiom for an accessor that may decline before there is
/// anything to size — a book with no series, a book with no cover. The
/// probe's status comes back rather than being asserted away.
fn try_read_string(
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

fn last_error() -> String {
    read_string(|buf, cap, needed| unsafe { cb_last_error_message(buf, cap, needed) })
        .unwrap_or_else(|_| "<no message>".into())
}

fn metrics() -> cb_metrics {
    cb_metrics {
        width: 600.0,
        height: 800.0,
        margin_top: 40.0,
        margin_right: 40.0,
        margin_bottom: 40.0,
        margin_left: 40.0,
        dpi_scale: 1.0,
        rotation: cb_rotation::CB_ROTATION_NONE,
    }
}

// ---- The shape of the ABI itself ----

#[test]
fn the_build_reports_what_it_can_actually_do() {
    // The Android spike shipped for weeks without a library and said
    // nothing, because a build compiled without a capability has nothing to
    // report an error about. This is the answer to that, so it is worth a
    // test that it tells the truth rather than always returning zero.
    let caps = cb_capabilities();
    assert_eq!(
        caps & cb_capability::CB_CAP_LIBRARY as u32 != 0,
        cfg!(feature = "library")
    );
    assert_eq!(
        caps & cb_capability::CB_CAP_PDF as u32 != 0,
        cfg!(feature = "pdf")
    );
    assert!(cb_abi_version() > 0);
}

#[test]
fn null_handles_are_reported_and_never_dereferenced() {
    // A host will pass null. It must get a code, not a segfault, and the
    // *_free calls must tolerate it so error paths need no cascade of ifs.
    let mut moved = false;
    assert_eq!(
        unsafe { cb_session_next_page(std::ptr::null_mut(), &mut moved) },
        cb_status::CB_ERR_NULL_ARGUMENT
    );
    assert_eq!(
        unsafe {
            cb_session_render_size(std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut())
        },
        cb_status::CB_ERR_NULL_ARGUMENT
    );
    unsafe {
        cb_session_close(std::ptr::null_mut());
        cb_config_free(std::ptr::null_mut());
        cb_font_source_free(std::ptr::null_mut());
    }
}

#[test]
fn a_null_out_pointer_is_refused_rather_than_written_through() {
    let session = open("null-out", "epub/illustrated.epub");
    assert_eq!(
        unsafe { cb_session_position(session, std::ptr::null_mut()) },
        cb_status::CB_ERR_NULL_ARGUMENT
    );
    unsafe { cb_session_close(session) };
}

#[test]
fn a_bad_utf8_path_is_a_code_and_not_a_crash() {
    // 0xFF is not valid UTF-8 anywhere, and a host with a mangled filename
    // should learn that rather than open something surprising.
    let bad = [0xFFu8, 0x00];
    let session =
        unsafe { cb_session_open_path(bad.as_ptr() as *const c_char, config("bad-utf8")) };
    assert!(session.is_null());
    assert!(
        last_error().contains("UTF-8"),
        "the message should name the problem: {}",
        last_error()
    );
}

#[test]
fn the_string_idiom_reports_the_size_it_needs() {
    let session = open("strings", "epub/illustrated.epub");
    let title =
        read_string(|buf, cap, needed| unsafe { cb_session_title(session, buf, cap, needed) })
            .expect("title");
    assert!(!title.is_empty());

    // One byte short is an error with `needed` set, not a truncation.
    let mut needed = 0usize;
    let mut small = vec![0u8; title.len()];
    let rc = unsafe {
        cb_session_title(
            session,
            small.as_mut_ptr() as *mut c_char,
            small.len(),
            &mut needed,
        )
    };
    assert_eq!(rc, cb_status::CB_ERR_BUFFER_TOO_SMALL);
    assert_eq!(needed, title.len() + 1, "needed counts the NUL");
    assert!(small.iter().all(|b| *b == 0), "nothing was written");
    unsafe { cb_session_close(session) };
}

#[test]
fn a_bad_font_source_fails_the_open_instead_of_reading_blank() {
    // A source resolving to no faces used to paginate every book to one
    // blank page — rendering, navigating and conforming the whole way. It
    // is an error at construction now, and this ABI must pass that through
    // rather than hand back a working-looking handle.
    let dir = cstr("/nonexistent-directory-for-this-test");
    let family = cstr("Nothing");
    let fonts = unsafe { cb_font_source_embedded(dir.as_ptr(), family.as_ptr()) };
    let config = unsafe { cb_config_new(fonts) };
    let path = fixture("epub/illustrated.epub");
    let session = unsafe { cb_session_open_path(path.as_ptr(), config) };
    assert!(session.is_null(), "a fontless session must not open");
    assert!(
        last_error().to_lowercase().contains("font"),
        "the message should name fonts: {}",
        last_error()
    );
}

#[test]
fn invalid_metrics_are_refused_including_nan() {
    let session = open("metrics", "epub/illustrated.epub");
    for bad in [f32::NAN, 0.0, -1.0, f32::INFINITY] {
        let mut m = metrics();
        m.width = bad;
        assert_eq!(
            unsafe { cb_session_set_metrics(session, m) },
            cb_status::CB_ERR_INVALID_ARGUMENT,
            "width {bad} must be refused"
        );
    }
    // And with no valid metrics ever set, a render size is unavailable
    // rather than a guess.
    let (mut w, mut h) = (0u32, 0u32);
    assert_eq!(
        unsafe { cb_session_render_size(session, &mut w, &mut h) },
        cb_status::CB_ERR_UNAVAILABLE
    );
    unsafe { cb_session_close(session) };
}

// ---- Reading, through the boundary ----

#[test]
fn a_book_opens_paginates_and_turns() {
    let session = open("read", "epub/illustrated.epub");
    assert_eq!(
        unsafe { cb_session_set_metrics(session, metrics()) },
        cb_status::CB_OK
    );

    let mut kind = cb_book_kind::CB_BOOK_COMIC;
    assert_eq!(
        unsafe { cb_session_book_kind(session, &mut kind) },
        cb_status::CB_OK
    );
    assert_eq!(kind, cb_book_kind::CB_BOOK_EPUB);

    let mut faces = 0usize;
    assert_eq!(
        unsafe { cb_session_font_face_count(session, &mut faces) },
        cb_status::CB_OK
    );
    assert!(faces > 0, "the embedded source produced faces");

    let mut spine_len = 0usize;
    assert_eq!(
        unsafe { cb_session_spine_len(session, &mut spine_len) },
        cb_status::CB_OK
    );
    assert!(spine_len > 0);

    let mut start = cb_position { spine: 9, page: 9 };
    assert_eq!(
        unsafe { cb_session_position(session, &mut start) },
        cb_status::CB_OK
    );

    // Walk with the returned flag, never by comparing positions.
    let mut turns = 0;
    loop {
        let mut moved = false;
        assert_eq!(
            unsafe { cb_session_next_page(session, &mut moved) },
            cb_status::CB_OK
        );
        if !moved {
            break;
        }
        turns += 1;
        assert!(turns < 10_000, "the walk terminates");
    }
    assert!(turns > 0, "the book has more than one page");

    // The end stands still.
    let mut at_end = cb_position { spine: 0, page: 0 };
    assert_eq!(
        unsafe { cb_session_position(session, &mut at_end) },
        cb_status::CB_OK
    );
    let mut moved = true;
    assert_eq!(
        unsafe { cb_session_next_page(session, &mut moved) },
        cb_status::CB_OK
    );
    assert!(!moved, "a turn past the end does not move");
    let mut still = cb_position { spine: 0, page: 0 };
    assert_eq!(
        unsafe { cb_session_position(session, &mut still) },
        cb_status::CB_OK
    );
    assert_eq!(still, at_end);

    // And back is symmetric.
    let mut back = false;
    assert_eq!(
        unsafe { cb_session_prev_page(session, &mut back) },
        cb_status::CB_OK
    );
    assert!(back);
    unsafe { cb_session_close(session) };
}

#[test]
fn a_quarter_turn_swaps_the_buffer_and_leaves_the_pagination_alone() {
    // Until this test, `cb_rotation` was a knob no caller in any language
    // had ever turned across the boundary: every case in this file set
    // `CB_ROTATION_NONE`, and `cb_session_render_into` takes a different
    // path for a turned page — through a temporary, copied row by row.
    let session = open("rotate", "epub/minimal.epub");
    assert_eq!(
        unsafe { cb_session_set_metrics(session, metrics()) },
        cb_status::CB_OK
    );

    let (mut w, mut h) = (0u32, 0u32);
    assert_eq!(
        unsafe { cb_session_render_size(session, &mut w, &mut h) },
        cb_status::CB_OK
    );
    assert_eq!((w, h), (600, 800), "the page box, unrotated");
    let mut upright_pages = 0usize;
    assert_eq!(
        unsafe { cb_session_page_count(session, &mut upright_pages) },
        cb_status::CB_OK
    );

    // Same page box, quarter turn. `width`/`height` stay in *reading*
    // orientation — that is what the header now says in as many words —
    // so this is deliberately the same 600x800 as above.
    let turned = cb_metrics {
        rotation: cb_rotation::CB_ROTATION_QUARTER,
        ..metrics()
    };
    assert_eq!(
        unsafe { cb_session_set_metrics(session, turned) },
        cb_status::CB_OK
    );
    assert_eq!(
        unsafe { cb_session_render_size(session, &mut w, &mut h) },
        cb_status::CB_OK
    );
    assert_eq!((w, h), (800, 600), "the axes swap on the way out");

    // And the text did not reflow to fit the panel: rotation is a
    // property of the output. A host that saw the page count move here
    // would be looking at a locator bug, not a paint one.
    let mut turned_pages = 0usize;
    assert_eq!(
        unsafe { cb_session_page_count(session, &mut turned_pages) },
        cb_status::CB_OK
    );
    assert_eq!(turned_pages, upright_pages, "a turn is not a relayout");

    let stride = w as usize * 4;
    let mut surface = vec![0u8; stride * h as usize];
    assert_eq!(
        unsafe {
            cb_session_render_into(session, surface.as_mut_ptr(), surface.len(), w, h, stride)
        },
        cb_status::CB_OK
    );
    assert!(
        surface.iter().any(|b| *b != 0),
        "the rotated copy path actually wrote something"
    );
    assert_eq!(
        &surface[0..4],
        &[255, 255, 255, 255],
        "top-left is still opaque paper after the turn"
    );

    // The buffer the *unrotated* size would have asked for is now the
    // wrong shape, and has to be refused rather than half-filled.
    let mut wrong = vec![0u8; 600 * 4 * 800];
    assert_eq!(
        unsafe { cb_session_render_into(session, wrong.as_mut_ptr(), wrong.len(), 600, 800, 2400) },
        cb_status::CB_ERR_INVALID_ARGUMENT
    );

    unsafe { cb_session_close(session) };
}

#[test]
fn pixels_come_back_in_a_buffer_the_caller_owns() {
    let session = open("pixels", "epub/illustrated.epub");
    assert_eq!(
        unsafe { cb_session_set_metrics(session, metrics()) },
        cb_status::CB_OK
    );

    let (mut w, mut h) = (0u32, 0u32);
    assert_eq!(
        unsafe { cb_session_render_size(session, &mut w, &mut h) },
        cb_status::CB_OK
    );
    assert_eq!((w, h), (600, 800), "unrotated, at scale 1");

    let stride = w as usize * 4;
    let mut surface = vec![0u8; stride * h as usize];
    assert_eq!(
        unsafe {
            cb_session_render_into(session, surface.as_mut_ptr(), surface.len(), w, h, stride)
        },
        cb_status::CB_OK
    );
    assert!(
        surface.iter().any(|b| *b != 0),
        "something was actually drawn"
    );
    // Premultiplied RGBA8888: the default theme's paper is opaque white.
    assert_eq!(
        &surface[0..4],
        &[255, 255, 255, 255],
        "top-left is white paper"
    );

    // A surface of the wrong size is refused, not misdrawn into — and is
    // reported as a bad argument rather than as an unavailable page, so a
    // caller is sent to look at its own arithmetic.
    assert_eq!(
        unsafe {
            cb_session_render_into(
                session,
                surface.as_mut_ptr(),
                surface.len(),
                w - 1,
                h,
                stride,
            )
        },
        cb_status::CB_ERR_INVALID_ARGUMENT
    );
    assert!(
        last_error().contains("render_size"),
        "the message should point at the fix: {}",
        last_error()
    );
    // A stride narrower than a row is caught before any write happens.
    assert_eq!(
        unsafe { cb_session_render_into(session, surface.as_mut_ptr(), surface.len(), w, h, 4) },
        cb_status::CB_ERR_INVALID_ARGUMENT
    );
    // As is a buffer too short for the stride it claims.
    assert_eq!(
        unsafe { cb_session_render_into(session, surface.as_mut_ptr(), 16, w, h, stride) },
        cb_status::CB_ERR_INVALID_ARGUMENT
    );
    unsafe { cb_session_close(session) };
}

#[test]
fn a_theme_change_reaches_the_pixels() {
    // Sepia is the one setting whose red and blue channels differ, so it is
    // what tells a premultiplied-RGBA buffer from a BGRA one. Black text on
    // white paper looks identical either way.
    let session = open("theme", "epub/illustrated.epub");
    assert_eq!(
        unsafe { cb_session_set_metrics(session, metrics()) },
        cb_status::CB_OK
    );

    let mut settings = unsafe {
        let mut out = std::mem::zeroed::<cb_settings>();
        assert_eq!(cb_session_settings(session, &mut out), cb_status::CB_OK);
        out
    };
    assert_eq!(settings.theme, cb_theme::CB_THEME_LIGHT);

    settings.theme = cb_theme::CB_THEME_SEPIA;
    assert_eq!(
        unsafe {
            cb_session_set_settings(session, settings, cb_settings_scope::CB_SCOPE_THIS_BOOK)
        },
        cb_status::CB_OK
    );

    let (mut w, mut h) = (0u32, 0u32);
    assert_eq!(
        unsafe { cb_session_render_size(session, &mut w, &mut h) },
        cb_status::CB_OK
    );
    let stride = w as usize * 4;
    let mut surface = vec![0u8; stride * h as usize];
    assert_eq!(
        unsafe {
            cb_session_render_into(session, surface.as_mut_ptr(), surface.len(), w, h, stride)
        },
        cb_status::CB_OK
    );
    let (r, g, b) = (surface[0], surface[1], surface[2]);
    assert!(
        r > b && g > b,
        "sepia paper is warm: got r={r} g={g} b={b} — if b is highest the channels are swapped"
    );
    unsafe { cb_session_close(session) };
}

#[test]
fn a_reading_position_survives_a_close_and_reopen() {
    // The whole point of the library reaching the boundary at all. Uses one
    // library dir across two sessions, which is also the arrangement that
    // caught the restored-offset bug.
    //
    // Asked of the ABI rather than of `cfg!`, deliberately: this is the
    // question a host has to be able to ask, and a build without the
    // library remembers nothing *correctly*. Skipping on the answer is what
    // a host would do, so the test does it the same way.
    if cb_capabilities() & cb_capability::CB_CAP_LIBRARY as u32 == 0 {
        eprintln!("skipped: this build has no library, so nothing persists");
        return;
    }
    // `long.epub`, so six turns land somewhere a reopen has to actually
    // find again — in a two-page book the position restores correctly by
    // accident.
    let path = fixture("epub/long.epub");
    let dir = std::env::temp_dir().join(format!("chapbook-ffi-restore-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("library dir");
    let dir_c = cstr(&dir.to_string_lossy());

    let open_one = || {
        let config = unsafe { cb_config_new(fonts()) };
        assert_eq!(
            unsafe { cb_config_set_library_dir(config, dir_c.as_ptr()) },
            cb_status::CB_OK
        );
        let session = unsafe { cb_session_open_path(path.as_ptr(), config) };
        assert!(!session.is_null(), "open: {}", last_error());
        assert_eq!(
            unsafe { cb_session_set_metrics(session, metrics()) },
            cb_status::CB_OK
        );
        session
    };

    let left_at = {
        let session = open_one();
        for _ in 0..6 {
            let mut moved = false;
            unsafe { cb_session_next_page(session, &mut moved) };
        }
        let mut at = cb_position { spine: 0, page: 0 };
        unsafe { cb_session_position(session, &mut at) };
        assert_eq!(unsafe { cb_session_suspend(session) }, cb_status::CB_OK);
        unsafe { cb_session_close(session) };
        at
    };
    assert!(left_at.spine > 0 || left_at.page > 0, "moved off the start");

    let session = open_one();
    let mut back = cb_position { spine: 0, page: 0 };
    unsafe { cb_session_position(session, &mut back) };
    assert_eq!(back.spine, left_at.spine, "reopened in the same unit");
    unsafe { cb_session_close(session) };
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn a_descriptor_book_keeps_its_place_across_the_boundary() {
    // The custody flow a phone runs, spelled in C: resolve, open a
    // descriptor, read, suspend, relaunch, resolve again, open again — and
    // come back where the reader left off. The engine adopts the book by
    // its bytes' fingerprint, so no path ever crosses.
    if cb_capabilities() & cb_capability::CB_CAP_LIBRARY as u32 == 0 {
        eprintln!("skipped: this build has no library, so nothing persists");
        return;
    }
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/epub/long.epub");
    let dir = std::env::temp_dir().join(format!("chapbook-ffi-fd-restore-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("library dir");
    let dir_c = cstr(&dir.to_string_lossy());

    let open_one = || {
        use std::os::fd::IntoRawFd;
        let fd = std::fs::File::open(&path)
            .expect("fixture opens")
            .into_raw_fd();
        let config = unsafe { cb_config_new(fonts()) };
        assert_eq!(
            unsafe { cb_config_set_library_dir(config, dir_c.as_ptr()) },
            cb_status::CB_OK
        );
        let session = unsafe { cb_session_open_fd(fd, cb_format::CB_FORMAT_GUESS, config) };
        assert!(!session.is_null(), "open fd: {}", last_error());
        assert_eq!(
            unsafe { cb_session_set_metrics(session, metrics()) },
            cb_status::CB_OK
        );
        session
    };

    let left_at = {
        let session = open_one();
        for _ in 0..6 {
            let mut moved = false;
            unsafe { cb_session_next_page(session, &mut moved) };
        }
        let mut at = cb_position { spine: 0, page: 0 };
        unsafe { cb_session_position(session, &mut at) };
        assert_eq!(unsafe { cb_session_suspend(session) }, cb_status::CB_OK);
        unsafe { cb_session_close(session) };
        at
    };
    assert!(left_at.spine > 0 || left_at.page > 0, "moved off the start");

    let session = open_one();
    let mut back = cb_position { spine: 0, page: 0 };
    unsafe { cb_session_position(session, &mut back) };
    assert_eq!(back.spine, left_at.spine, "reopened in the same unit");
    unsafe { cb_session_close(session) };
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn memory_calls_are_answerable_and_do_not_lose_the_place() {
    let session = open("memory", "epub/illustrated.epub");
    assert_eq!(
        unsafe { cb_session_set_metrics(session, metrics()) },
        cb_status::CB_OK
    );
    let mut moved = false;
    unsafe { cb_session_next_page(session, &mut moved) };
    let mut before = cb_position { spine: 0, page: 0 };
    unsafe { cb_session_position(session, &mut before) };

    let (mut budget, mut used) = (0usize, 0usize);
    assert_eq!(
        unsafe { cb_session_cache_budget(session, &mut budget) },
        cb_status::CB_OK
    );
    assert!(budget > 0);
    assert_eq!(
        unsafe { cb_session_cache_bytes(session, &mut used) },
        cb_status::CB_OK
    );

    // onTrimMemory: give the caches up, keep the place.
    assert_eq!(
        unsafe { cb_session_release_caches(session) },
        cb_status::CB_OK
    );
    let mut after = cb_position { spine: 9, page: 9 };
    unsafe { cb_session_position(session, &mut after) };
    assert_eq!(after, before, "releasing caches is not navigation");

    // And the page still renders, rebuilt from nothing.
    let (mut w, mut h) = (0u32, 0u32);
    unsafe { cb_session_render_size(session, &mut w, &mut h) };
    let stride = w as usize * 4;
    let mut surface = vec![0u8; stride * h as usize];
    assert_eq!(
        unsafe {
            cb_session_render_into(session, surface.as_mut_ptr(), surface.len(), w, h, stride)
        },
        cb_status::CB_OK
    );
    unsafe { cb_session_close(session) };
}

#[test]
fn a_session_moves_between_threads() {
    // `Send` and not `Sync` is the contract the header states. Moving one
    // to another thread and using it there must work; this is what a host
    // that opens on a worker and reads on the UI thread does.
    let session = open("threads", "epub/illustrated.epub") as usize;
    let handle = std::thread::spawn(move || {
        let session = session as *mut cb_session;
        assert_eq!(
            unsafe { cb_session_set_metrics(session, metrics()) },
            cb_status::CB_OK
        );
        let mut moved = false;
        assert_eq!(
            unsafe { cb_session_next_page(session, &mut moved) },
            cb_status::CB_OK
        );
        unsafe { cb_session_close(session) };
    });
    handle.join().expect("the worker did not panic");
}

#[test]
fn the_bytes_decide_the_format_not_the_name() {
    // The same claim the Android spike checked against a `content://` URI,
    // here with no Android in sight: hand over bytes with the format left
    // unstated and the sniffer gets it right.
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/epub/illustrated.epub");
    let bytes = std::fs::read(&path).expect("fixture is readable");
    let session = unsafe {
        cb_session_open_bytes(
            bytes.as_ptr(),
            bytes.len(),
            cb_format::CB_FORMAT_GUESS,
            config("sniff"),
        )
    };
    assert!(!session.is_null(), "open from bytes: {}", last_error());
    let mut kind = cb_book_kind::CB_BOOK_COMIC;
    unsafe { cb_session_book_kind(session, &mut kind) };
    assert_eq!(kind, cb_book_kind::CB_BOOK_EPUB, "sniffed as EPUB");
    unsafe { cb_session_close(session) };
}

// ---- Input ----

/// The tap helper every input test wants: what does a tap here mean.
fn tap(session: *const cb_session, x: f32, y: f32) -> cb_action {
    let mut action = cb_action::CB_ACTION_TOGGLE_MENU;
    assert_eq!(
        unsafe { cb_session_tap_action(session, x, y, &mut action) },
        cb_status::CB_OK,
        "tap at ({x}, {y}): {}",
        last_error()
    );
    action
}

#[test]
fn taps_resolve_through_the_book_and_not_the_shell() {
    // The same tap, on the same page box, means the opposite thing in an
    // RTL book — and the direction is nowhere in the calls a host makes,
    // which is the point: a shell that could pass a direction would pass
    // `Ltr` on every platform it ships to.
    let session = open("tap-ltr", "epub/minimal.epub");

    // Before metrics there are no thirds to land in.
    let mut action = cb_action::CB_ACTION_NONE;
    assert_eq!(
        unsafe { cb_session_tap_action(session, 100.0, 400.0, &mut action) },
        cb_status::CB_ERR_UNAVAILABLE
    );

    assert_eq!(
        unsafe { cb_session_set_metrics(session, metrics()) },
        cb_status::CB_OK
    );
    let mut direction = cb_reading_direction::CB_DIRECTION_RTL;
    assert_eq!(
        unsafe { cb_session_reading_direction(session, &mut direction) },
        cb_status::CB_OK
    );
    assert_eq!(direction, cb_reading_direction::CB_DIRECTION_LTR);

    assert_eq!(tap(session, 100.0, 400.0), cb_action::CB_ACTION_PREV_PAGE);
    assert_eq!(tap(session, 500.0, 400.0), cb_action::CB_ACTION_NEXT_PAGE);
    assert_eq!(tap(session, 300.0, 400.0), cb_action::CB_ACTION_TOGGLE_MENU);
    // NaN is not a coordinate, and must not quietly resolve to a band.
    assert_eq!(
        unsafe { cb_session_tap_action(session, f32::NAN, 400.0, &mut action) },
        cb_status::CB_ERR_INVALID_ARGUMENT
    );
    unsafe { cb_session_close(session) };

    let rtl = open("tap-rtl", "epub/page-direction.epub");
    assert_eq!(
        unsafe { cb_session_set_metrics(rtl, metrics()) },
        cb_status::CB_OK
    );
    let mut direction = cb_reading_direction::CB_DIRECTION_LTR;
    assert_eq!(
        unsafe { cb_session_reading_direction(rtl, &mut direction) },
        cb_status::CB_OK
    );
    assert_eq!(direction, cb_reading_direction::CB_DIRECTION_RTL);
    assert_eq!(tap(rtl, 100.0, 400.0), cb_action::CB_ACTION_NEXT_PAGE);
    assert_eq!(tap(rtl, 500.0, 400.0), cb_action::CB_ACTION_PREV_PAGE);
    unsafe { cb_session_close(rtl) };
}

#[test]
fn a_turned_panel_does_not_turn_the_tap_zones() {
    // Taps arrive in panel coordinates and the rotation is undone inside,
    // so a host drawing to a turned panel forwards what the touch event
    // carries. The failure this guards is a reader whose page turns are
    // ninety degrees out — which is what forwarding these coordinates
    // through unrotated zones would produce (300 of 800 is the middle).
    let session = open("tap-turned", "epub/minimal.epub");
    let mut turned = metrics();
    turned.width = 800.0;
    turned.height = 600.0;
    turned.rotation = cb_rotation::CB_ROTATION_QUARTER;
    assert_eq!(
        unsafe { cb_session_set_metrics(session, turned) },
        cb_status::CB_OK
    );
    // The panel is 600x800. Reading runs down it, so the bottom of the
    // panel is the next-page edge and the top the previous-page edge.
    assert_eq!(tap(session, 300.0, 700.0), cb_action::CB_ACTION_NEXT_PAGE);
    assert_eq!(tap(session, 300.0, 100.0), cb_action::CB_ACTION_PREV_PAGE);
    unsafe { cb_session_close(session) };
}

#[test]
fn the_middle_band_is_the_hosts_to_configure() {
    let session = open("tap-zones", "epub/minimal.epub");
    assert_eq!(
        unsafe { cb_session_set_metrics(session, metrics()) },
        cb_status::CB_OK
    );

    // A host with its own menu gesture makes the middle inert.
    assert_eq!(
        unsafe { cb_session_set_tap_zones(session, 0.4, 0.4, cb_action::CB_ACTION_NONE) },
        cb_status::CB_OK
    );
    assert_eq!(tap(session, 300.0, 400.0), cb_action::CB_ACTION_NONE);
    assert_eq!(tap(session, 100.0, 400.0), cb_action::CB_ACTION_PREV_PAGE);

    // Or binds it to something else entirely.
    assert_eq!(
        unsafe { cb_session_set_tap_zones(session, 0.3, 0.3, cb_action::CB_ACTION_CYCLE_THEME) },
        cb_status::CB_OK
    );
    assert_eq!(tap(session, 300.0, 400.0), cb_action::CB_ACTION_CYCLE_THEME);

    // A fraction outside the page, or one that is not a number, is a
    // mistake worth hearing about, not a policy.
    assert_eq!(
        unsafe { cb_session_set_tap_zones(session, 1.5, 0.3, cb_action::CB_ACTION_NONE) },
        cb_status::CB_ERR_INVALID_ARGUMENT
    );
    assert_eq!(
        unsafe { cb_session_set_tap_zones(session, f32::NAN, 0.3, cb_action::CB_ACTION_NONE) },
        cb_status::CB_ERR_INVALID_ARGUMENT
    );
    // And the refused configuration did not half-apply.
    assert_eq!(tap(session, 300.0, 400.0), cb_action::CB_ACTION_CYCLE_THEME);
    unsafe { cb_session_close(session) };
}

#[test]
fn the_default_key_table_answers_without_a_session() {
    // The table is the value, not the mutability: it already knows the
    // bezel buttons on a Kobo and the volume keys an Android reader
    // borrows, with no session in sight.
    assert_eq!(
        cb_key_default_action(cb_key::CB_KEY_PAGE_DOWN),
        cb_action::CB_ACTION_NEXT_PAGE
    );
    assert_eq!(
        cb_key_default_action(cb_key::CB_KEY_TURN_PREV),
        cb_action::CB_ACTION_PREV_PAGE
    );
    assert_eq!(
        cb_key_default_action(cb_key::CB_KEY_VOLUME_UP),
        cb_action::CB_ACTION_PREV_PAGE
    );

    assert_eq!(
        cb_char_default_action('n' as u32),
        cb_action::CB_ACTION_NEXT_UNIT
    );
    // ASCII case is folded here, so a host need not care.
    assert_eq!(
        cb_char_default_action('N' as u32),
        cb_action::CB_ACTION_NEXT_UNIT
    );
    assert_eq!(
        cb_char_default_action('x' as u32),
        cb_action::CB_ACTION_NONE
    );
    // A surrogate is not a scalar value, and answers nothing rather than
    // panicking on the way to a char.
    assert_eq!(cb_char_default_action(0xD800), cb_action::CB_ACTION_NONE);
}

#[test]
fn apply_answers_repaint_and_consumed_separately() {
    let session = open("apply", "epub/illustrated.epub");
    assert_eq!(
        unsafe { cb_session_set_metrics(session, metrics()) },
        cb_status::CB_OK
    );
    let mut outcome = cb_action_outcome::CB_OUTCOME_CHANGED;

    // The case the outcome type was built for, straight from the Android
    // device run: at unit 0 page 0 a previous-page does not move, and the
    // event is still the reader's — a host that forwards it gets the
    // system's volume slider drawn over the book.
    assert_eq!(
        unsafe { cb_session_apply(session, cb_action::CB_ACTION_PREV_PAGE, &mut outcome) },
        cb_status::CB_OK
    );
    assert_eq!(outcome, cb_action_outcome::CB_OUTCOME_UNCHANGED);

    assert_eq!(
        unsafe { cb_session_apply(session, cb_action::CB_ACTION_NEXT_PAGE, &mut outcome) },
        cb_status::CB_OK
    );
    assert_eq!(outcome, cb_action_outcome::CB_OUTCOME_CHANGED);

    // The engine has no chrome, and an empty back trail is the platform's
    // Back to take — both hand the event back to the host.
    assert_eq!(
        unsafe { cb_session_apply(session, cb_action::CB_ACTION_TOGGLE_MENU, &mut outcome) },
        cb_status::CB_OK
    );
    assert_eq!(outcome, cb_action_outcome::CB_OUTCOME_UNHANDLED);
    assert_eq!(
        unsafe { cb_session_apply(session, cb_action::CB_ACTION_BACK, &mut outcome) },
        cb_status::CB_OK
    );
    assert_eq!(outcome, cb_action_outcome::CB_OUTCOME_UNHANDLED);

    // NONE is a bug in the caller, not a quiet no-op.
    assert_eq!(
        unsafe { cb_session_apply(session, cb_action::CB_ACTION_NONE, &mut outcome) },
        cb_status::CB_ERR_INVALID_ARGUMENT
    );
    unsafe { cb_session_close(session) };
}

// ---- The text surface ----

/// Set metrics and force a layout the way a host does: by asking a
/// question whose answer needs one.
fn laid_out(name: &str, rel: &str) -> *mut cb_session {
    let session = open(name, rel);
    assert_eq!(
        unsafe { cb_session_set_metrics(session, metrics()) },
        cb_status::CB_OK
    );
    let mut pages: usize = 0;
    assert_eq!(
        unsafe { cb_session_page_count(session, &mut pages) },
        cb_status::CB_OK
    );
    assert!(pages > 0);
    session
}

#[test]
fn text_runs_cross_the_boundary() {
    let session = laid_out("text-runs", "epub/minimal.epub");

    let mut count: usize = 0;
    assert_eq!(
        unsafe { cb_session_page_text_run_count(session, &mut count) },
        cb_status::CB_OK
    );
    assert!(count > 0, "a text page has runs");

    let mut run = cb_text_run {
        rect: cb_rect {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0,
        },
        locator_start: 1,
        locator_end: 0,
    };
    assert_eq!(
        unsafe { cb_session_page_text_run(session, 0, &mut run) },
        cb_status::CB_OK
    );
    assert!(run.rect.w > 0.0 && run.rect.h > 0.0, "{run:?}");
    assert!(run.locator_end >= run.locator_start, "{run:?}");

    let text = read_string(|buf, cap, needed| unsafe {
        cb_session_page_text_run_text(session, 0, buf, cap, needed)
    })
    .expect("run text crosses");
    assert!(!text.is_empty());

    unsafe { cb_session_close(session) };
}

#[test]
fn words_and_speakable_text_cross() {
    let session = laid_out("text-words", "epub/minimal.epub");

    let speakable = read_string(|buf, cap, needed| unsafe {
        cb_session_page_speakable_text(session, buf, cap, needed)
    })
    .expect("speakable text crosses");
    assert!(!speakable.is_empty());

    let mut words: usize = 0;
    assert_eq!(
        unsafe { cb_session_page_word_count(session, &mut words) },
        cb_status::CB_OK
    );
    assert!(words > 0, "a text page has words");

    let mut span = cb_word_span {
        text_start: 0,
        text_end: 0,
        locator_start: 0,
        locator_end: 0,
    };
    assert_eq!(
        unsafe { cb_session_page_word(session, 0, &mut span) },
        cb_status::CB_OK
    );
    assert!(span.text_end > span.text_start, "{span:?}");
    assert!(span.locator_end > span.locator_start, "{span:?}");
    // The span indexes the string it was defined against.
    let chars = speakable.chars().count() as u32;
    assert!(span.text_end <= chars, "{span:?} over {chars} chars");

    unsafe { cb_session_close(session) };
}

#[test]
fn text_surface_is_unavailable_before_metrics() {
    let session = open("text-early", "epub/minimal.epub");
    let mut count: usize = 0;
    assert_eq!(
        unsafe { cb_session_page_text_run_count(session, &mut count) },
        cb_status::CB_ERR_UNAVAILABLE
    );
    let mut needed: usize = 0;
    assert_eq!(
        unsafe { cb_session_page_speakable_text(session, std::ptr::null_mut(), 0, &mut needed) },
        cb_status::CB_ERR_UNAVAILABLE
    );
    unsafe { cb_session_close(session) };
}

#[test]
fn text_run_index_out_of_range_is_invalid() {
    let session = laid_out("text-range", "epub/minimal.epub");
    let mut count: usize = 0;
    assert_eq!(
        unsafe { cb_session_page_text_run_count(session, &mut count) },
        cb_status::CB_OK
    );
    let mut run = cb_text_run {
        rect: cb_rect {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0,
        },
        locator_start: 0,
        locator_end: 0,
    };
    assert_eq!(
        unsafe { cb_session_page_text_run(session, count, &mut run) },
        cb_status::CB_ERR_INVALID_ARGUMENT
    );
    let mut words: usize = 0;
    assert_eq!(
        unsafe { cb_session_page_word_count(session, &mut words) },
        cb_status::CB_OK
    );
    let mut span = cb_word_span {
        text_start: 0,
        text_end: 0,
        locator_start: 0,
        locator_end: 0,
    };
    assert_eq!(
        unsafe { cb_session_page_word(session, words, &mut span) },
        cb_status::CB_ERR_INVALID_ARGUMENT
    );
    unsafe { cb_session_close(session) };
}

#[test]
fn range_rects_two_call_idiom() {
    let session = laid_out("text-rects", "epub/minimal.epub");

    // A range with geometry: the first run's own.
    let mut run = cb_text_run {
        rect: cb_rect {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0,
        },
        locator_start: 0,
        locator_end: 0,
    };
    assert_eq!(
        unsafe { cb_session_page_text_run(session, 0, &mut run) },
        cb_status::CB_OK
    );

    // The sizing call, as a host writes it.
    let mut needed: usize = 0;
    assert_eq!(
        unsafe {
            cb_session_range_rects(
                session,
                run.locator_start,
                run.locator_end,
                std::ptr::null_mut(),
                0,
                &mut needed,
            )
        },
        cb_status::CB_ERR_BUFFER_TOO_SMALL
    );
    assert!(needed > 0, "the first run has geometry");

    let mut rects = vec![
        cb_rect {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0,
        };
        needed
    ];
    assert_eq!(
        unsafe {
            cb_session_range_rects(
                session,
                run.locator_start,
                run.locator_end,
                rects.as_mut_ptr(),
                rects.len(),
                &mut needed,
            )
        },
        cb_status::CB_OK
    );
    assert_eq!(needed, rects.len());
    for rect in &rects {
        assert!(rect.w > 0.0 && rect.h > 0.0, "{rect:?}");
    }

    // An empty range sizes to zero, and a zero-capacity call for it is OK.
    assert_eq!(
        unsafe {
            cb_session_range_rects(
                session,
                run.locator_start,
                run.locator_start,
                std::ptr::null_mut(),
                0,
                &mut needed,
            )
        },
        cb_status::CB_OK
    );
    assert_eq!(needed, 0);

    unsafe { cb_session_close(session) };
}

#[test]
fn word_at_answers_under_text() {
    let session = laid_out("text-word-at", "epub/minimal.epub");

    // Where the text sits depends on the fixture fonts, so sweep for it —
    // the same discipline the session tests use.
    let mut hit = None;
    'sweep: for y in (60..760).step_by(8) {
        for x in (60..560).step_by(8) {
            let (mut start, mut end) = (0u32, 0u32);
            if unsafe { cb_session_word_at(session, x as f32, y as f32, &mut start, &mut end) }
                == cb_status::CB_OK
            {
                hit = Some((start, end));
                break 'sweep;
            }
        }
    }
    let (start, end) = hit.expect("some point on the page is a word");
    assert!(end > start);

    // The word has geometry, reachable by the same range.
    let mut needed: usize = 0;
    assert_eq!(
        unsafe {
            cb_session_range_rects(session, start, end, std::ptr::null_mut(), 0, &mut needed)
        },
        cb_status::CB_ERR_BUFFER_TOO_SMALL
    );
    assert!(needed > 0);

    unsafe { cb_session_close(session) };
}

/// The font family crosses this ABI on its own calls, because it is a
/// string and `cb_settings` is plain data a host holds by value.
///
/// The trap that shape creates, and the reason this test exists: a host
/// that reads the settings, changes the font *size*, and writes them back
/// must not silently lose the typeface on the way through. There is no
/// field for it in the struct, so it has to be carried across.
#[test]
fn a_chosen_font_survives_a_settings_round_trip() {
    let session = open("font-family", "epub/illustrated.epub");
    assert_eq!(
        unsafe { cb_session_set_metrics(session, metrics()) },
        cb_status::CB_OK
    );

    // Nothing chosen reads back as empty, not as an error — one branch for
    // a host showing "Publisher's font" in a picker.
    let initial = read_string(|buf, cap, needed| unsafe {
        cb_session_font_family(session, buf, cap, needed)
    })
    .expect("font family");
    assert_eq!(initial, "");

    // Offer what the session can actually match, then choose one.
    let mut count = 0usize;
    assert_eq!(
        unsafe { cb_session_font_family_count(session, &mut count) },
        cb_status::CB_OK
    );
    assert!(count > 0, "a picker needs something to offer");
    let first = read_string(|buf, cap, needed| unsafe {
        cb_session_font_family_at(session, 0, buf, cap, needed)
    })
    .expect("a family name");
    assert!(!first.is_empty());

    let chosen = std::ffi::CString::new(first.clone()).unwrap();
    assert_eq!(
        unsafe {
            cb_session_set_font_family(
                session,
                chosen.as_ptr(),
                cb_settings_scope::CB_SCOPE_THIS_BOOK,
            )
        },
        cb_status::CB_OK
    );
    let now = read_string(|buf, cap, needed| unsafe {
        cb_session_font_family(session, buf, cap, needed)
    })
    .expect("font family");
    assert_eq!(now, first);

    // Now the trap: an unrelated settings write.
    let mut settings = unsafe {
        let mut out = std::mem::zeroed::<cb_settings>();
        assert_eq!(cb_session_settings(session, &mut out), cb_status::CB_OK);
        out
    };
    settings.base_font_px += 2.0;
    assert_eq!(
        unsafe {
            cb_session_set_settings(session, settings, cb_settings_scope::CB_SCOPE_THIS_BOOK)
        },
        cb_status::CB_OK
    );
    let after = read_string(|buf, cap, needed| unsafe {
        cb_session_font_family(session, buf, cap, needed)
    })
    .expect("font family");
    assert_eq!(
        after, first,
        "changing the font size cleared the chosen typeface"
    );

    // Null returns the book to the publisher's font.
    assert_eq!(
        unsafe {
            cb_session_set_font_family(
                session,
                std::ptr::null(),
                cb_settings_scope::CB_SCOPE_THIS_BOOK,
            )
        },
        cb_status::CB_OK
    );
    let cleared = read_string(|buf, cap, needed| unsafe {
        cb_session_font_family(session, buf, cap, needed)
    })
    .expect("font family");
    assert_eq!(cleared, "");

    // Past the end is an argument error, not a crash. The count is read
    // again first, deliberately: a book's own `@font-face` families join
    // the database as units lay out, so the number captured before the
    // relayout above is already stale. That is documented behaviour and
    // this is what it looks like from a host.
    let mut grown = 0usize;
    assert_eq!(
        unsafe { cb_session_font_family_count(session, &mut grown) },
        cb_status::CB_OK
    );
    assert!(grown >= count, "the family list should only grow");
    assert_eq!(
        unsafe {
            cb_session_font_family_at(
                session,
                grown,
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
            )
        },
        cb_status::CB_ERR_INVALID_ARGUMENT
    );

    unsafe { cb_session_close(session) };
}

// ---- The shelf ----

/// Open each book once, which is how a book reaches the library, then
/// hand back the directory they all landed in.
fn stocked_shelf(name: &str) -> PathBuf {
    let dir = library_dir(name);
    for rel in [
        "epub/minimal.epub",
        "epub/series.epub",
        "epub/series-legacy.epub",
    ] {
        let config = unsafe { cb_config_new(fonts()) };
        assert!(!config.is_null(), "config: {}", last_error());
        let dir = cstr(&dir.to_string_lossy());
        assert_eq!(
            unsafe { cb_config_set_library_dir(config, dir.as_ptr()) },
            cb_status::CB_OK
        );
        let path = fixture(rel);
        let session = unsafe { cb_session_open_path(path.as_ptr(), config) };
        assert!(!session.is_null(), "open {rel}: {}", last_error());
        unsafe { cb_session_close(session) };
    }
    dir
}

fn library(name: &str) -> (*mut cb_library, PathBuf) {
    let dir = stocked_shelf(name);
    let mut handle: *mut cb_library = std::ptr::null_mut();
    let path = cstr(&dir.to_string_lossy());
    assert_eq!(
        unsafe { cb_library_open(path.as_ptr(), &mut handle) },
        cb_status::CB_OK,
        "{}",
        last_error()
    );
    assert!(!handle.is_null());
    (handle, dir)
}

fn shelf(library: *mut cb_library, query: &cb_book_query) -> *mut cb_shelf {
    let mut shelf: *mut cb_shelf = std::ptr::null_mut();
    assert_eq!(
        unsafe { cb_library_query(library, query, &mut shelf) },
        cb_status::CB_OK,
        "{}",
        last_error()
    );
    assert!(!shelf.is_null());
    shelf
}

fn shelf_len(shelf: *const cb_shelf) -> usize {
    let mut len = 0usize;
    assert_eq!(
        unsafe { cb_shelf_len(shelf, &mut len) },
        cb_status::CB_OK,
        "{}",
        last_error()
    );
    len
}

fn title_at(shelf: *const cb_shelf, index: usize) -> String {
    read_string(|buf, cap, needed| unsafe { cb_shelf_title(shelf, index, buf, cap, needed) })
        .expect("every book has a title")
}

fn titles(shelf: *const cb_shelf) -> Vec<String> {
    (0..shelf_len(shelf)).map(|i| title_at(shelf, i)).collect()
}

/// A zero-initialized query is the whole shelf. A C caller filling in
/// seven fields to ask for everything is a C caller who will get one of
/// them wrong.
#[test]
fn a_zeroed_query_is_the_whole_shelf() {
    let (lib, dir) = library("shelf-all");
    // Exactly what `cb_book_query query = {0};` produces in C.
    let query = cb_book_query {
        search: std::ptr::null(),
        series: std::ptr::null(),
        collection: 0,
        state: cb_reading_state::CB_STATE_ANY,
        sort: cb_sort::CB_SORT_ADDED,
        limit: 0,
        offset: 0,
    };
    let shelf = shelf(lib, &query);
    assert_eq!(shelf_len(shelf), 3, "{:?}", titles(shelf));

    unsafe { cb_shelf_free(shelf) };
    unsafe { cb_library_close(lib) };
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_row_carries_its_data_and_its_strings_separately() {
    let (lib, dir) = library("shelf-row");
    let series = cstr("The Fixture Cycle");
    let query = cb_book_query {
        series: series.as_ptr(),
        sort: cb_sort::CB_SORT_SERIES,
        ..zeroed_query()
    };
    let shelf = shelf(lib, &query);
    assert_eq!(shelf_len(shelf), 2, "{:?}", titles(shelf));

    // Sorted by position within the series, so #1 comes first.
    let mut book = unsafe { std::mem::zeroed::<cb_book>() };
    assert_eq!(
        unsafe { cb_shelf_book(shelf, 0, &mut book) },
        cb_status::CB_OK
    );
    assert!(book.id > 0);
    assert!(book.has_series_index && book.series_index == 1.0);
    assert_eq!(book.state, cb_reading_state::CB_STATE_UNREAD);
    assert_eq!(book.finished_at, 0, "0 is never, not 1970");
    assert!(!book.has_progress, "an unopened book has no progress");
    assert_eq!(book.author_count, 1);
    assert_eq!(book.collection_count, 0);

    assert_eq!(title_at(shelf, 0), "The Legacy Fixture");
    let author =
        read_string(|b, c, n| unsafe { cb_shelf_author(shelf, 0, 0, b, c, n) }).expect("an author");
    assert_eq!(author, "Ada Fixture");
    let series_out =
        try_read_string(|b, c, n| unsafe { cb_shelf_series(shelf, 0, b, c, n) }).expect("a series");
    assert_eq!(series_out, "The Fixture Cycle");
    let fingerprint = read_string(|b, c, n| unsafe { cb_shelf_fingerprint(shelf, 0, b, c, n) })
        .expect("a fingerprint");
    assert_eq!(fingerprint.len(), 40, "SHA-1 as hex");

    // Past the end is a code, not a read of whatever was there.
    assert_eq!(
        unsafe { cb_shelf_book(shelf, 99, &mut book) },
        cb_status::CB_ERR_INVALID_ARGUMENT
    );
    assert_eq!(
        try_read_string(|b, c, n| unsafe { cb_shelf_author(shelf, 0, 9, b, c, n) }),
        Err(cb_status::CB_ERR_INVALID_ARGUMENT)
    );

    unsafe { cb_shelf_free(shelf) };
    unsafe { cb_library_close(lib) };
    std::fs::remove_dir_all(&dir).ok();
}

/// A book in no series is not a failure to report a series — the
/// accessor declines and the flag on the row said so first.
#[test]
fn an_absent_field_declines_rather_than_returning_an_empty_string() {
    let (lib, dir) = library("shelf-absent");
    let search = cstr("minimal");
    let shelf = shelf(
        lib,
        &cb_book_query {
            search: search.as_ptr(),
            ..zeroed_query()
        },
    );
    assert_eq!(shelf_len(shelf), 1);

    let mut book = unsafe { std::mem::zeroed::<cb_book>() };
    assert_eq!(
        unsafe { cb_shelf_book(shelf, 0, &mut book) },
        cb_status::CB_OK
    );
    assert!(!book.has_series_index);
    assert!(!book.has_cover);
    assert_eq!(
        try_read_string(|b, c, n| unsafe { cb_shelf_series(shelf, 0, b, c, n) }),
        Err(cb_status::CB_ERR_UNAVAILABLE)
    );
    assert_eq!(
        try_read_string(|b, c, n| unsafe { cb_shelf_cover_path(shelf, 0, b, c, n) }),
        Err(cb_status::CB_ERR_UNAVAILABLE)
    );
    // But the language is there, so the same shape succeeds.
    assert_eq!(
        try_read_string(|b, c, n| unsafe { cb_shelf_language(shelf, 0, b, c, n) }).as_deref(),
        Ok("en")
    );

    unsafe { cb_shelf_free(shelf) };
    unsafe { cb_library_close(lib) };
    std::fs::remove_dir_all(&dir).ok();
}

/// The reason the shelf is its own handle and not state on the library:
/// a search box issues a second query while the first page is still on
/// screen, and the rows being drawn must not move.
#[test]
fn a_second_query_does_not_disturb_the_first() {
    let (lib, dir) = library("shelf-two");
    let first = shelf(lib, &zeroed_query());
    let before = titles(first);
    assert_eq!(before.len(), 3);

    let search = cstr("legacy");
    let second = shelf(
        lib,
        &cb_book_query {
            search: search.as_ptr(),
            ..zeroed_query()
        },
    );
    assert_eq!(shelf_len(second), 1);
    assert_eq!(titles(first), before, "the first result set moved");

    // And it outlives the library handle, because it owns its rows.
    unsafe { cb_library_close(lib) };
    assert_eq!(titles(first), before);
    unsafe { cb_shelf_free(second) };
    unsafe { cb_shelf_free(first) };
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn collections_group_books_and_show_up_on_the_rows() {
    let (lib, dir) = library("shelf-collections");
    let all = shelf(lib, &zeroed_query());
    let mut first = unsafe { std::mem::zeroed::<cb_book>() };
    assert_eq!(
        unsafe { cb_shelf_book(all, 0, &mut first) },
        cb_status::CB_OK
    );
    unsafe { cb_shelf_free(all) };

    let name = cstr("To Reread");
    let mut collection = 0i64;
    assert_eq!(
        unsafe { cb_library_create_collection(lib, name.as_ptr(), &mut collection) },
        cb_status::CB_OK,
        "{}",
        last_error()
    );
    assert!(collection > 0);
    // Idempotent on the name.
    let mut again = 0i64;
    assert_eq!(
        unsafe { cb_library_create_collection(lib, name.as_ptr(), &mut again) },
        cb_status::CB_OK
    );
    assert_eq!(again, collection);

    assert_eq!(
        unsafe { cb_library_add_to_collection(lib, first.id, collection) },
        cb_status::CB_OK
    );

    // The array idiom: probe for the count, then fill.
    let mut needed = 0usize;
    assert_eq!(
        unsafe { cb_library_collections(lib, std::ptr::null_mut(), 0, &mut needed) },
        cb_status::CB_ERR_BUFFER_TOO_SMALL
    );
    assert_eq!(needed, 1);
    let mut buf = vec![unsafe { std::mem::zeroed::<cb_collection>() }; needed];
    assert_eq!(
        unsafe { cb_library_collections(lib, buf.as_mut_ptr(), buf.len(), &mut needed) },
        cb_status::CB_OK
    );
    assert_eq!(buf[0].id, collection);
    assert_eq!(buf[0].books, 1);
    assert_eq!(
        read_string(|b, c, n| unsafe { cb_library_collection_name(lib, collection, b, c, n) })
            .as_deref(),
        Ok("To Reread")
    );

    // Filtering by it finds the one book, and the row names the
    // collection it is in without a second query per book.
    let narrowed = shelf(
        lib,
        &cb_book_query {
            collection,
            ..zeroed_query()
        },
    );
    assert_eq!(shelf_len(narrowed), 1);
    let mut row = unsafe { std::mem::zeroed::<cb_book>() };
    assert_eq!(
        unsafe { cb_shelf_book(narrowed, 0, &mut row) },
        cb_status::CB_OK
    );
    assert_eq!(row.collection_count, 1);
    let mut id = 0i64;
    assert_eq!(
        unsafe { cb_shelf_collection_id(narrowed, 0, 0, &mut id) },
        cb_status::CB_OK
    );
    assert_eq!(id, collection);
    assert_eq!(
        read_string(|b, c, n| unsafe { cb_shelf_collection_name(narrowed, 0, 0, b, c, n) })
            .as_deref(),
        Ok("To Reread")
    );
    unsafe { cb_shelf_free(narrowed) };

    // Deleting takes the grouping and leaves the books.
    assert_eq!(
        unsafe { cb_library_delete_collection(lib, collection) },
        cb_status::CB_OK
    );
    // A zero-capacity probe over an empty list is `CB_OK`, not
    // "too small": nothing is what fits in nothing.
    assert_eq!(
        unsafe { cb_library_collections(lib, std::ptr::null_mut(), 0, &mut needed) },
        cb_status::CB_OK
    );
    assert_eq!(needed, 0);
    let survivors = shelf(lib, &zeroed_query());
    assert_eq!(shelf_len(survivors), 3);
    unsafe { cb_shelf_free(survivors) };

    unsafe { cb_library_close(lib) };
    std::fs::remove_dir_all(&dir).ok();
}

/// The join a host needs: the session imported the book, so only it
/// knows which row that became.
#[test]
fn a_session_names_the_row_it_imported() {
    let dir = library_dir("shelf-join");
    let config = unsafe { cb_config_new(fonts()) };
    let dir_c = cstr(&dir.to_string_lossy());
    assert_eq!(
        unsafe { cb_config_set_library_dir(config, dir_c.as_ptr()) },
        cb_status::CB_OK
    );
    let path = fixture("epub/minimal.epub");
    let session = unsafe { cb_session_open_path(path.as_ptr(), config) };
    assert!(!session.is_null(), "{}", last_error());

    let mut id = 0i64;
    assert_eq!(
        unsafe { cb_session_book_id(session, &mut id) },
        cb_status::CB_OK,
        "{}",
        last_error()
    );
    assert!(id > 0);

    // Marking it finished from the shelf side is visible to a query,
    // which is the round trip a "mark as read" button makes.
    let mut lib: *mut cb_library = std::ptr::null_mut();
    assert_eq!(
        unsafe { cb_library_open(dir_c.as_ptr(), &mut lib) },
        cb_status::CB_OK
    );
    assert_eq!(
        unsafe { cb_library_set_finished(lib, id, true) },
        cb_status::CB_OK
    );
    let finished = shelf(
        lib,
        &cb_book_query {
            state: cb_reading_state::CB_STATE_FINISHED,
            ..zeroed_query()
        },
    );
    assert_eq!(shelf_len(finished), 1);
    let mut row = unsafe { std::mem::zeroed::<cb_book>() };
    assert_eq!(
        unsafe { cb_shelf_book(finished, 0, &mut row) },
        cb_status::CB_OK
    );
    assert_eq!(row.id, id);
    assert!(row.finished_at > 0);
    unsafe { cb_shelf_free(finished) };

    // And taking a book off the shelf is soft, so the row keeps its id.
    assert_eq!(unsafe { cb_library_delete_book(lib, id) }, cb_status::CB_OK);
    let remaining = shelf(lib, &zeroed_query());
    assert_eq!(shelf_len(remaining), 0);
    unsafe { cb_shelf_free(remaining) };

    unsafe { cb_library_close(lib) };
    unsafe { cb_session_close(session) };
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn shelf_calls_tolerate_null_the_way_the_rest_of_the_abi_does() {
    // Freeing null is legal, so an error path needs no cascade of tests.
    unsafe { cb_library_close(std::ptr::null_mut()) };
    unsafe { cb_shelf_free(std::ptr::null_mut()) };

    let mut len = 0usize;
    assert_eq!(
        unsafe { cb_shelf_len(std::ptr::null(), &mut len) },
        cb_status::CB_ERR_NULL_ARGUMENT
    );
    let mut id = 0i64;
    assert_eq!(
        unsafe { cb_session_book_id(std::ptr::null(), &mut id) },
        cb_status::CB_ERR_NULL_ARGUMENT
    );
    assert_eq!(
        unsafe { cb_library_delete_book(std::ptr::null_mut(), 1) },
        cb_status::CB_ERR_NULL_ARGUMENT
    );
    // A query with nowhere to put its answer is refused, not written
    // through.
    let mut shelf: *mut cb_shelf = std::ptr::null_mut();
    assert_eq!(
        unsafe { cb_library_query(std::ptr::null(), &zeroed_query(), &mut shelf) },
        cb_status::CB_ERR_NULL_ARGUMENT
    );
    assert!(shelf.is_null());
}

/// What `cb_book_query query = {0};` is in C.
fn zeroed_query() -> cb_book_query {
    cb_book_query {
        search: std::ptr::null(),
        series: std::ptr::null(),
        collection: 0,
        state: cb_reading_state::CB_STATE_ANY,
        sort: cb_sort::CB_SORT_ADDED,
        limit: 0,
        offset: 0,
    }
}

// ---- Sync across the boundary ----

/// Where a book's services are recorded and read back — the half a host
/// that browses catalogs itself needs before a sync can find anything.
#[test]
fn sync_targets_round_trip_through_the_shelf() {
    if cb_capabilities() & cb_capability::CB_CAP_LIBRARY as u32 == 0 {
        eprintln!("skipped: this build has no library");
        return;
    }
    let session = open("sync-targets", "epub/minimal.epub");
    let mut book = 0i64;
    assert_eq!(
        unsafe { cb_session_book_id(session, &mut book) },
        cb_status::CB_OK
    );
    unsafe { cb_session_close(session) };

    let dir = std::env::temp_dir().join(format!(
        "chapbook-ffi-test-{}-sync-targets",
        std::process::id()
    ));
    let dir_c = cstr(&dir.to_string_lossy());
    let mut lib: *mut cb_library = std::ptr::null_mut();
    assert_eq!(
        unsafe { cb_library_open(dir_c.as_ptr(), &mut lib) },
        cb_status::CB_OK
    );

    // A sideloaded book has no services, and says so per service —
    // straight from the probe call, before any buffer is offered.
    let mut needed = 0usize;
    assert_eq!(
        unsafe { cb_library_sync_progression_url(lib, book, std::ptr::null_mut(), 0, &mut needed) },
        cb_status::CB_ERR_UNAVAILABLE
    );

    let progression = cstr("http://127.0.0.1:1/progression");
    let container = cstr("http://127.0.0.1:1/annotations");
    assert_eq!(
        unsafe { cb_library_set_sync_targets(lib, book, progression.as_ptr(), container.as_ptr()) },
        cb_status::CB_OK
    );
    assert_eq!(
        read_string(|buf, cap, needed| unsafe {
            cb_library_sync_progression_url(lib, book, buf, cap, needed)
        })
        .as_deref(),
        Ok("http://127.0.0.1:1/progression")
    );
    assert_eq!(
        read_string(|buf, cap, needed| unsafe {
            cb_library_sync_annotation_container(lib, book, buf, cap, needed)
        })
        .as_deref(),
        Ok("http://127.0.0.1:1/annotations")
    );

    // Two nulls make it local again.
    assert_eq!(
        unsafe { cb_library_set_sync_targets(lib, book, std::ptr::null(), std::ptr::null()) },
        cb_status::CB_OK
    );
    assert_eq!(
        unsafe {
            cb_library_sync_annotation_container(lib, book, std::ptr::null_mut(), 0, &mut needed)
        },
        cb_status::CB_ERR_UNAVAILABLE
    );

    unsafe { cb_library_close(lib) };
    std::fs::remove_dir_all(&dir).ok();
}

static SYNC_WAKES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static SYNC_FINALIZED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Serializes the two tests that count finalizer runs.
///
/// `SYNC_FINALIZED` is process-global, and both of them read it, do one
/// thing that should finalize exactly once, and assert the count moved by
/// one. Run in parallel — the default — each sees the other's finalizer
/// and reads `+2`, which looks precisely like the ABI double-releasing a
/// host's object and is not: it is two tests sharing a counter. The
/// library tests serialize for the same reason, and the failure is worth
/// naming because the thing it impersonates would be serious.
static SYNC_COUNTER: std::sync::Mutex<()> = std::sync::Mutex::new(());

extern "C" fn sync_wake(_user: *mut std::ffi::c_void) {
    SYNC_WAKES.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
}

unsafe extern "C" fn sync_finalize(_user: *mut std::ffi::c_void) {
    SYNC_FINALIZED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
}

/// A transport with nobody on the other end, spelled as host callbacks.
unsafe extern "C" fn dead_get(
    _request: *const cb_http_request,
    response: *mut cb_http_response,
    _user: *mut std::ffi::c_void,
) {
    unsafe {
        cb_http_response_fail(response, c"nobody home".as_ptr());
    }
}

unsafe extern "C" fn dead_send(
    _method: *const c_char,
    _request: *const cb_http_request,
    _body: *const u8,
    _len: usize,
    response: *mut cb_http_response,
    _user: *mut std::ffi::c_void,
) {
    unsafe {
        cb_http_response_fail(response, c"nobody home".as_ptr());
    }
}

/// Wait out the worker: the next report, or a named failure.
fn next_report(sync: *mut cb_sync) -> cb_sync_report {
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
    for _ in 0..500 {
        match unsafe { cb_sync_next(sync, &mut report) } {
            cb_status::CB_OK => return report,
            cb_status::CB_ERR_UNAVAILABLE => {
                std::thread::sleep(std::time::Duration::from_millis(10))
            }
            other => panic!("cb_sync_next: {other:?}: {}", last_error()),
        }
    }
    panic!("no sync report within five seconds");
}

/// The whole reach, driven the way a phone would: record a service, open
/// a worker over host callbacks, ask for the shelf, drain what happened.
/// The service is dead, which is the honest first thing to prove — the
/// failure crosses the boundary as a report on the book, not a dead
/// batch and not silence.
#[test]
fn a_dead_service_crosses_as_a_report_not_a_dead_batch() {
    if cb_capabilities() & cb_capability::CB_CAP_SYNC as u32 == 0 {
        eprintln!("skipped: this build has no sync");
        return;
    }
    let _counting = SYNC_COUNTER.lock().unwrap_or_else(|e| e.into_inner());
    let session = open("sync-drive", "epub/minimal.epub");
    let mut book = 0i64;
    assert_eq!(
        unsafe { cb_session_book_id(session, &mut book) },
        cb_status::CB_OK
    );
    unsafe { cb_session_close(session) };

    let dir = std::env::temp_dir().join(format!(
        "chapbook-ffi-test-{}-sync-drive",
        std::process::id()
    ));
    let dir_c = cstr(&dir.to_string_lossy());
    let mut lib: *mut cb_library = std::ptr::null_mut();
    assert_eq!(
        unsafe { cb_library_open(dir_c.as_ptr(), &mut lib) },
        cb_status::CB_OK
    );
    let progression = cstr("http://127.0.0.1:1/progression");
    assert_eq!(
        unsafe { cb_library_set_sync_targets(lib, book, progression.as_ptr(), std::ptr::null()) },
        cb_status::CB_OK
    );
    unsafe { cb_library_close(lib) };

    let device_id = cstr("abi-test-device");
    let device_name = cstr("abi test");
    let mut sync: *mut cb_sync = std::ptr::null_mut();
    let finalized_before = SYNC_FINALIZED.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        unsafe {
            cb_sync_open(
                dir_c.as_ptr(),
                device_id.as_ptr(),
                device_name.as_ptr(),
                Some(dead_get),
                Some(dead_send),
                Some(sync_finalize),
                std::ptr::null_mut(),
                Some(sync_wake),
                std::ptr::null_mut(),
                &mut sync,
            )
        },
        cb_status::CB_OK,
        "{}",
        last_error()
    );
    assert!(!sync.is_null());

    assert_eq!(unsafe { cb_sync_request_all(sync) }, cb_status::CB_OK);

    let report = next_report(sync);
    assert_eq!(report.kind, cb_sync_kind::CB_SYNC_BOOK);
    assert_eq!(report.book, book);
    assert_eq!(
        report.position,
        cb_sync_position::CB_SYNC_POSITION_FAILED,
        "a dead transport fails the position half"
    );
    assert!(!report.detail.is_null(), "a failure explains itself");
    let detail = unsafe { std::ffi::CStr::from_ptr(report.detail) }
        .to_string_lossy()
        .into_owned();
    assert!(
        detail.contains("nobody home"),
        "the transport's own words cross: {detail}"
    );

    let finished = next_report(sync);
    assert_eq!(finished.kind, cb_sync_kind::CB_SYNC_FINISHED);
    assert_eq!(finished.books, 1);

    // Between batches the queue is empty, and says so quietly.
    let mut spare = finished;
    assert_eq!(
        unsafe { cb_sync_next(sync, &mut spare) },
        cb_status::CB_ERR_UNAVAILABLE
    );
    assert!(
        SYNC_WAKES.load(std::sync::atomic::Ordering::SeqCst) >= 2,
        "the waker fired per report"
    );

    unsafe { cb_sync_close(sync) };
    assert_eq!(
        SYNC_FINALIZED.load(std::sync::atomic::Ordering::SeqCst),
        finalized_before + 1,
        "closing the worker ran the transport finalizer exactly once"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// The refusal that keeps a host honest: a transport that can read but
/// not write is not a sync transport, and the finalizer still runs so
/// the ownership contract holds on the failure path.
#[test]
fn sync_refuses_a_transport_that_cannot_write() {
    if cb_capabilities() & cb_capability::CB_CAP_SYNC as u32 == 0 {
        eprintln!("skipped: this build has no sync");
        return;
    }
    let _counting = SYNC_COUNTER.lock().unwrap_or_else(|e| e.into_inner());
    let dir = library_dir("sync-readonly");
    let dir_c = cstr(&dir.to_string_lossy());
    let device_id = cstr("abi-test-device");
    let device_name = cstr("abi test");
    let mut sync: *mut cb_sync = std::ptr::null_mut();
    let finalized_before = SYNC_FINALIZED.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        unsafe {
            cb_sync_open(
                dir_c.as_ptr(),
                device_id.as_ptr(),
                device_name.as_ptr(),
                Some(dead_get),
                None,
                Some(sync_finalize),
                std::ptr::null_mut(),
                None,
                std::ptr::null_mut(),
                &mut sync,
            )
        },
        cb_status::CB_ERR_NULL_ARGUMENT
    );
    assert!(sync.is_null());
    assert_eq!(
        SYNC_FINALIZED.load(std::sync::atomic::Ordering::SeqCst),
        finalized_before + 1,
        "declining still released the host's object"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// The event drain: a page turn is a position change a progress UI can
/// see, and finishing the book is a transition, not a level.
#[test]
fn session_events_cross_oldest_first() {
    let session = open("events", "epub/minimal.epub");
    assert_eq!(
        unsafe { cb_session_set_metrics(session, metrics()) },
        cb_status::CB_OK
    );
    // Drain whatever opening produced (a restored-position resolve, loads)
    // so the assertions below start from quiet.
    let mut event = cb_session_event {
        kind: cb_session_event_kind::CB_SESSION_EVENT_BOOK_FINISHED,
        spine: 0,
        page: 0,
        message: std::ptr::null(),
    };
    while unsafe { cb_session_next_event(session, &mut event) } == cb_status::CB_OK {}

    let mut moved = false;
    assert_eq!(
        unsafe { cb_session_next_page(session, &mut moved) },
        cb_status::CB_OK
    );
    assert!(moved, "minimal.epub has a second page to turn to");

    assert_eq!(
        unsafe { cb_session_next_event(session, &mut event) },
        cb_status::CB_OK,
        "{}",
        last_error()
    );
    assert_eq!(
        event.kind,
        cb_session_event_kind::CB_SESSION_EVENT_POSITION_CHANGED
    );
    assert!(event.message.is_null(), "a move needs no explaining");

    // Ride to the end: the last turn that moves fires the finish.
    for _ in 0..200 {
        let mut moved = false;
        unsafe { cb_session_next_page(session, &mut moved) };
        if !moved {
            break;
        }
    }
    let mut finished = false;
    while unsafe { cb_session_next_event(session, &mut event) } == cb_status::CB_OK {
        if event.kind == cb_session_event_kind::CB_SESSION_EVENT_BOOK_FINISHED {
            finished = true;
        }
    }
    assert!(finished, "reaching the last page is a reportable event");
    assert_eq!(
        unsafe { cb_session_next_event(session, &mut event) },
        cb_status::CB_ERR_UNAVAILABLE,
        "quiet between drains"
    );
    unsafe { cb_session_close(session) };
}

/// The loop a touch reader runs, spelled across the boundary: long-press
/// selects a word, the selection becomes a highlight, the highlight is
/// found again under a finger, listed, recolored, jumped to, removed.
#[test]
fn a_mark_lives_its_whole_life_across_the_boundary() {
    if cb_capabilities() & cb_capability::CB_CAP_LIBRARY as u32 == 0 {
        eprintln!("skipped: marks persist through the library");
        return;
    }
    let session = open("marks", "epub/minimal.epub");
    assert_eq!(
        unsafe { cb_session_set_metrics(session, metrics()) },
        cb_status::CB_OK
    );

    // Where the text sits depends on the fixture fonts, so sweep for a
    // word rather than knowing a coordinate.
    let (mut wx, mut wy, mut selected) = (0f32, 0f32, false);
    'sweep: for y in (40..760).step_by(20) {
        for x in (40..560).step_by(20) {
            unsafe {
                cb_session_select_word_at(session, x as f32, y as f32, &mut selected);
            }
            if selected {
                wx = x as f32;
                wy = y as f32;
                break 'sweep;
            }
        }
    }
    assert!(selected, "a page of text has a word to long-press");

    let (mut start, mut end) = (0u32, 0u32);
    assert_eq!(
        unsafe { cb_session_selected_range(session, &mut start, &mut end) },
        cb_status::CB_OK
    );
    assert!(end > start, "a word is a non-empty range");
    let word = read_string(|buf, cap, needed| unsafe {
        cb_session_selected_text(session, buf, cap, needed)
    })
    .expect("selected text crosses");
    assert!(!word.trim().is_empty());

    // Grow the selection by exact range — the adjusted-handle move.
    unsafe { cb_session_select_range(session, start, end + 4) };

    let mut id = 0i64;
    assert_eq!(
        unsafe { cb_session_add_highlight(session, &mut id) },
        cb_status::CB_OK,
        "{}",
        last_error()
    );
    assert!(id > 0);
    // The highlight replaces the selection, and saying so is the shell's
    // move — the paint order is explicit.
    unsafe { cb_session_selection_clear(session) };
    let mut none = (0u32, 0u32);
    assert_eq!(
        unsafe { cb_session_selected_range(session, &mut none.0, &mut none.1) },
        cb_status::CB_ERR_UNAVAILABLE
    );

    // The tap that opens the recolor menu — aimed at the highlight's own
    // ink, since `highlight_at` hit-tests exactly (inside the marked
    // text, like a link) while the word sweep above was allowed to snap.
    let mut rect = cb_rect {
        x: 0.0,
        y: 0.0,
        w: 0.0,
        h: 0.0,
    };
    let mut filled = 0usize;
    assert_eq!(
        unsafe { cb_session_range_rects(session, start, end + 4, &mut rect, 1, &mut filled) },
        cb_status::CB_OK
    );
    let _ = (wx, wy);
    let mut found = 0i64;
    assert_eq!(
        unsafe {
            cb_session_highlight_at(
                session,
                rect.x + rect.w / 2.0,
                rect.y + rect.h / 2.0,
                &mut found,
            )
        },
        cb_status::CB_OK,
        "{}",
        last_error()
    );
    assert_eq!(found, id);

    // Listed, recolored, read back.
    let mut count = 0usize;
    assert_eq!(
        unsafe { cb_session_annotation_count(session, &mut count) },
        cb_status::CB_OK
    );
    assert_eq!(count, 1);
    let color = cstr("#ffcc00");
    assert_eq!(
        unsafe { cb_session_set_highlight_color(session, id, color.as_ptr()) },
        cb_status::CB_OK
    );
    let mut row = cb_annotation {
        id: 0,
        kind: cb_annotation_kind::CB_ANNOTATION_BOOKMARK,
        spine: 0,
        progression: 0.0,
        has_text: false,
        has_color: false,
    };
    assert_eq!(
        unsafe { cb_session_annotation(session, 0, &mut row) },
        cb_status::CB_OK
    );
    assert_eq!(row.id, id);
    assert_eq!(row.kind, cb_annotation_kind::CB_ANNOTATION_HIGHLIGHT);
    assert!(row.has_text && row.has_color);
    assert_eq!(
        read_string(|buf, cap, needed| unsafe {
            cb_session_annotation_color(session, 0, buf, cap, needed)
        })
        .as_deref(),
        Ok("#ffcc00")
    );
    let quote = read_string(|buf, cap, needed| unsafe {
        cb_session_annotation_text(session, 0, buf, cap, needed)
    })
    .expect("a highlight quotes its text");
    assert!(quote.contains(word.trim()), "{quote:?} carries {word:?}");

    // Jump to it from somewhere else, then remove it.
    let mut moved = false;
    unsafe { cb_session_next_page(session, &mut moved) };
    assert_eq!(
        unsafe { cb_session_goto_annotation(session, id, &mut moved) },
        cb_status::CB_OK
    );
    assert_eq!(
        unsafe { cb_session_remove_annotation(session, id) },
        cb_status::CB_OK
    );
    assert_eq!(
        unsafe { cb_session_annotation_count(session, &mut count) },
        cb_status::CB_OK
    );
    assert_eq!(count, 0);

    // An external link is the shell's to open, and says so quietly.
    let external = cstr("https://example.com/elsewhere");
    assert_eq!(
        unsafe { cb_session_follow_link(session, external.as_ptr(), &mut moved) },
        cb_status::CB_OK
    );
    assert!(!moved, "the engine does not browse");

    unsafe { cb_session_close(session) };
}

/// The pinch, across the boundary: refused on prose, honored on a comic,
/// pan falling through at fit.
#[test]
fn zoom_is_for_image_books_and_says_so() {
    if cb_capabilities() & cb_capability::CB_CAP_CBZ as u32 == 0 {
        eprintln!("skipped: this build opens no comics");
        return;
    }
    // Prose refuses: the gesture belongs to font size there.
    let session = open("zoom-epub", "epub/minimal.epub");
    assert_eq!(
        unsafe { cb_session_set_metrics(session, metrics()) },
        cb_status::CB_OK
    );
    let mut changed = true;
    assert_eq!(
        unsafe { cb_session_set_page_zoom(session, 2.0, 100.0, 100.0, &mut changed) },
        cb_status::CB_OK
    );
    assert!(!changed, "prose maps pinch to FontUp/FontDown instead");
    unsafe { cb_session_close(session) };

    // A comic zooms once its page has landed.
    let session = open("zoom-cbz", "cbz/minimal.cbz");
    assert_eq!(
        unsafe { cb_session_set_metrics(session, metrics()) },
        cb_status::CB_OK
    );
    let mut pending = true;
    for _ in 0..400 {
        let mut visible = false;
        unsafe {
            cb_session_poll_loaded(session, &mut visible);
            cb_session_has_pending_loads(session, &mut pending);
        }
        if !pending {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(!pending, "the first page decodes");

    // Pan at fit falls through, so a drag can mean a swipe turn.
    assert_eq!(
        unsafe { cb_session_pan_page(session, -30.0, 0.0, &mut changed) },
        cb_status::CB_OK
    );
    assert!(!changed);

    assert_eq!(
        unsafe { cb_session_set_page_zoom(session, 2.0, 300.0, 400.0, &mut changed) },
        cb_status::CB_OK
    );
    assert!(changed, "{}", last_error());
    let mut zoom = 0.0f32;
    assert_eq!(
        unsafe { cb_session_page_zoom(session, &mut zoom) },
        cb_status::CB_OK
    );
    assert_eq!(zoom, 2.0);
    assert_eq!(
        unsafe { cb_session_pan_page(session, -30.0, -10.0, &mut changed) },
        cb_status::CB_OK
    );
    assert!(changed, "a zoomed page pans");
    let (mut px, mut py) = (0.0f32, 0.0f32);
    assert_eq!(
        unsafe { cb_session_page_pan(session, &mut px, &mut py) },
        cb_status::CB_OK
    );
    assert!(px < 0.0 || py < 0.0, "the pan moved off origin");
    unsafe { cb_session_close(session) };
}

/// Contents, search, and the locator: the three ways a reader goes
/// somewhere on purpose, spelled the way a host writes them.
#[test]
fn a_host_can_reach_a_place_it_names() {
    let session = open("navigation", "epub/long.epub");
    assert_eq!(
        unsafe { cb_session_set_metrics(session, metrics()) },
        cb_status::CB_OK
    );

    // ---- Contents ----
    let mut count = 0usize;
    assert_eq!(
        unsafe { cb_session_toc_count(session, &mut count) },
        cb_status::CB_OK,
        "{}",
        last_error()
    );
    assert!(count > 1, "long.epub has contents");
    let mut entry = cb_toc_entry {
        depth: 0,
        spine: 0,
        has_spine: false,
        has_fragment: false,
    };
    assert_eq!(
        unsafe { cb_session_toc_entry(session, 0, &mut entry) },
        cb_status::CB_OK
    );
    assert_eq!(entry.depth, 0, "the first entry is top level");
    let label = read_string(|buf, cap, needed| unsafe {
        cb_session_toc_label(session, 0, buf, cap, needed)
    })
    .expect("an entry has a label");
    assert!(!label.trim().is_empty());
    // Past the end is an argument error, not a crash.
    assert_eq!(
        unsafe { cb_session_toc_entry(session, count, &mut entry) },
        cb_status::CB_ERR_INVALID_ARGUMENT
    );

    // Jump to the last entry and land in its unit.
    let mut last = cb_toc_entry {
        depth: 0,
        spine: 0,
        has_spine: false,
        has_fragment: false,
    };
    assert_eq!(
        unsafe { cb_session_toc_entry(session, count - 1, &mut last) },
        cb_status::CB_OK
    );
    let mut moved = false;
    assert_eq!(
        unsafe { cb_session_goto_toc(session, count - 1, &mut moved) },
        cb_status::CB_OK
    );
    if last.has_spine {
        assert!(moved, "an entry that points somewhere moves the reader");
        let mut at = cb_position { spine: 0, page: 0 };
        unsafe { cb_session_position(session, &mut at) };
        assert_eq!(at.spine, last.spine, "landed in the entry's unit");
    }

    // ---- The locator, and going back to one ----
    let (mut spine, mut offset) = (0usize, 0u32);
    assert_eq!(
        unsafe { cb_session_locator(session, &mut spine, &mut offset) },
        cb_status::CB_OK
    );
    let saved = (spine, offset);

    // Wander off, then return to the saved place exactly.
    assert_eq!(
        unsafe { cb_session_goto(session, 0, 0, &mut moved) },
        cb_status::CB_OK
    );
    assert!(moved);
    assert_eq!(
        unsafe { cb_session_goto(session, saved.0, saved.1, &mut moved) },
        cb_status::CB_OK
    );
    let (mut back_spine, mut back_offset) = (0usize, 0u32);
    unsafe { cb_session_locator(session, &mut back_spine, &mut back_offset) };
    assert_eq!(back_spine, saved.0, "a locator names the unit it named");
    assert!(
        back_offset <= saved.1,
        "and lands at or before its offset, never past it"
    );

    // A jump pushed a return position, so Back has somewhere to go.
    let mut can = false;
    assert_eq!(
        unsafe { cb_session_can_go_back(session, &mut can) },
        cb_status::CB_OK
    );
    assert!(can, "jumping is what fills the back stack");

    // A spine index the book does not have is a refusal, not a crash.
    assert_eq!(
        unsafe { cb_session_goto(session, 9999, 0, &mut moved) },
        cb_status::CB_OK
    );
    assert!(!moved);

    // ---- Search ----
    let query = cstr("the");
    let mut hits = 0usize;
    assert_eq!(
        unsafe { cb_session_search(session, query.as_ptr(), 10, &mut hits) },
        cb_status::CB_OK,
        "{}",
        last_error()
    );
    assert!(hits > 0 && hits <= 10, "the limit is honored: {hits}");

    let mut hit = cb_search_hit {
        spine: 0,
        start: 0,
        end: 0,
        match_start: 0,
        match_end: 0,
    };
    assert_eq!(
        unsafe { cb_session_search_hit(session, 0, &mut hit) },
        cb_status::CB_OK
    );
    assert!(hit.end > hit.start, "a match is a non-empty range");
    let context = read_string(|buf, cap, needed| unsafe {
        cb_session_search_context(session, 0, buf, cap, needed)
    })
    .expect("a hit carries context");
    // The match range indexes the context, so a host can embolden it.
    let matched: String = context
        .chars()
        .skip(hit.match_start as usize)
        .take((hit.match_end - hit.match_start) as usize)
        .collect();
    assert_eq!(
        matched.to_lowercase(),
        "the",
        "the match range names the match inside {context:?}"
    );

    // Going to a hit and painting it is the flow a search box runs.
    assert_eq!(
        unsafe { cb_session_goto(session, hit.spine, hit.start, &mut moved) },
        cb_status::CB_OK
    );
    assert_eq!(
        unsafe { cb_session_select_range(session, hit.start, hit.end) },
        cb_status::CB_OK
    );
    let (mut sel_start, mut sel_end) = (0u32, 0u32);
    assert_eq!(
        unsafe { cb_session_selected_range(session, &mut sel_start, &mut sel_end) },
        cb_status::CB_OK,
        "the hit is on the page and selected"
    );

    // Per-unit search is the worker-drivable half.
    assert_eq!(
        unsafe { cb_session_search_unit(session, 0, query.as_ptr(), &mut hits) },
        cb_status::CB_OK
    );
    let mut first = cb_search_hit {
        spine: 99,
        start: 0,
        end: 0,
        match_start: 0,
        match_end: 0,
    };
    if hits > 0 {
        unsafe { cb_session_search_hit(session, 0, &mut first) };
        assert_eq!(first.spine, 0, "a unit search stays in its unit");
    }
    // A unit the book does not have is refused by name.
    assert_eq!(
        unsafe { cb_session_search_unit(session, 9999, query.as_ptr(), &mut hits) },
        cb_status::CB_ERR_INVALID_ARGUMENT
    );

    unsafe { cb_session_close(session) };
}

/// The catalog flow a phone runs, in C shapes: point at a URL, read what
/// is there, drill into a section, and put a book on the shelf with its
/// sync services recorded.
///
/// Skipped unless `CHAPBOOK_TEST_OPDS` names a running catalog, because
/// a test suite that needs a server is a test suite that fails on a
/// laptop in a tunnel. `mocklib` is what this was written against.
#[test]
fn a_catalog_can_be_browsed_and_a_book_taken_from_it() {
    let Ok(root) = std::env::var("CHAPBOOK_TEST_OPDS") else {
        eprintln!("skipped: set CHAPBOOK_TEST_OPDS to a catalog URL to run this");
        return;
    };
    if cb_capabilities() & cb_capability::CB_CAP_OPDS as u32 == 0 {
        eprintln!("skipped: this build has no OPDS");
        return;
    }

    let mut catalog: *mut cb_catalog = std::ptr::null_mut();
    assert_eq!(
        unsafe { cb_catalog_open(None, None, None, std::ptr::null_mut(), &mut catalog,) },
        cb_status::CB_OK,
        "{}",
        last_error()
    );
    assert!(!catalog.is_null());

    // The root is a navigation feed: sections, not books.
    let url = cstr(&root);
    assert_eq!(
        unsafe { cb_catalog_fetch(catalog, url.as_ptr()) },
        cb_status::CB_OK,
        "{}",
        last_error()
    );
    let title =
        read_string(|buf, cap, needed| unsafe { cb_catalog_feed_title(catalog, buf, cap, needed) })
            .expect("a feed has a title");
    assert!(!title.trim().is_empty());

    let mut count = 0usize;
    assert_eq!(
        unsafe { cb_catalog_entry_count(catalog, &mut count) },
        cb_status::CB_OK
    );
    assert!(count > 0, "the root offers somewhere to go");

    let mut row = cb_entry {
        kind: cb_entry_kind::CB_ENTRY_NAVIGATION,
        author_count: 0,
        can_download: false,
        is_open_access: false,
        has_thumbnail: false,
        has_cover: false,
        has_summary: false,
        has_series: false,
        series_position: 0.0,
        has_series_position: false,
        syncs_position: false,
        syncs_annotations: false,
    };
    assert_eq!(
        unsafe { cb_catalog_entry(catalog, 0, &mut row) },
        cb_status::CB_OK
    );
    assert_eq!(
        row.kind,
        cb_entry_kind::CB_ENTRY_NAVIGATION,
        "a root's rows are places to go, not books"
    );

    // Drill in: the href of a navigation row is the next fetch.
    let section = read_string(|buf, cap, needed| unsafe {
        cb_catalog_entry_text(catalog, 0, cb_entry_field::CB_ENTRY_HREF, buf, cap, needed)
    })
    .expect("a navigation row points somewhere");
    let section_c = cstr(&section);
    assert_eq!(
        unsafe { cb_catalog_fetch(catalog, section_c.as_ptr()) },
        cb_status::CB_OK,
        "{}",
        last_error()
    );

    // Find a row that is actually a book.
    assert_eq!(
        unsafe { cb_catalog_entry_count(catalog, &mut count) },
        cb_status::CB_OK
    );
    let mut book_row = None;
    for index in 0..count {
        unsafe { cb_catalog_entry(catalog, index, &mut row) };
        if row.can_download {
            book_row = Some(index);
            break;
        }
    }
    let Some(index) = book_row else {
        panic!("a publication feed with nothing to download");
    };

    // A book row carries what a list draws.
    let title = read_string(|buf, cap, needed| unsafe {
        cb_catalog_entry_text(
            catalog,
            index,
            cb_entry_field::CB_ENTRY_TITLE,
            buf,
            cap,
            needed,
        )
    })
    .expect("a book has a title");
    assert!(!title.trim().is_empty());
    unsafe { cb_catalog_entry(catalog, index, &mut row) };
    if row.author_count > 0 {
        let author = read_string(|buf, cap, needed| unsafe {
            cb_catalog_entry_author(catalog, index, 0, buf, cap, needed)
        })
        .expect("an author reads back");
        assert!(!author.trim().is_empty());
    }
    if row.has_thumbnail {
        let thumb = read_string(|buf, cap, needed| unsafe {
            cb_catalog_entry_text(
                catalog,
                index,
                cb_entry_field::CB_ENTRY_THUMBNAIL_URL,
                buf,
                cap,
                needed,
            )
        })
        .expect("a thumbnail is a URL");
        assert!(
            thumb.starts_with("http"),
            "images cross as absolute URLs: {thumb}"
        );
    }

    // The money call: onto the shelf, with its services recorded.
    if cb_capabilities() & cb_capability::CB_CAP_LIBRARY as u32 != 0 {
        let dir = library_dir("catalog-download");
        let dir_c = cstr(&dir.to_string_lossy());
        let mut book_id = 0i64;
        assert_eq!(
            unsafe { cb_catalog_download(catalog, index, dir_c.as_ptr(), &mut book_id) },
            cb_status::CB_OK,
            "{}",
            last_error()
        );
        assert!(book_id > 0, "the download answers with its library row");

        // It is on the shelf, and it knows where it syncs — the gap this
        // whole module exists to close.
        let mut lib: *mut cb_library = std::ptr::null_mut();
        assert_eq!(
            unsafe { cb_library_open(dir_c.as_ptr(), &mut lib) },
            cb_status::CB_OK
        );
        let shelved = shelf(lib, &zeroed_query());
        let mut rows = 0usize;
        unsafe { cb_shelf_len(shelved, &mut rows) };
        assert_eq!(rows, 1, "one book, once");
        unsafe { cb_shelf_free(shelved) };
        if row.syncs_annotations {
            let mut needed = 0usize;
            assert_eq!(
                unsafe {
                    cb_library_sync_annotation_container(
                        lib,
                        book_id,
                        std::ptr::null_mut(),
                        0,
                        &mut needed,
                    )
                },
                cb_status::CB_ERR_BUFFER_TOO_SMALL,
                "an entry that advertises a container had it recorded"
            );
        }
        unsafe { cb_library_close(lib) };
        std::fs::remove_dir_all(&dir).ok();
    }

    unsafe { cb_catalog_close(catalog) };
}
