//! Status codes, the last-error string, and the panic guard.
//!
//! Three rules hold across every entry point in this crate, and they are
//! the reason this module exists before any of them:
//!
//! 1. **A code is the contract; a message never is.** `cb_status` is
//!    numbered explicitly and those numbers are Contract tier. The string
//!    behind [`cb_last_error_message`] is for logs and bug reports, and is
//!    free to change in any release.
//! 2. **Nothing unwinds across the boundary.** A panic reaching an
//!    `extern "C"` frame is undefined behaviour, and under `panic = "abort"`
//!    it is a process kill the host cannot observe or attribute. Every
//!    entry point runs inside [`guard`].
//! 3. **Nothing crosses owned.** Strings are written into a caller's buffer
//!    and the callee reports the length it needed, so there is no
//!    `cb_free_string` and therefore no chance of a host freeing a Rust
//!    allocation with the wrong allocator.

use std::cell::RefCell;
use std::panic::{catch_unwind, AssertUnwindSafe};

use chapbook_reader::chapbook_core::ChapbookError;

/// What every fallible entry point returns. Zero is success; every failure
/// is negative, so `if (rc < 0)` is the whole host-side test.
///
/// The numbers are deliberate and permanent. New conditions take new
/// numbers; none is ever reused for a different meaning, and none is
/// renumbered. Grouped so the ranges mean something: `-1..-9` are faults in
/// the *call itself*, which a correct host never sees, and `-10` down are
/// conditions from the engine, which it must handle.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum cb_status {
    CB_OK = 0,

    /// A panic was caught at the boundary. One code, not a family: a panic
    /// here is a chapbook bug, not a condition worth enumerating. The
    /// message names the payload where it could be recovered.
    CB_ERR_PANIC = -1,
    /// A pointer argument that may not be null was null.
    CB_ERR_NULL_ARGUMENT = -2,
    /// A `const char*` was not valid UTF-8, or had no terminator in range.
    CB_ERR_INVALID_UTF8 = -3,
    /// The output buffer was too small. `*needed` holds the byte count that
    /// would have sufficed, including the NUL.
    CB_ERR_BUFFER_TOO_SMALL = -4,
    /// An argument was well-formed but not usable: an out-of-range enum, a
    /// zero-sized surface, a negative descriptor.
    CB_ERR_INVALID_ARGUMENT = -5,
    /// The call does not apply right now — asking a session with no metrics
    /// for its render size, or for a page it has not laid out.
    CB_ERR_UNAVAILABLE = -6,

    CB_ERR_BOOK_OPEN = -10,
    CB_ERR_BOOK_MALFORMED = -11,
    CB_ERR_RESOURCE_NOT_FOUND = -12,
    CB_ERR_FIXED_LAYOUT_UNSUPPORTED = -13,
    /// The format is one chapbook implements and this build left out. A
    /// distinct code because it is a packaging mistake, not a bad book, and
    /// a host that reports it as corruption sends the reader hunting for a
    /// problem in their file.
    CB_ERR_FORMAT_NOT_BUILT = -14,
    CB_ERR_SPINE_OUT_OF_RANGE = -15,
    CB_ERR_PARSE = -16,
    CB_ERR_STYLE = -17,
    CB_ERR_LAYOUT = -18,
    CB_ERR_FONT = -19,
    CB_ERR_CFI = -20,
    CB_ERR_NETWORK = -21,
    CB_ERR_OPDS = -22,
    CB_ERR_LIBRARY = -23,
    CB_ERR_CREDENTIAL = -24,
    CB_ERR_PANEL = -25,
    CB_ERR_IO = -26,
    /// The catalog wants credentials, and said so with an
    /// authentication document — read it with the `cb_catalog_auth_*`
    /// calls, put up a native login, set the credential, and fetch
    /// again. A response, not a failure: it is how OPDS says "who are
    /// you", and treating it as an error is what produces a reader that
    /// cannot open a subscription catalog at all.
    CB_ERR_AUTH_REQUIRED = -27,
}

impl cb_status {
    pub(crate) fn of(error: &ChapbookError) -> cb_status {
        match error {
            ChapbookError::BookOpen { .. } => cb_status::CB_ERR_BOOK_OPEN,
            ChapbookError::BookMalformed(_) => cb_status::CB_ERR_BOOK_MALFORMED,
            ChapbookError::ResourceNotFound(_) => cb_status::CB_ERR_RESOURCE_NOT_FOUND,
            ChapbookError::FixedLayoutUnsupported => cb_status::CB_ERR_FIXED_LAYOUT_UNSUPPORTED,
            ChapbookError::FormatNotBuilt(_) => cb_status::CB_ERR_FORMAT_NOT_BUILT,
            ChapbookError::SpineOutOfRange(_) => cb_status::CB_ERR_SPINE_OUT_OF_RANGE,
            ChapbookError::Parse(_) => cb_status::CB_ERR_PARSE,
            ChapbookError::Style(_) => cb_status::CB_ERR_STYLE,
            ChapbookError::Layout(_) => cb_status::CB_ERR_LAYOUT,
            ChapbookError::Font(_) => cb_status::CB_ERR_FONT,
            ChapbookError::Cfi(_) => cb_status::CB_ERR_CFI,
            ChapbookError::Network(_) => cb_status::CB_ERR_NETWORK,
            ChapbookError::Opds(_) => cb_status::CB_ERR_OPDS,
            ChapbookError::Library(_) => cb_status::CB_ERR_LIBRARY,
            ChapbookError::Credential(_) => cb_status::CB_ERR_CREDENTIAL,
            ChapbookError::Panel(_) => cb_status::CB_ERR_PANEL,
            ChapbookError::Io(_) => cb_status::CB_ERR_IO,
        }
    }
}

thread_local! {
    /// The last failure reported on *this thread*.
    ///
    /// The obvious design hangs this off the session, and building it
    /// is what showed why it cannot: the failures a host most needs
    /// explained — a book that would not open, a font source that resolved
    /// to nothing, a config it built wrong — are exactly the ones where no
    /// session exists to ask. Thread-local covers those and the session
    /// case both, and it is per-thread rather than global because a session
    /// is `Send`: two threads may each hold one, and neither should be able
    /// to overwrite the other's diagnosis.
    static LAST_ERROR: RefCell<String> = const { RefCell::new(String::new()) };
}

pub(crate) fn set_last_error(message: impl Into<String>) {
    let message = message.into();
    log::debug!("ffi: {message}");
    LAST_ERROR.with(|slot| *slot.borrow_mut() = message);
}

pub(crate) fn clear_last_error() {
    LAST_ERROR.with(|slot| slot.borrow_mut().clear());
}

pub(crate) fn last_error_message() -> String {
    LAST_ERROR.with(|slot| slot.borrow().clone())
}

pub(crate) fn fail(status: cb_status, message: impl Into<String>) -> cb_status {
    set_last_error(message);
    status
}

pub(crate) fn from_error(error: &ChapbookError) -> cb_status {
    fail(cb_status::of(error), error.to_string())
}

/// Run `body` with unwinding stopped at the boundary.
///
/// Every `extern "C"` function in this crate is one of these and nothing
/// else, which is the only way the property stays true as functions are
/// added: a new entry point that forgets the guard is visibly shaped
/// differently from its neighbours.
pub(crate) fn guard<T>(fallback: T, body: impl FnOnce() -> T) -> T {
    // `AssertUnwindSafe` is honest here rather than a shrug: a caught panic
    // leaves the session it touched possibly inconsistent, and the contract
    // this crate offers is that the *process* survives so the host can
    // report the bug — not that the handle is still good afterwards. The
    // header says so.
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(value) => value,
        Err(payload) => {
            let what = payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "panic with a non-string payload".to_string());
            set_last_error(format!("panic caught at the FFI boundary: {what}"));
            log::error!("panic caught at the FFI boundary: {what}");
            fallback
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panic_becomes_a_code_and_a_message() {
        let rc = guard(cb_status::CB_ERR_PANIC, || -> cb_status {
            panic!("deliberate, from a test");
        });
        assert_eq!(rc, cb_status::CB_ERR_PANIC);
        assert!(
            last_error_message().contains("deliberate, from a test"),
            "the payload is recovered into the last-error string: {}",
            last_error_message()
        );
    }

    #[test]
    fn a_panic_with_no_string_payload_still_reports() {
        let rc = guard(cb_status::CB_ERR_PANIC, || -> cb_status {
            std::panic::panic_any(42u32);
        });
        assert_eq!(rc, cb_status::CB_ERR_PANIC);
        assert!(last_error_message().contains("non-string payload"));
    }

    #[test]
    fn the_last_error_is_per_thread() {
        // Two sessions on two threads must not overwrite each other's
        // diagnosis, which is the reason this is thread-local rather than
        // global.
        set_last_error("main thread");
        std::thread::spawn(|| {
            assert!(
                last_error_message().is_empty(),
                "a fresh thread starts clean"
            );
            set_last_error("worker thread");
        })
        .join()
        .expect("worker did not panic");
        assert_eq!(last_error_message(), "main thread");
    }

    #[test]
    fn every_engine_error_has_its_own_code() {
        // A code that collides with another silently merges two conditions
        // a host may need to tell apart. Cheap to assert, and the kind of
        // thing a careless addition breaks.
        use chapbook_reader::chapbook_core::ChapbookError;
        let samples = [
            ChapbookError::BookMalformed(String::new()),
            ChapbookError::ResourceNotFound(String::new()),
            ChapbookError::FixedLayoutUnsupported,
            ChapbookError::FormatNotBuilt("x"),
            ChapbookError::SpineOutOfRange(0),
            ChapbookError::Parse(String::new()),
            ChapbookError::Style(String::new()),
            ChapbookError::Layout(String::new()),
            ChapbookError::Font(String::new()),
            ChapbookError::Cfi(String::new()),
            ChapbookError::Network(String::new()),
            ChapbookError::Opds(String::new()),
            ChapbookError::Library(String::new()),
            ChapbookError::Credential(String::new()),
            ChapbookError::Panel(String::new()),
        ];
        let mut seen = std::collections::HashSet::new();
        for error in &samples {
            let code = cb_status::of(error) as i32;
            assert!(code < 0, "every failure is negative: {error}");
            assert!(seen.insert(code), "code {code} is used twice");
        }
    }
}
