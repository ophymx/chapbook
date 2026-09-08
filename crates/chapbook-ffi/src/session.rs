//! The session: opening a book, moving through it, and getting pixels.
//!
//! One opaque handle per open book, created and destroyed by explicit
//! calls, with no global state and no implicit singleton. `Session` is
//! `Send` and deliberately not `Sync`, and the handle inherits exactly
//! that: **a host may move it between threads and must never touch it from
//! two at once.** No lock is taken here to make that safe, because a lock
//! would be a silent tax on the single-threaded case that every shell
//! actually has.

use std::ffi::{c_char, c_void};

use chapbook_reader::chapbook_core::{
    EdgeSizes, Format, PageMetrics, Rotation, Size, Source, TapZones, Theme,
};
use chapbook_reader::Session;

use crate::abi::{slice_out, str_in, str_out};
use crate::config::cb_config;
use crate::error::{cb_status, clear_last_error, fail, from_error, guard};

/// An open book. Opaque.
pub struct cb_session {
    pub(crate) inner: Session,
    /// Session events drained from the engine and not yet handed out,
    /// plus the message the last-returned event's `message` pointer
    /// borrows — replaced on the next call, which bounds its lifetime.
    pub(crate) events: std::collections::VecDeque<chapbook_reader::SessionEvent>,
    pub(crate) event_message: Option<std::ffi::CString>,
    /// What the last search found, held so the per-index readers have
    /// something to read — replaced by the next search, dead with the
    /// session.
    pub(crate) hits: Vec<chapbook_reader::SearchHit>,
    /// The tap policy for this session — beside the session rather than a
    /// free-standing struct so the one field a host must *not* choose, the
    /// reading direction, is read off the book on every configuration and
    /// can never be handed in wrong. That shape was settled on a device,
    /// where every shell defaulting to `Ltr` was the bug about to ship.
    pub(crate) zones: TapZones,
}

/// Which reader opens the bytes. `CB_FORMAT_GUESS` decides from the bytes
/// themselves, and is the right answer even when a name is available — the
/// EPUB `mimetype` entry and the `%PDF` header do not lie and an extension
/// does.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum cb_format {
    CB_FORMAT_GUESS = 0,
    CB_FORMAT_EPUB = 1,
    CB_FORMAT_CBZ = 2,
    CB_FORMAT_PDF = 3,
}

impl From<cb_format> for Format {
    fn from(format: cb_format) -> Format {
        match format {
            cb_format::CB_FORMAT_GUESS => Format::Guess,
            cb_format::CB_FORMAT_EPUB => Format::Epub,
            cb_format::CB_FORMAT_CBZ => Format::Cbz,
            cb_format::CB_FORMAT_PDF => Format::Pdf,
        }
    }
}

/// Quarter-turns clockwise between the page as laid out and the panel it is
/// painted into. A property of the output, not the layout.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum cb_rotation {
    CB_ROTATION_NONE = 0,
    CB_ROTATION_QUARTER = 1,
    CB_ROTATION_HALF = 2,
    CB_ROTATION_THREE_QUARTER = 3,
}

impl From<cb_rotation> for Rotation {
    fn from(rotation: cb_rotation) -> Rotation {
        match rotation {
            cb_rotation::CB_ROTATION_NONE => Rotation::None,
            cb_rotation::CB_ROTATION_QUARTER => Rotation::Quarter,
            cb_rotation::CB_ROTATION_HALF => Rotation::Half,
            cb_rotation::CB_ROTATION_THREE_QUARTER => Rotation::ThreeQuarter,
        }
    }
}

/// Page ground and default text colours.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum cb_theme {
    CB_THEME_LIGHT = 0,
    CB_THEME_SEPIA = 1,
    CB_THEME_DARK = 2,
}

/// What kind of book is open. Comics and PDFs page as images, which is why
/// a host may want to know before offering text-shaped affordances.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum cb_book_kind {
    CB_BOOK_EPUB = 0,
    CB_BOOK_COMIC = 1,
    CB_BOOK_PDF = 2,
}

/// Where the reader is.
///
/// The pair, always. A page number alone is meaningless across a unit
/// boundary, and a host that stores one and compares it after a turn has a
/// bug the conformance harness exists to catch.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct cb_position {
    pub spine: u32,
    pub page: u32,
}

/// The page box, in logical units, plus the scale that turns it into
/// device pixels. Laying out at logical size and rasterizing at device
/// size is what keeps text a readable size on a dense panel.
///
/// **`width` and `height` are the page in *reading* orientation, not the
/// panel you paint into.** They are the same thing only while `rotation`
/// is `CB_ROTATION_NONE`. On a quarter or three-quarter turn the axes
/// swap, so a host with a 600x800 view that wants a turned page passes
/// 800x600 here and gets 600x800 back from `cb_session_render_size`.
///
/// Passing the view's own dimensions on a turn is not an error and will
/// not be reported as one: the page simply paginates to the wrong aspect,
/// and because you allocate from `cb_session_render_size` there is no
/// mismatch left for anything to catch. Rotation is a property of the
/// output; it must never change what the text reflows to.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct cb_metrics {
    pub width: f32,
    pub height: f32,
    pub margin_top: f32,
    pub margin_right: f32,
    pub margin_bottom: f32,
    pub margin_left: f32,
    pub dpi_scale: f32,
    pub rotation: cb_rotation,
}

/// How a page is typeset. `base_font_px` and `line_height` are the two a
/// reader adjusts; the rest a shell usually sets once.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct cb_settings {
    pub base_font_px: f32,
    /// Unitless multiplier.
    pub line_height: f32,
    pub justify: bool,
    /// Honour the publisher's stylesheets. Off leaves UA and user sheets.
    pub publisher_styles: bool,
    pub theme: cb_theme,
}

/// Where a settings change sticks.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum cb_settings_scope {
    /// Every book without an override of its own.
    CB_SCOPE_GLOBAL = 0,
    /// This book only.
    CB_SCOPE_THIS_BOOK = 1,
}

/// A dimension a caller may legally give us.
///
/// Spelled out rather than written `!(x > 0.0)` because the interesting
/// case is NaN, which arrives from C as easily as any other bit pattern and
/// compares false against everything. Infinity is refused for the same
/// reason: it is not a page size, and it propagates into layout as NaN.
fn usable(value: f32) -> bool {
    value.is_finite() && value > 0.0
}

// ---- Opening and closing ----

fn open_with(source: Source, config: *mut cb_config) -> *mut cb_session {
    if config.is_null() {
        fail(cb_status::CB_ERR_NULL_ARGUMENT, "config is null");
        return std::ptr::null_mut();
    }
    // SAFETY: a handle from `config`, consumed exactly here. Consumed even
    // when the open fails, which the header states, because the alternative
    // is a host that cannot tell whether it still owns the thing.
    let config: Box<cb_config> = unsafe { Box::from_raw(config) };
    match Session::open_with(source, config.inner) {
        Ok(inner) => {
            clear_last_error();
            // The default bands, in the direction the book declares;
            // everything else waits for `cb_session_set_tap_zones`.
            let zones = TapZones::new(inner.reading_direction());
            Box::into_raw(Box::new(cb_session {
                inner,
                zones,
                events: std::collections::VecDeque::new(),
                event_message: None,
                hits: Vec::new(),
            }))
        }
        Err(e) => {
            from_error(&e);
            std::ptr::null_mut()
        }
    }
}

/// Open a book from a filesystem path. **Consumes `config`** either way.
///
/// Returns null on failure; `cb_last_error_message` says why. The book is
/// imported into the library (when the config names one): copied, indexed,
/// and reopened at its stored reading position.
#[no_mangle]
pub unsafe extern "C" fn cb_session_open_path(
    path: *const c_char,
    config: *mut cb_config,
) -> *mut cb_session {
    guard(std::ptr::null_mut(), || {
        // SAFETY: the header's contract.
        let Some(path) = (unsafe { str_in(path, "path") }) else {
            cb_config_free_internal(config);
            return std::ptr::null_mut();
        };
        open_with(Source::Path(path.into()), config)
    })
}

/// Open a book from bytes the host already holds — a WASM `ArrayBuffer`, a
/// download it performed itself. The bytes are copied; the caller's buffer
/// is its own again on return.
///
/// **Consumes `config`.** Reaches the library by content: the bytes are
/// hashed and the book adopted — recorded, not copied — under the same
/// edition fingerprint a path import gets, so its position, annotations
/// and per-book settings persist. Keeping hold of the *file* for the next
/// launch stays the host's job.
#[no_mangle]
pub unsafe extern "C" fn cb_session_open_bytes(
    bytes: *const u8,
    len: usize,
    format: cb_format,
    config: *mut cb_config,
) -> *mut cb_session {
    guard(std::ptr::null_mut(), || {
        if bytes.is_null() {
            fail(cb_status::CB_ERR_NULL_ARGUMENT, "bytes is null");
            cb_config_free_internal(config);
            return std::ptr::null_mut();
        }
        // SAFETY: the header's contract — `len` readable bytes at `bytes`,
        // valid for the duration of this call.
        let copied = unsafe { std::slice::from_raw_parts(bytes, len) }.to_vec();
        open_with(
            Source::Bytes {
                format: format.into(),
                bytes: copied,
            },
            config,
        )
    })
}

/// Open a book from an already-open file descriptor — an Android
/// `content://` URI resolved through `ParcelFileDescriptor`, an iOS
/// security-scoped file.
///
/// **Takes ownership of `fd`** and closes it when the session is closed, so
/// the caller must have detached it. **Consumes `config`.** Reaches the
/// library the same way bytes do: hashed on open, adopted by fingerprint,
/// position and annotations persist. The descriptor is not something the
/// library can reopen, so re-resolving the bookmark or URI grant on the
/// next launch stays the caller's job — resolve first, then open.
///
/// Unix only: a descriptor is what Android and iOS hand out, and Windows
/// has no analogue worth guessing at from here.
#[cfg(unix)]
#[no_mangle]
pub unsafe extern "C" fn cb_session_open_fd(
    fd: i32,
    format: cb_format,
    config: *mut cb_config,
) -> *mut cb_session {
    guard(std::ptr::null_mut(), || {
        if fd < 0 {
            fail(cb_status::CB_ERR_INVALID_ARGUMENT, "fd is negative");
            cb_config_free_internal(config);
            return std::ptr::null_mut();
        }
        // SAFETY: the header's contract — an owned descriptor the caller
        // has given up, which this `File` closes on drop.
        let file = unsafe {
            use std::os::fd::FromRawFd;
            std::fs::File::from_raw_fd(fd)
        };
        open_with(
            Source::Reader {
                format: format.into(),
                reader: Box::new(file),
            },
            config,
        )
    })
}

/// Open an OPDS catalog URL as a streamed book. **Consumes `config`.**
///
/// The URL names a catalog feed or entry whose page-streaming link
/// (`vaemendis.net/opds-pse`) becomes the book; every page is fetched on
/// demand and cached under the library directory, so the config **must**
/// name one — without it there is nowhere for pages to land and the open
/// fails saying so.
///
/// Fetching goes through the config's transport: the one injected with
/// [`cb_config_set_http_transport`](crate::cb_config_set_http_transport),
/// or the bundled one when the build has it (`CB_CAP_BUNDLED_HTTP`). With
/// neither, the open fails with a message naming the missing piece. A
/// build without OPDS (`CB_CAP_OPDS`) reports `CB_ERR_FORMAT_NOT_BUILT`.
///
/// A 401 surfaces as an auth failure carrying the server's Authentication
/// Document in the error message, so a shell can put up a real login; the
/// credential store on the config is what answers it.
#[no_mangle]
pub unsafe extern "C" fn cb_session_open_url(
    url: *const c_char,
    config: *mut cb_config,
) -> *mut cb_session {
    guard(std::ptr::null_mut(), || {
        // SAFETY: the header's contract.
        let Some(url) = (unsafe { str_in(url, "url") }) else {
            cb_config_free_internal(config);
            return std::ptr::null_mut();
        };
        // `Source` sniffs the scheme itself, but a session opened from a
        // typo would fall back to "no such file", which points the caller
        // at the wrong problem.
        if !url.starts_with("http://") && !url.starts_with("https://") {
            fail(
                cb_status::CB_ERR_INVALID_ARGUMENT,
                "url must be http:// or https://; local books open through \
                 cb_session_open_path",
            );
            cb_config_free_internal(config);
            return std::ptr::null_mut();
        }
        open_with(Source::Url(url.into()), config)
    })
}

/// Free a config the caller handed us on a path that failed before
/// `open_with` could consume it. Keeps "consumes `config` either way" true.
fn cb_config_free_internal(config: *mut cb_config) {
    if !config.is_null() {
        // SAFETY: a handle from `config`, freed once.
        drop(unsafe { Box::from_raw(config) });
    }
}

/// Close a session and release everything it holds. Passing null is a
/// no-op. Every handle from a `cb_session_open_*` must reach this exactly
/// once.
///
/// **This blocks until the session's background work has finished.** An
/// image book loads its pages on a worker thread, and that thread holds
/// the publication — and therefore any host transport, and therefore the
/// host's own `user` context. Returning before it finished would hand a
/// host back control while its context was still live on a thread it
/// cannot see, and a host that then freed it — which is what the
/// ownership rule invites — would be freeing memory a page fetch is still
/// using.
///
/// So the wait is the contract, not an implementation detail: when this
/// returns, every callback the host installed has been called for the
/// last time and `finalize` has already run. The cost is that closing
/// during a slow fetch takes as long as that fetch, which is why a shell
/// tearing down in a hurry should prefer `cb_session_suspend`.
#[no_mangle]
pub unsafe extern "C" fn cb_session_close(session: *mut cb_session) {
    guard((), || {
        if !session.is_null() {
            // SAFETY: a handle from an open call, closed once.
            drop(unsafe { Box::from_raw(session) });
        }
    })
}

// ---- Plumbing shared by the accessors ----

macro_rules! session_mut {
    ($session:expr) => {
        // SAFETY: a handle from an open call, not yet closed.
        match unsafe { $session.as_mut() } {
            Some(session) => session,
            None => return fail(cb_status::CB_ERR_NULL_ARGUMENT, "session is null"),
        }
    };
}

macro_rules! session_ref {
    ($session:expr) => {
        // SAFETY: a handle from an open call, not yet closed.
        match unsafe { $session.as_ref() } {
            Some(session) => session,
            None => return fail(cb_status::CB_ERR_NULL_ARGUMENT, "session is null"),
        }
    };
}

/// Write an out-parameter, refusing a null destination.
macro_rules! out {
    ($ptr:expr, $value:expr, $what:literal) => {{
        if $ptr.is_null() {
            return fail(
                cb_status::CB_ERR_NULL_ARGUMENT,
                concat!($what, " out-pointer is null"),
            );
        }
        // SAFETY: checked non-null just above.
        unsafe { *$ptr = $value };
    }};
}

// ---- Diagnostics ----

/// The message behind the last failure **on this thread**, as a
/// NUL-terminated string.
///
/// Call with `cap` 0 and `buf` null to learn the size, then again with a
/// buffer. Empty when nothing has failed. **The text is not stable** — it
/// is for logs and bug reports; branch on the status code instead.
#[no_mangle]
pub unsafe extern "C" fn cb_last_error_message(
    buf: *mut c_char,
    cap: usize,
    needed: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        let message = crate::error::last_error_message();
        // SAFETY: the header's contract for the buffer triple.
        unsafe { str_out(&message, buf, cap, needed) }
    })
}

// ---- Metrics and layout ----

/// Give the session its page box. Required before anything paginates: a
/// session with no metrics has no pages, and reports `CB_ERR_UNAVAILABLE`
/// when asked for a size or a render.
#[no_mangle]
pub unsafe extern "C" fn cb_session_set_metrics(
    session: *mut cb_session,
    metrics: cb_metrics,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        let session = session_mut!(session);
        if !(usable(metrics.width) && usable(metrics.height) && usable(metrics.dpi_scale)) {
            return fail(
                cb_status::CB_ERR_INVALID_ARGUMENT,
                "width, height and dpi_scale must all be finite and positive",
            );
        }
        session.inner.set_metrics(PageMetrics {
            size: Size {
                w: metrics.width,
                h: metrics.height,
            },
            margins: EdgeSizes {
                top: metrics.margin_top,
                right: metrics.margin_right,
                bottom: metrics.margin_bottom,
                left: metrics.margin_left,
            },
            dpi_scale: metrics.dpi_scale,
            rotation: metrics.rotation.into(),
        });
        cb_status::CB_OK
    })
}

// ---- Navigation ----

/// Turn forward one page, crossing into the next unit at the end of this
/// one. `*moved` reports whether the position changed.
///
/// **Use `*moved`.** Do not compare positions across a turn: that is the
/// defect this out-parameter exists to prevent.
#[no_mangle]
pub unsafe extern "C" fn cb_session_next_page(
    session: *mut cb_session,
    moved: *mut bool,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        let session = session_mut!(session);
        let did = session.inner.next_page();
        out!(moved, did, "moved");
        cb_status::CB_OK
    })
}

/// Turn back one page. `*moved` reports whether the position changed.
#[no_mangle]
pub unsafe extern "C" fn cb_session_prev_page(
    session: *mut cb_session,
    moved: *mut bool,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        let session = session_mut!(session);
        let did = session.inner.prev_page();
        out!(moved, did, "moved");
        cb_status::CB_OK
    })
}

/// Skip to the start of the next unit. `*moved` reports whether it moved.
#[no_mangle]
pub unsafe extern "C" fn cb_session_next_unit(
    session: *mut cb_session,
    moved: *mut bool,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        let session = session_mut!(session);
        let did = session.inner.next_unit();
        out!(moved, did, "moved");
        cb_status::CB_OK
    })
}

/// Skip to the start of the previous unit. `*moved` reports whether it moved.
#[no_mangle]
pub unsafe extern "C" fn cb_session_prev_unit(
    session: *mut cb_session,
    moved: *mut bool,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        let session = session_mut!(session);
        let did = session.inner.prev_unit();
        out!(moved, did, "moved");
        cb_status::CB_OK
    })
}

/// Where the reader is, as the pair.
#[no_mangle]
pub unsafe extern "C" fn cb_session_position(
    session: *const cb_session,
    position: *mut cb_position,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        let session = session_ref!(session);
        let at = session.inner.position();
        out!(
            position,
            cb_position {
                spine: at.spine as u32,
                page: at.page as u32,
            },
            "position"
        );
        cb_status::CB_OK
    })
}

/// How many units the spine holds. Never compacted: a dangling idref keeps
/// its slot, because indices are locator identity.
#[no_mangle]
pub unsafe extern "C" fn cb_session_spine_len(
    session: *const cb_session,
    len: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        let session = session_ref!(session);
        out!(len, session.inner.spine_len(), "len");
        cb_status::CB_OK
    })
}

/// How many pages the current unit holds at the current metrics. Lays the
/// unit out if it has not been laid out yet, so it is not free.
#[no_mangle]
pub unsafe extern "C" fn cb_session_page_count(
    session: *mut cb_session,
    count: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        let session = session_mut!(session);
        let n = session.inner.page_count();
        out!(count, n, "count");
        cb_status::CB_OK
    })
}

// ---- Metadata ----

/// The book's title. Caller-allocates; see [`cb_last_error_message`] for
/// the two-call idiom.
#[no_mangle]
pub unsafe extern "C" fn cb_session_title(
    session: *const cb_session,
    buf: *mut c_char,
    cap: usize,
    needed: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        let session = session_ref!(session);
        // SAFETY: the header's contract for the buffer triple.
        unsafe { str_out(session.inner.title(), buf, cap, needed) }
    })
}

/// Whether the open book pages as text or as images.
#[no_mangle]
pub unsafe extern "C" fn cb_session_book_kind(
    session: *const cb_session,
    kind: *mut cb_book_kind,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        use chapbook_reader::chapbook_core::BookKind;
        let session = session_ref!(session);
        let value = match session.inner.kind() {
            BookKind::Epub => cb_book_kind::CB_BOOK_EPUB,
            BookKind::Comic => cb_book_kind::CB_BOOK_COMIC,
            BookKind::Pdf => cb_book_kind::CB_BOOK_PDF,
        };
        out!(kind, value, "kind");
        cb_status::CB_OK
    })
}

// ---- Settings ----

/// The settings in force for the open book.
///
/// The chosen font family is **not** here: it is a string, and this struct
/// is plain data a host can hold by value. Read it with
/// [`cb_session_font_family`] and set it with
/// [`cb_session_set_font_family`].
#[no_mangle]
pub unsafe extern "C" fn cb_session_settings(
    session: *const cb_session,
    settings: *mut cb_settings,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        let session = session_ref!(session);
        let current = session.inner.settings();
        out!(
            settings,
            cb_settings {
                base_font_px: current.base_font_px,
                line_height: current.line_height,
                justify: current.justify,
                publisher_styles: current.publisher_styles,
                theme: match current.theme {
                    Theme::Light => cb_theme::CB_THEME_LIGHT,
                    Theme::Sepia => cb_theme::CB_THEME_SEPIA,
                    Theme::Dark => cb_theme::CB_THEME_DARK,
                },
            },
            "settings"
        );
        cb_status::CB_OK
    })
}

/// Apply settings, keeping the reader's place across the reflow.
///
/// The chosen font family is preserved, not cleared — it does not travel
/// in `cb_settings` and is changed only by
/// [`cb_session_set_font_family`].
#[no_mangle]
pub unsafe extern "C" fn cb_session_set_settings(
    session: *mut cb_session,
    settings: cb_settings,
    scope: cb_settings_scope,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        use chapbook_reader::chapbook_core::ReadingSettings;
        use chapbook_reader::SettingsScope;
        let session = session_mut!(session);
        let current_family = session.inner.settings().font_family.clone();
        if !usable(settings.base_font_px) {
            return fail(
                cb_status::CB_ERR_INVALID_ARGUMENT,
                "base_font_px must be finite and positive",
            );
        }
        if !usable(settings.line_height) {
            // Real books ship `line-height: 0`, and the engine clamps it.
            // An explicit setting is a different thing: refuse it, rather
            // than silently disagree with the host about what it asked for.
            return fail(
                cb_status::CB_ERR_INVALID_ARGUMENT,
                "line_height must be finite and positive",
            );
        }
        session.inner.set_settings(
            ReadingSettings {
                base_font_px: settings.base_font_px,
                line_height: settings.line_height,
                justify: settings.justify,
                publisher_styles: settings.publisher_styles,
                theme: match settings.theme {
                    cb_theme::CB_THEME_LIGHT => Theme::Light,
                    cb_theme::CB_THEME_SEPIA => Theme::Sepia,
                    cb_theme::CB_THEME_DARK => Theme::Dark,
                },
                // Carried over, not reset. `cb_settings` is a plain
                // `#[repr(C)]` struct and the family is a string, so it
                // travels through its own calls; a host that never touches
                // the font must not clear it by setting the font size.
                font_family: current_family,
            },
            match scope {
                cb_settings_scope::CB_SCOPE_GLOBAL => SettingsScope::Global,
                cb_settings_scope::CB_SCOPE_THIS_BOOK => SettingsScope::ThisBook,
            },
        );
        cb_status::CB_OK
    })
}

/// How many font families this session can match.
///
/// The read-back half of the font source, and what a picker needs: a host
/// cannot offer a choice it cannot enumerate. Grows as chapters load,
/// because a book's own `@font-face` families join the database when their
/// unit lays out — so a host that caches this should refresh it after a
/// unit change rather than once at open.
#[no_mangle]
pub unsafe extern "C" fn cb_session_font_family_count(
    session: *const cb_session,
    count: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        let session = session_ref!(session);
        out!(count, session.inner.font_families().len(), "count");
        cb_status::CB_OK
    })
}

/// One available font family by index, sorted and deduplicated.
/// `CB_ERR_INVALID_ARGUMENT` past the count.
///
/// Caller-allocates; see [`cb_last_error_message`] for the two-call idiom.
#[no_mangle]
pub unsafe extern "C" fn cb_session_font_family_at(
    session: *const cb_session,
    index: usize,
    buf: *mut c_char,
    cap: usize,
    needed: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        let session = session_ref!(session);
        let families = session.inner.font_families();
        let Some(name) = families.get(index) else {
            return fail(
                cb_status::CB_ERR_INVALID_ARGUMENT,
                "font family index past the end",
            );
        };
        // SAFETY: the header's contract for the buffer triple.
        unsafe { str_out(name, buf, cap, needed) }
    })
}

/// The reader's chosen font family, or empty for the publisher's.
///
/// Caller-allocates; see [`cb_last_error_message`] for the two-call idiom.
/// Empty and unset are the same answer on purpose: a host that wants to
/// show "Publisher's font" in a picker tests for an empty string, which is
/// one branch rather than a sentinel it has to remember.
#[no_mangle]
pub unsafe extern "C" fn cb_session_font_family(
    session: *const cb_session,
    buf: *mut c_char,
    cap: usize,
    needed: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        let session = session_ref!(session);
        let family = session
            .inner
            .settings()
            .font_family
            .clone()
            .unwrap_or_default();
        // SAFETY: the header's contract for the buffer triple.
        unsafe { str_out(&family, buf, cap, needed) }
    })
}

/// Choose the typeface the reader sees, keeping their place across the
/// reflow.
///
/// `family` is a family name as [`cb_session_font_family_at`] reports them.
/// Null or empty returns the book to the publisher's own font. A name
/// nothing in the font database answers to is not an error — the cascade
/// moves on to the next family, exactly as it would for an unknown family
/// in a publisher's stylesheet — so a host that wants certainty should
/// offer only names it enumerated.
///
/// This beats the publisher's `font-family`, which is the point: nearly
/// every real EPUB sets one. Monospace is left alone, so code listings
/// stay legible.
#[no_mangle]
pub unsafe extern "C" fn cb_session_set_font_family(
    session: *mut cb_session,
    family: *const c_char,
    scope: cb_settings_scope,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        use chapbook_reader::SettingsScope;
        let session = session_mut!(session);
        let chosen = if family.is_null() {
            None
        } else {
            // SAFETY: the header's contract for a `const char *`.
            match unsafe { str_in(family, "family") } {
                Some(name) if !name.trim().is_empty() => Some(name.to_string()),
                Some(_) => None,
                None => return cb_status::CB_ERR_INVALID_UTF8,
            }
        };
        let settings = chapbook_reader::chapbook_core::ReadingSettings {
            font_family: chosen,
            ..session.inner.settings().clone()
        };
        session.inner.set_settings(
            settings,
            match scope {
                cb_settings_scope::CB_SCOPE_GLOBAL => SettingsScope::Global,
                cb_settings_scope::CB_SCOPE_THIS_BOOK => SettingsScope::ThisBook,
            },
        );
        cb_status::CB_OK
    })
}

// ---- Pixels ----

/// The device-pixel size a surface must be for [`cb_session_render_into`],
/// rotation included.
///
/// A host cannot allocate a surface without this, and must not compute it
/// itself: the logical-to-device round trip does not always land on the
/// pixel it started from, and `cb_session_render_into` refuses a
/// mismatched buffer rather than misdrawing into it.
///
/// `CB_ERR_UNAVAILABLE` until metrics are set.
#[no_mangle]
pub unsafe extern "C" fn cb_session_render_size(
    session: *const cb_session,
    width: *mut u32,
    height: *mut u32,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        let session = session_ref!(session);
        let Some((w, h)) = session.inner.render_size() else {
            return fail(
                cb_status::CB_ERR_UNAVAILABLE,
                "no render size yet: set metrics first",
            );
        };
        out!(width, w, "width");
        out!(height, h, "height");
        cb_status::CB_OK
    })
}

/// Rasterize the current page into a buffer the caller owns.
///
/// `width` and `height` must equal [`cb_session_render_size`]; `stride` is
/// the byte distance between rows and must be at least `width * 4`. Pixels
/// come back **premultiplied RGBA8888**, which is what Android's
/// `ARGB_8888` holds natively, so the common path converts nothing.
///
/// An unrotated page whose stride is exactly `width * 4` is rasterized
/// straight into `dst` with no intermediate and no copy. A rotated page or
/// a padded stride goes through a temporary and is copied row by row —
/// correct either way, free only in the first case, and every named
/// platform hits the first case.
///
/// A surface whose dimensions disagree with `cb_session_render_size`, a
/// stride narrower than a row, or a buffer too short for the two, are all
/// `CB_ERR_INVALID_ARGUMENT` and nothing is written. `CB_ERR_UNAVAILABLE`
/// means the size was right and there is simply nothing to draw yet.
#[no_mangle]
pub unsafe extern "C" fn cb_session_render_into(
    session: *mut cb_session,
    dst: *mut u8,
    len: usize,
    width: u32,
    height: u32,
    stride: usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        let session = session_mut!(session);
        if dst.is_null() {
            return fail(cb_status::CB_ERR_NULL_ARGUMENT, "dst is null");
        }
        // Checked against the session's own answer first, so "your surface
        // is the wrong shape" is distinguishable from "there is nothing to
        // draw". Writing a C host against this is what showed they were
        // being conflated: a caller that got the width wrong was told the
        // page was unavailable, which sends them looking in the wrong
        // place entirely.
        match session.inner.render_size() {
            None => {
                return fail(
                    cb_status::CB_ERR_UNAVAILABLE,
                    "no render size yet: set metrics first",
                )
            }
            Some((want_w, want_h)) if (want_w, want_h) != (width, height) => {
                return fail(
                    cb_status::CB_ERR_INVALID_ARGUMENT,
                    format!(
                        "surface is {width}x{height}, this page renders at                          {want_w}x{want_h} — ask cb_session_render_size"
                    ),
                )
            }
            Some(_) => {}
        }
        let Some(row) = (width as usize).checked_mul(4) else {
            return fail(cb_status::CB_ERR_INVALID_ARGUMENT, "width overflows a row");
        };
        if stride < row {
            return fail(
                cb_status::CB_ERR_INVALID_ARGUMENT,
                format!("stride {stride} is narrower than a {row}-byte row"),
            );
        }
        if len < stride.saturating_mul(height as usize) {
            return fail(
                cb_status::CB_ERR_INVALID_ARGUMENT,
                format!(
                    "buffer holds {len} bytes, {} needed",
                    stride.saturating_mul(height as usize)
                ),
            );
        }
        // SAFETY: the header's contract — `len` writable bytes at `dst`,
        // not aliased by anything this crate holds, valid for the call. The
        // bounds above are checked before it is ever formed.
        let buffer = unsafe { std::slice::from_raw_parts_mut(dst, len) };
        if session.inner.render_into(buffer, width, height, stride) {
            cb_status::CB_OK
        } else {
            // The size already agreed, so this is genuinely "nothing to
            // draw" — an unloaded unit, most likely.
            fail(
                cb_status::CB_ERR_UNAVAILABLE,
                "nothing to render for the current page",
            )
        }
    })
}

// ---- The text surface ----
//
// The current page's text with geometry — what an accessibility tree, a
// TTS engine, or a dictionary popup consumes. Deliberately not the
// display list, which carries glyph indices and no text. Everything here
// reads what is already laid out; indexes are stable only until the
// session mutates (a turn, a reflow, a settings change), so re-ask after
// anything that redraws.

/// A rectangle in page space: CSS px, origin at the page's top-left.
/// Rotation is a property of the output, so a host painting a rotated
/// panel maps these itself — the same transform it applies to the pixels.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct cb_rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl From<chapbook_reader::chapbook_core::Rect> for cb_rect {
    fn from(rect: chapbook_reader::chapbook_core::Rect) -> cb_rect {
        cb_rect {
            x: rect.origin.x,
            y: rect.origin.y,
            w: rect.size.w,
            h: rect.size.h,
        }
    }
}

/// One run of the current page's text: one visual line's geometry and
/// locator range. The text itself comes from
/// [`cb_session_page_text_run_text`] — split from the struct so nothing
/// here crosses owned. The run's char count is *not* `locator_end -
/// locator_start`: shaped text collapses whitespace, drops soft hyphens
/// and may add generated marks.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct cb_text_run {
    pub rect: cb_rect,
    /// Locator range `[start, end)` in the unit's locator space — the
    /// same offsets positions, selections and annotations use.
    pub locator_start: u32,
    pub locator_end: u32,
}

/// One word on the current page: where it sits in the speakable string
/// ([`cb_session_page_speakable_text`], char offsets) and in locator
/// space. A TTS engine reports progress as ranges into the string it was
/// handed; the locator range is how that progress becomes a highlight —
/// feed it to [`cb_session_range_rects`].
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct cb_word_span {
    /// Char range `[start, end)` within the speakable text.
    pub text_start: u32,
    pub text_end: u32,
    /// Locator range `[start, end)` in the unit's locator space.
    pub locator_start: u32,
    pub locator_end: u32,
}

/// How many text runs the current page holds. `CB_ERR_UNAVAILABLE` until
/// the page is laid out; zero for a laid-out page with nothing to speak
/// (a comic), which is a different answer on purpose.
#[no_mangle]
pub unsafe extern "C" fn cb_session_page_text_run_count(
    session: *const cb_session,
    count: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        let session = session_ref!(session);
        let Some(runs) = session.inner.page_text_runs() else {
            return fail(
                cb_status::CB_ERR_UNAVAILABLE,
                "no text surface yet: the page is not laid out",
            );
        };
        out!(count, runs.len(), "count");
        cb_status::CB_OK
    })
}

/// One text run's geometry and locator range, by index in reading order.
/// `CB_ERR_INVALID_ARGUMENT` past the count.
#[no_mangle]
pub unsafe extern "C" fn cb_session_page_text_run(
    session: *const cb_session,
    index: usize,
    run: *mut cb_text_run,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        let session = session_ref!(session);
        let Some(runs) = session.inner.page_text_runs() else {
            return fail(
                cb_status::CB_ERR_UNAVAILABLE,
                "no text surface yet: the page is not laid out",
            );
        };
        let Some(found) = runs.get(index) else {
            return fail(
                cb_status::CB_ERR_INVALID_ARGUMENT,
                format!(
                    "run index {index} out of range: the page has {}",
                    runs.len()
                ),
            );
        };
        out!(
            run,
            cb_text_run {
                rect: found.rect.into(),
                locator_start: found.locator_start,
                locator_end: found.locator_end,
            },
            "run"
        );
        cb_status::CB_OK
    })
}

/// One text run's text, by the same index. Caller-allocates; see
/// [`cb_last_error_message`] for the two-call idiom.
#[no_mangle]
pub unsafe extern "C" fn cb_session_page_text_run_text(
    session: *const cb_session,
    index: usize,
    buf: *mut c_char,
    cap: usize,
    needed: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        let session = session_ref!(session);
        let Some(runs) = session.inner.page_text_runs() else {
            return fail(
                cb_status::CB_ERR_UNAVAILABLE,
                "no text surface yet: the page is not laid out",
            );
        };
        let Some(found) = runs.get(index) else {
            return fail(
                cb_status::CB_ERR_INVALID_ARGUMENT,
                format!(
                    "run index {index} out of range: the page has {}",
                    runs.len()
                ),
            );
        };
        // SAFETY: the header's contract for the buffer triple.
        unsafe { str_out(&found.text, buf, cap, needed) }
    })
}

/// Page-space rects covering a locator range on the current page — one
/// per line the range touches; the geometry a word highlight or an
/// accessibility extent asks for. The two-call idiom: `needed` is always
/// the full count, a zero-capacity call sizes. Empty when the page is not
/// laid out or the range lies elsewhere.
#[no_mangle]
pub unsafe extern "C" fn cb_session_range_rects(
    session: *const cb_session,
    start: u32,
    end: u32,
    rects: *mut cb_rect,
    cap: usize,
    needed: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        let session = session_ref!(session);
        let found: Vec<cb_rect> = session
            .inner
            .range_rects(start, end)
            .into_iter()
            .map(cb_rect::from)
            .collect();
        // SAFETY: the header's contract for the buffer triple.
        unsafe { slice_out(&found, rects, cap, needed) }
    })
}

/// The current page as one speakable string — hand it to a TTS engine
/// whole, then map its progress reports back through the word table.
/// Whitespace is collapsed and soft hyphens dropped, so its offsets are
/// the word table's `text_*` fields and nothing else. Caller-allocates;
/// two-call idiom. `CB_ERR_UNAVAILABLE` until the page is laid out.
#[no_mangle]
pub unsafe extern "C" fn cb_session_page_speakable_text(
    session: *const cb_session,
    buf: *mut c_char,
    cap: usize,
    needed: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        let session = session_ref!(session);
        let Some(page) = session.inner.speakable_page() else {
            return fail(
                cb_status::CB_ERR_UNAVAILABLE,
                "no text surface yet: the page is not laid out",
            );
        };
        // SAFETY: the header's contract for the buffer triple.
        unsafe { str_out(&page.text, buf, cap, needed) }
    })
}

/// How many words the speakable page holds. Same availability rule as
/// [`cb_session_page_speakable_text`].
#[no_mangle]
pub unsafe extern "C" fn cb_session_page_word_count(
    session: *const cb_session,
    count: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        let session = session_ref!(session);
        let Some(page) = session.inner.speakable_page() else {
            return fail(
                cb_status::CB_ERR_UNAVAILABLE,
                "no text surface yet: the page is not laid out",
            );
        };
        out!(count, page.words.len(), "count");
        cb_status::CB_OK
    })
}

/// One word span, by index in reading order. `CB_ERR_INVALID_ARGUMENT`
/// past the count.
#[no_mangle]
pub unsafe extern "C" fn cb_session_page_word(
    session: *const cb_session,
    index: usize,
    span: *mut cb_word_span,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        let session = session_ref!(session);
        let Some(page) = session.inner.speakable_page() else {
            return fail(
                cb_status::CB_ERR_UNAVAILABLE,
                "no text surface yet: the page is not laid out",
            );
        };
        let Some(word) = page.words.get(index) else {
            return fail(
                cb_status::CB_ERR_INVALID_ARGUMENT,
                format!(
                    "word index {index} out of range: the page has {}",
                    page.words.len()
                ),
            );
        };
        out!(
            span,
            cb_word_span {
                text_start: word.text_start,
                text_end: word.text_end,
                locator_start: word.locator_start,
                locator_end: word.locator_end,
            },
            "span"
        );
        cb_status::CB_OK
    })
}

/// The word under a point in panel coordinates, as a locator range —
/// dictionary lookup's question. `CB_ERR_UNAVAILABLE` when no word is
/// there: off text, on whitespace, on bare punctuation. May lay the unit
/// out, hence the mutable handle.
#[no_mangle]
pub unsafe extern "C" fn cb_session_word_at(
    session: *mut cb_session,
    x: f32,
    y: f32,
    start: *mut u32,
    end: *mut u32,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        let session = session_mut!(session);
        let Some((from, to)) = session.inner.word_at(x, y) else {
            return fail(cb_status::CB_ERR_UNAVAILABLE, "no word under the point");
        };
        out!(start, from, "start");
        out!(end, to, "end");
        cb_status::CB_OK
    })
}

// ---- Lifecycle ----

/// Save the reading position and let go of everything reconstructible.
///
/// Call it from the last callback the platform guarantees — Android's
/// `onStop`, iOS's `willResignActive`. The session stays usable; the
/// library reopens by itself if something needs it.
#[no_mangle]
pub unsafe extern "C" fn cb_session_suspend(session: *mut cb_session) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        let session = session_mut!(session);
        session.inner.suspend();
        cb_status::CB_OK
    })
}

/// Drop cached layouts and decoded images. The current page is rebuilt on
/// the next render. For Android's `onTrimMemory` and iOS's memory warning.
#[no_mangle]
pub unsafe extern "C" fn cb_session_release_caches(session: *mut cb_session) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        let session = session_mut!(session);
        session.inner.release_caches();
        cb_status::CB_OK
    })
}

/// Bytes the caches currently hold.
#[no_mangle]
pub unsafe extern "C" fn cb_session_cache_bytes(
    session: *const cb_session,
    bytes: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        let session = session_ref!(session);
        out!(bytes, session.inner.cache_bytes(), "bytes");
        cb_status::CB_OK
    })
}

/// The ceiling those bytes are held under.
#[no_mangle]
pub unsafe extern "C" fn cb_session_cache_budget(
    session: *const cb_session,
    bytes: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        let session = session_ref!(session);
        out!(bytes, session.inner.cache_budget(), "bytes");
        cb_status::CB_OK
    })
}

// ---- Fonts ----

/// How many faces the font source actually produced.
///
/// Worth showing once at startup. Zero never reaches a host — a source that
/// resolves to nothing fails the open with `CB_ERR_FONT` — but the number
/// still distinguishes a device that found its system fonts from one
/// running on a single embedded face.
#[no_mangle]
pub unsafe extern "C" fn cb_session_font_face_count(
    session: *const cb_session,
    count: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        let session = session_ref!(session);
        out!(count, session.inner.font_report().faces, "count");
        cb_status::CB_OK
    })
}

/// Generic families that resolved to a name no loaded face carries, as
/// `generic=family`, one per line. Empty when everything resolved.
///
/// Not an error: a book that never asks for `cursive` never notices. It is
/// the difference between a device a developer can diagnose and one they
/// have to guess at.
#[no_mangle]
pub unsafe extern "C" fn cb_session_font_unresolved(
    session: *const cb_session,
    buf: *mut c_char,
    cap: usize,
    needed: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        let session = session_ref!(session);
        let text = session
            .inner
            .font_report()
            .unresolved_generics
            .iter()
            .map(|(generic, family)| format!("{generic}={family}"))
            .collect::<Vec<_>>()
            .join("\n");
        // SAFETY: the header's contract for the buffer triple.
        unsafe { str_out(&text, buf, cap, needed) }
    })
}

// ---- Asynchronous loads ----

/// Called when a background load finishes and there is something new to
/// show. **Fires on the loader thread**, not the host's UI thread: it must
/// only hand a token to whatever the host's main loop watches — a pipe, a
/// run-loop source, `Handler.post`. Attaching a JVM thread or touching UI
/// from inside it is the shape this comment exists to prevent.
pub type cb_wake_fn = Option<extern "C" fn(user: *mut c_void)>;

/// Install the wake callback. `user` is handed back untouched.
///
/// The pointer must stay valid until the session is closed or the waker is
/// replaced, and the callback may run on any thread.
#[no_mangle]
pub unsafe extern "C" fn cb_session_set_waker(
    session: *mut cb_session,
    wake: cb_wake_fn,
    user: *mut c_void,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        let session = session_mut!(session);
        let Some(wake) = wake else {
            return fail(cb_status::CB_ERR_NULL_ARGUMENT, "wake is null");
        };
        // The host promises `user` outlives the session; `usize` carries it
        // across the `Send + Sync` bound the waker requires, which a raw
        // pointer cannot satisfy on its own.
        let user = user as usize;
        session.inner.set_waker(move || wake(user as *mut c_void));
        cb_status::CB_OK
    })
}

/// Take delivery of anything the loader finished. `*changed` reports
/// whether the visible page is now different, and therefore whether a
/// repaint is worth doing.
#[no_mangle]
pub unsafe extern "C" fn cb_session_poll_loaded(
    session: *mut cb_session,
    changed: *mut bool,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        let session = session_mut!(session);
        let did = session.inner.poll_loaded();
        out!(changed, did, "changed");
        cb_status::CB_OK
    })
}

/// Whether any unit is still being loaded. A host that wants to show a
/// spinner asks this; one that just repaints on wake does not need it.
#[no_mangle]
pub unsafe extern "C" fn cb_session_has_pending_loads(
    session: *const cb_session,
    pending: *mut bool,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        let session = session_ref!(session);
        out!(pending, session.inner.has_pending_loads(), "pending");
        cb_status::CB_OK
    })
}

// ---- Session events ----

/// What kind of thing [`cb_session_next_event`] is reporting.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum cb_session_event_kind {
    /// A background unit finished decoding, prefetches included —
    /// [`cb_session_poll_loaded`] deliberately answers false for those,
    /// and a shell watching load progress wants both.
    CB_SESSION_EVENT_UNIT_LOADED = 0,
    /// A background unit failed and will not be retried. Without this a
    /// comic page that failed to download stays a placeholder forever
    /// with nothing able to say why.
    CB_SESSION_EVENT_UNIT_FAILED = 1,
    /// The reader is somewhere else — including moves the host did not
    /// make: a restored position resolving after open, a load landing
    /// that settles the page.
    CB_SESSION_EVENT_POSITION_CHANGED = 2,
    /// The reader reached the last page of the last unit. Fires on the
    /// transition and re-arms if they leave. Whether it means "mark as
    /// read" is the host's policy.
    CB_SESSION_EVENT_BOOK_FINISHED = 3,
}

/// One session event. Plain data; `message` is borrowed from the session
/// and stays valid until the next [`cb_session_next_event`] or the
/// session closes — copy it before either.
#[repr(C)]
pub struct cb_session_event {
    pub kind: cb_session_event_kind,
    /// The spine unit, for the two unit events and the position.
    pub spine: usize,
    /// The page, for `CB_SESSION_EVENT_POSITION_CHANGED`; 0 otherwise.
    pub page: usize,
    /// A unit failure's reason, for a person to read (free to change; do
    /// not match on it). Null for every other kind.
    pub message: *const c_char,
}

/// Take the next session event, oldest first. `CB_ERR_UNAVAILABLE` when
/// there is none, which is the ordinary answer, not an error worth
/// surfacing.
///
/// Everything the session wants a host to know that is *not* "repaint":
/// loads landing and failing, the position moving (a progress bar's and
/// a sync client's feed), the book finishing. Drain after a wake or an
/// action; the engine coalesces on its side, so a host cannot miss a
/// move by draining rarely.
#[no_mangle]
pub unsafe extern "C" fn cb_session_next_event(
    session: *mut cb_session,
    out: *mut cb_session_event,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        use chapbook_reader::SessionEvent;
        clear_last_error();
        let session = session_mut!(session);
        if out.is_null() {
            return fail(cb_status::CB_ERR_NULL_ARGUMENT, "out is null");
        }
        if session.events.is_empty() {
            let drained = session.inner.drain_events();
            session.events.extend(drained);
        }
        let Some(event) = session.events.pop_front() else {
            return fail(cb_status::CB_ERR_UNAVAILABLE, "no event waiting");
        };
        // The previous event's message dies here — the documented lifetime.
        session.event_message = None;
        let mut report = cb_session_event {
            kind: cb_session_event_kind::CB_SESSION_EVENT_BOOK_FINISHED,
            spine: 0,
            page: 0,
            message: std::ptr::null(),
        };
        match event {
            SessionEvent::UnitLoaded { spine } => {
                report.kind = cb_session_event_kind::CB_SESSION_EVENT_UNIT_LOADED;
                report.spine = spine;
            }
            SessionEvent::UnitFailed { spine, message } => {
                report.kind = cb_session_event_kind::CB_SESSION_EVENT_UNIT_FAILED;
                report.spine = spine;
                let owned = std::ffi::CString::new(message.replace('\0', " "))
                    .expect("NULs were just replaced");
                report.message = owned.as_ptr();
                session.event_message = Some(owned);
            }
            SessionEvent::PositionChanged { spine, page } => {
                report.kind = cb_session_event_kind::CB_SESSION_EVENT_POSITION_CHANGED;
                report.spine = spine;
                report.page = page;
            }
            SessionEvent::BookFinished => {
                report.kind = cb_session_event_kind::CB_SESSION_EVENT_BOOK_FINISHED;
            }
        }
        // SAFETY: checked non-null above.
        unsafe { *out = report };
        cb_status::CB_OK
    })
}

// ---- Selection ----
//
// The reading model's most gesture-shaped surface, and the last part of
// the desktop reader that could not be built from C. Coordinates are
// panel coordinates, the same numbers a pointer event carries, exactly
// as `cb_session_word_at` takes them; offsets are locator offsets in the
// current unit, the same space the text surface, positions and
// annotations already share across this boundary.

/// Anchor a selection at a point. `*started` reports whether text was
/// there to anchor on — a press on bare page starts nothing, and a shell
/// that treats that as a tap wants to know. The anchor is empty until a
/// drag extends it; a press that never moves should be cleared rather
/// than left to outlive the page.
#[no_mangle]
pub unsafe extern "C" fn cb_session_selection_begin(
    session: *mut cb_session,
    x: f32,
    y: f32,
    started: *mut bool,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        let session = session_mut!(session);
        let did = session.inner.selection_begin(x, y);
        out!(started, did, "started");
        cb_status::CB_OK
    })
}

/// Extend the selection to a point — the move half of press-drag, and
/// equally the move half of dragging a selection handle.
#[no_mangle]
pub unsafe extern "C" fn cb_session_selection_drag(
    session: *mut cb_session,
    x: f32,
    y: f32,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        let session = session_mut!(session);
        session.inner.selection_drag(x, y);
        cb_status::CB_OK
    })
}

/// Select the word under a point — what a long press means on glass.
/// `*selected` reports whether a word was there. The selected range then
/// answers through [`cb_session_selected_range`], and its geometry
/// through [`cb_session_range_rects`] — which is how a shell draws its
/// grab handles.
#[no_mangle]
pub unsafe extern "C" fn cb_session_select_word_at(
    session: *mut cb_session,
    x: f32,
    y: f32,
    selected: *mut bool,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        let session = session_mut!(session);
        let did = session.inner.select_word_at(x, y);
        out!(selected, did, "selected");
        cb_status::CB_OK
    })
}

/// Select an exact locator range — how a search hit or an adjusted
/// handle position becomes the selection. Offsets beyond the unit's text
/// clamp rather than fail, matching the engine.
#[no_mangle]
pub unsafe extern "C" fn cb_session_select_range(
    session: *mut cb_session,
    start: u32,
    end: u32,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        let session = session_mut!(session);
        session.inner.select_range(start, end);
        cb_status::CB_OK
    })
}

/// Drop the selection. A no-op when there is none.
#[no_mangle]
pub unsafe extern "C" fn cb_session_selection_clear(session: *mut cb_session) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        let session = session_mut!(session);
        session.inner.selection_clear();
        cb_status::CB_OK
    })
}

/// The selection as a locator range, `CB_ERR_UNAVAILABLE` when there is
/// none — including the empty anchor a press leaves before any drag,
/// which is deliberately not a selection yet.
#[no_mangle]
pub unsafe extern "C" fn cb_session_selected_range(
    session: *const cb_session,
    start: *mut u32,
    end: *mut u32,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        // SAFETY: a handle from an open call, not yet closed.
        let Some(session) = (unsafe { session.as_ref() }) else {
            return fail(cb_status::CB_ERR_NULL_ARGUMENT, "session is null");
        };
        let Some((from, to)) = session.inner.selected_range() else {
            return fail(cb_status::CB_ERR_UNAVAILABLE, "nothing selected");
        };
        out!(start, from, "start");
        out!(end, to, "end");
        cb_status::CB_OK
    })
}

/// The selected text, whitespace collapsed the way a clipboard wants it.
/// `CB_ERR_UNAVAILABLE` when nothing is selected.
#[no_mangle]
pub unsafe extern "C" fn cb_session_selected_text(
    session: *const cb_session,
    buf: *mut c_char,
    cap: usize,
    needed: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        // SAFETY: a handle from an open call, not yet closed.
        let Some(session) = (unsafe { session.as_ref() }) else {
            return fail(cb_status::CB_ERR_NULL_ARGUMENT, "session is null");
        };
        let Some(text) = session.inner.selected_text() else {
            return fail(cb_status::CB_ERR_UNAVAILABLE, "nothing selected");
        };
        // SAFETY: the header's contract for the buffer triple.
        unsafe { str_out(&text, buf, cap, needed) }
    })
}

// ---- Links ----

/// The link under a point, as the href the book wrote.
/// `CB_ERR_UNAVAILABLE` when the point is not on a link. A shell checks
/// this before starting a selection, so a press on a link follows it —
/// the ordering `docs/SHELLS.md` specifies.
#[no_mangle]
pub unsafe extern "C" fn cb_session_link_at(
    session: *mut cb_session,
    x: f32,
    y: f32,
    buf: *mut c_char,
    cap: usize,
    needed: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        let session = session_mut!(session);
        let Some(href) = session.inner.link_at(x, y) else {
            return fail(cb_status::CB_ERR_UNAVAILABLE, "no link under the point");
        };
        // SAFETY: the header's contract for the buffer triple.
        unsafe { str_out(&href, buf, cap, needed) }
    })
}

/// Follow an href — one [`cb_session_link_at`] answered, or a TOC
/// entry's. `*moved` reports whether the reader went anywhere; an
/// external `http(s)` href answers false and is the shell's to open in a
/// browser. A followed link pushes the return position for the `Back`
/// action.
#[no_mangle]
pub unsafe extern "C" fn cb_session_follow_link(
    session: *mut cb_session,
    href: *const c_char,
    moved: *mut bool,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        let session = session_mut!(session);
        // SAFETY: the header's contract.
        let Some(href) = (unsafe { crate::abi::str_in(href, "href") }) else {
            return cb_status::CB_ERR_NULL_ARGUMENT;
        };
        let did = session.inner.follow_link(href);
        out!(moved, did, "moved");
        cb_status::CB_OK
    })
}

// ---- Page zoom (image books) ----

/// Zoom the page around a focal point in panel coordinates — the pinch.
/// Image books only, clamped to `[1.0, 8.0]`, 1.0 returning to fit;
/// `*changed` reports whether the view moved. **Always false on
/// reflowable text**, where the same gesture means "make the text
/// bigger" — a settings change the shell maps to the `FontUp`/`FontDown`
/// actions itself. Zoom is view state: nothing persists it, and it
/// survives a page turn on purpose (a shell wanting turn-resets sets
/// 1.0 on turn).
///
/// Input crossing this boundary is mapped through the zoom
/// automatically. Output geometry — `cb_session_range_rects`, the text
/// surface — stays in fit-page space; a shell drawing overlays on a
/// zoomed page maps forward with [`cb_session_page_zoom`] and
/// [`cb_session_page_pan`]: `view = fit * zoom + pan`.
#[no_mangle]
pub unsafe extern "C" fn cb_session_set_page_zoom(
    session: *mut cb_session,
    zoom: f32,
    focus_x: f32,
    focus_y: f32,
    changed: *mut bool,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        let session = session_mut!(session);
        let did = session.inner.set_page_zoom(zoom, focus_x, focus_y);
        out!(changed, did, "changed");
        cb_status::CB_OK
    })
}

/// Pan the zoomed page by a pointer delta in panel coordinates, clamped
/// at the page's edges. `*changed` is false at fit — how a shell knows
/// the same drag should fall through to whatever an unzoomed drag means
/// (a selection, a swipe turn).
#[no_mangle]
pub unsafe extern "C" fn cb_session_pan_page(
    session: *mut cb_session,
    dx: f32,
    dy: f32,
    changed: *mut bool,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        let session = session_mut!(session);
        let did = session.inner.pan_page(dx, dy);
        out!(changed, did, "changed");
        cb_status::CB_OK
    })
}

/// The current zoom, 1.0 at fit.
#[no_mangle]
pub unsafe extern "C" fn cb_session_page_zoom(
    session: *const cb_session,
    zoom: *mut f32,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        // SAFETY: a handle from an open call, not yet closed.
        let Some(session) = (unsafe { session.as_ref() }) else {
            return fail(cb_status::CB_ERR_NULL_ARGUMENT, "session is null");
        };
        out!(zoom, session.inner.page_zoom(), "zoom");
        cb_status::CB_OK
    })
}

/// The current pan in page units — with the zoom, the forward map for a
/// shell's own overlays.
#[no_mangle]
pub unsafe extern "C" fn cb_session_page_pan(
    session: *const cb_session,
    x: *mut f32,
    y: *mut f32,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        // SAFETY: a handle from an open call, not yet closed.
        let Some(session) = (unsafe { session.as_ref() }) else {
            return fail(cb_status::CB_ERR_NULL_ARGUMENT, "session is null");
        };
        let (px, py) = session.inner.page_pan();
        out!(x, px, "x");
        out!(y, py, "y");
        cb_status::CB_OK
    })
}
