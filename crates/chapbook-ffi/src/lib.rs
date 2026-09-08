//! **The C ABI for chapbook.** Contract tier: this header is the artifact
//! every non-Rust host consumes, and breaking it breaks all of them at once.
//!
//! Kotlin reaches it through JNI or Panama, Swift through C interop
//! directly, a browser through a `wasm-bindgen` wrapper over the same core.
//! It is hand-written rather than generated because it is the one object
//! all three share, and because a generated binding dictates the API's
//! shape from the outside. UniFFI was the considered alternative — it
//! would have generated idiomatic Kotlin and Swift and deleted most of the
//! glue — and was rejected on purpose rather than by default: it is
//! MPL-2.0 and therefore a `deny.toml` decision, its WASM story is not the
//! one wanted here, and the shape it dictates is its own.
//!
//! # The rules this ABI keeps
//!
//! - **Codes are the contract, strings never are.** Every fallible call
//!   returns `cb_status`: zero for success, negative for failure, and the
//!   numbers are permanent. [`cb_last_error_message`] carries the
//!   human-readable half, and it is explicitly free to change.
//! - **Nothing crosses owned.** There is no `cb_free_string`. Strings are
//!   written into the caller's buffer, and the callee reports the size it
//!   needed — so a host can never free a Rust allocation with the wrong
//!   allocator.
//! - **Nothing unwinds out.** Every entry point runs inside a
//!   `catch_unwind`; a caught panic becomes `CB_ERR_PANIC` and leaves the
//!   process alive to report it.
//! - **One handle per session, and it is not shared.** `cb_session` may be
//!   moved between threads and must never be used from two at once.
//! - **Null is tolerated where it is meaningful.** Every `*_free` and
//!   `*_close` accepts null, so error paths need no cascade of tests.
//!
//! # Safety
//!
//! Every entry point that takes a pointer is declared `unsafe`, because
//! every one of them has preconditions C cannot check and Rust cannot
//! verify. They are the same preconditions in each case, which is why they
//! are stated here once rather than repeated on all forty-two — cbindgen
//! copies doc comments into `chapbook.h`, and a header carrying the same
//! paragraph forty-two times is a worse artifact, not a safer one.
//!
//! A caller must ensure that:
//!
//! - Every handle (`cb_session*`, `cb_config*`, `cb_font_source*`) came
//!   from the matching constructor in this ABI, and has not been freed,
//!   closed, or consumed. Handles documented as *consumed* — a
//!   `cb_font_source` given to [`cb_config_new`], a `cb_config` given to
//!   any `cb_session_open_*` — are dead on return **whether the call
//!   succeeded or not**, and must not be freed afterwards.
//! - Every `const char*` is NUL-terminated, valid UTF-8, and readable for
//!   the duration of the call.
//! - Every out-pointer is either null or writable and correctly aligned for
//!   its type. Null is reported as `CB_ERR_NULL_ARGUMENT` rather than
//!   written through, except where a null buffer with zero capacity is the
//!   documented way to ask a string's length.
//! - Every buffer pointer is valid for the length given, and is not aliased
//!   by memory this ABI already holds.
//! - No `cb_session` is used from two threads at once. It may be *moved*
//!   between threads freely.
//!
//! Passing null where a handle is expected is always reported, never
//! dereferenced; `*_free` and `*_close` accept null and do nothing.
//!
//! # What is deliberately absent
//!
//! The display list. Shells that rasterize for themselves still take
//! `frame()` in Rust and that path is unchanged, but it does not cross
//! here: it would drag `cosmic_text::fontdb::ID` across a boundary meant to
//! name no third-party type, and nothing needs it yet. Pixels only — see
//! [`cb_session_render_into`]. Accessibility once argued the other way and
//! no longer does; it wanted a text-runs-and-rects accessor, and that is
//! what it got — [`cb_session_page_text_run`] and its neighbours arrived
//! additively, after the shape was proven against AT-SPI, exactly as this
//! paragraph predicted.
//!
//! The table of contents, links and annotations are absent for the same
//! reason and not for a different one: the Contract tier means what ships
//! holds still, so this first header carries what a reader shell was
//! *shown* to need by the Android spike's five rungs. All of it is additive
//! later; none of it is blocked by anything here — the shelf
//! ([`cb_library_open`] and its neighbours) is the proof, having arrived
//! exactly that way once an app needed to open onto something other than a
//! book, and sync ([`cb_sync_open`] and its neighbours) arrived the same
//! way once the desktop application had shown the shape a driver needs.
//!
//! Two things the shelf deliberately did *not* bring with it. There is no
//! `cb_library_import`: a session imports the book it opens, so a host
//! adds to the shelf by reading, and [`cb_session_book_id`] is how it
//! learns which row that became. And there is no series listing — a row
//! carries its own series and `CB_SORT_SERIES` groups them, which is what
//! a browse-by-series screen is built from.
//!
//! # The header
//!
//! `include/chapbook.h` is generated by cbindgen, checked in, and guarded
//! as a golden by `tests/header.rs` — so drift between this file and the
//! header a host compiles against fails the gate rather than a link.
//! Regenerate deliberately with `UPDATE_FFI_HEADER=1 cargo test -p
//! chapbook-ffi` and read the diff.

// C ABI naming is C's, not Rust's: these names appear verbatim in a header
// that a person reads, and `cb_session_next_page` matching between the two
// matters more than any lint about case.
#![allow(non_camel_case_types)]
// The safety contract is identical across every entry point and is stated
// in full in the module documentation above, under *Safety*. Repeating it
// per function would duplicate it into the generated header forty-two
// times, which makes the deliverable worse rather than the code safer.
#![allow(clippy::missing_safety_doc)]

mod abi;
mod annotations;
mod config;
mod error;
mod http;
mod input;
mod library;
mod logging;
mod navigation;
mod session;
mod sync;

pub use annotations::{
    cb_annotation, cb_annotation_kind, cb_session_add_bookmark, cb_session_add_highlight,
    cb_session_add_note, cb_session_annotation, cb_session_annotation_color,
    cb_session_annotation_count, cb_session_annotation_text, cb_session_goto_annotation,
    cb_session_highlight_at, cb_session_remove_annotation, cb_session_set_highlight_color,
};
pub use config::{
    cb_config, cb_config_free, cb_config_new, cb_config_set_cache_budget,
    cb_config_set_library_dir, cb_font_source, cb_font_source_add_dir,
    cb_font_source_android_system, cb_font_source_embedded, cb_font_source_free,
    cb_font_source_host, cb_font_source_set_generics, cb_font_source_use_platform_generics,
};
pub use error::cb_status;
pub use http::{
    cb_config_set_http_transport, cb_http_download_fn, cb_http_finalize_fn, cb_http_get_fn,
    cb_http_header, cb_http_request, cb_http_response, cb_http_response_add_header,
    cb_http_response_append_body, cb_http_response_fail, cb_http_response_set_content_type,
    cb_http_response_set_status, cb_http_send_fn,
};
pub use input::{
    cb_action, cb_action_outcome, cb_char_default_action, cb_key, cb_key_default_action,
    cb_reading_direction, cb_session_apply, cb_session_reading_direction, cb_session_set_tap_zones,
    cb_session_tap_action,
};
pub use library::*;
pub use logging::{cb_log, cb_log_enabled, cb_log_fn, cb_log_level, cb_set_log_callback};
pub use navigation::{
    cb_search_hit, cb_session_can_go_back, cb_session_goto, cb_session_goto_anchor,
    cb_session_goto_toc, cb_session_locator, cb_session_search, cb_session_search_context,
    cb_session_search_hit, cb_session_search_unit, cb_session_toc_count, cb_session_toc_entry,
    cb_session_toc_label, cb_toc_entry,
};
pub use session::*;
pub use sync::{
    cb_sync, cb_sync_close, cb_sync_kind, cb_sync_next, cb_sync_open, cb_sync_position,
    cb_sync_report, cb_sync_request_all, cb_sync_request_book,
};

/// The version of this ABI, as `major * 10000 + minor * 100 + patch`.
///
/// A host that loads the library at runtime — which is every Android app —
/// cannot tell from a header which build it actually got. This is how it
/// asks. Tracks the workspace version.
#[no_mangle]
pub extern "C" fn cb_abi_version() -> u32 {
    error::guard(0, || {
        const fn part(s: &str) -> u32 {
            // `parse` is not const; the version string is validated by Cargo.
            let bytes = s.as_bytes();
            let mut value = 0u32;
            let mut i = 0;
            while i < bytes.len() {
                value = value * 10 + (bytes[i] - b'0') as u32;
                i += 1;
            }
            value
        }
        part(env!("CARGO_PKG_VERSION_MAJOR")) * 10_000
            + part(env!("CARGO_PKG_VERSION_MINOR")) * 100
            + part(env!("CARGO_PKG_VERSION_PATCH"))
    })
}

/// Which optional capabilities this build actually has.
///
/// A header cannot tell a host which `.so` it loaded, and the Android spike
/// is the argument: it built without the `library` feature for weeks, read
/// books perfectly, and silently remembered nothing — no error, because
/// there is nothing to error about. A build that can be configured down
/// must be able to say what it was configured to.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum cb_capability {
    /// Reading positions, annotations and settings persist. Without it a
    /// session reads and remembers nothing.
    CB_CAP_LIBRARY = 1,
    /// CBZ comic archives open.
    CB_CAP_CBZ = 2,
    /// PDFs open.
    CB_CAP_PDF = 4,
    /// OPDS catalogs and streamed comics open.
    CB_CAP_OPDS = 8,
    /// A transport is bundled. Without it a host must supply its own before
    /// anything can be fetched.
    CB_CAP_BUNDLED_HTTP = 16,
    /// SVG images rasterize. Without it they degrade silently (a missing
    /// `<img>`, an inline `<svg>` flattened to its text).
    CB_CAP_SVG = 32,
    /// Block MathML renders natively. Without it every `<math>` takes the
    /// EPUB altimg/alttext fallback.
    CB_CAP_MATHML = 64,
    /// Positions and marks reconcile with a book's services. Without it
    /// `cb_sync_open` declines and the library is read and written only
    /// locally.
    CB_CAP_SYNC = 128,
}

/// A bitmask of [`cb_capability`].
#[no_mangle]
pub extern "C" fn cb_capabilities() -> u32 {
    error::guard(0, || {
        let mut bits = 0;
        if cfg!(feature = "library") {
            bits |= cb_capability::CB_CAP_LIBRARY as u32;
        }
        if cfg!(feature = "cbz") {
            bits |= cb_capability::CB_CAP_CBZ as u32;
        }
        if cfg!(feature = "pdf") {
            bits |= cb_capability::CB_CAP_PDF as u32;
        }
        if cfg!(feature = "opds") {
            bits |= cb_capability::CB_CAP_OPDS as u32;
        }
        if cfg!(feature = "ureq") {
            bits |= cb_capability::CB_CAP_BUNDLED_HTTP as u32;
        }
        if cfg!(feature = "sync") {
            bits |= cb_capability::CB_CAP_SYNC as u32;
        }
        if cfg!(feature = "svg") {
            bits |= cb_capability::CB_CAP_SVG as u32;
        }
        if cfg!(feature = "mathml") {
            bits |= cb_capability::CB_CAP_MATHML as u32;
        }
        bits
    })
}
