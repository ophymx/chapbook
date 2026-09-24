//! Sync, driven from C.
//!
//! The engine side existed and was reachable by nobody outside Rust:
//! `chapbook-sync` reconciles positions and marks, `SyncWorker` puts it
//! on a thread, and neither crossed this boundary — so every shell that
//! is not itself Rust held a library it had no way to reconcile. This
//! module is that reach: open a worker over the library, ask it for a
//! book or the shelf, drain what happened.
//!
//! Two decisions shape the surface, both learned from building the
//! desktop application:
//!
//! **The transport does its own authorization.** The Rust desktop app
//! needs per-book credential choreography because its bundled transport
//! is credential-less; a host transport is the host's networking, and
//! the host attaches `Authorization` per request by whatever store it
//! keeps. So this ABI carries no credential surface at all — smaller
//! than the Rust one, on purpose. The corollary is that a sync transport
//! must *write*: reconciling marks means POST, PUT and DELETE, so
//! [`cb_sync_open`] requires the send callback whenever a get callback
//! is given, rather than letting a read-only transport turn "synced"
//! into a per-book failure a host has to explain.
//!
//! **The device identity is the host's.** The engine deliberately
//! neither mints nor persists one; a host has a better device name than
//! this library could invent and a place to keep the id. Mint an id
//! once, store it, and pass the same one forever — it is how a
//! progression service tells this device's positions from another's.
//!
//! The waker follows [`cb_session_set_waker`]'s contract exactly: it
//! fires on the worker thread, once per report, and must only nudge the
//! host's main loop to come call [`cb_sync_next`].
//!
//! [`cb_session_set_waker`]: crate::cb_session_set_waker

use std::ffi::{c_char, c_void};

use crate::error::{cb_status, fail, guard};
use crate::session::cb_wake_fn;

#[cfg(feature = "sync")]
use crate::error::clear_last_error;
#[cfg(feature = "sync")]
use std::collections::VecDeque;
#[cfg(any(feature = "sync", feature = "opds"))]
use std::ffi::CString;

// The report marshalling serves two drivers: the worker this module
// opens, and the application layer's. Either build has the sync crate
// in reach — directly, or through the app crate — and the types are one
// crate's either way.
#[cfg(all(not(feature = "sync"), feature = "opds"))]
use chapbook_app::chapbook_sync as sync_types;
#[cfg(feature = "sync")]
use chapbook_sync as sync_types;

/// A sync worker over one library. Opaque.
///
/// Owns a thread; [`cb_sync_close`] joins it. Not thread-safe: like a
/// session, a handle belongs to one thread at a time, and the thread the
/// worker owns reaches back only through the waker.
pub struct cb_sync {
    #[cfg(feature = "sync")]
    worker: chapbook_sync::SyncWorker,
    /// Reports drained from the worker and not yet handed out.
    #[cfg(feature = "sync")]
    pending: VecDeque<chapbook_sync::SyncEvent>,
    /// The strings the last-returned report borrows; replaced on the
    /// next call, which is what bounds their lifetime.
    #[cfg(feature = "sync")]
    strings: Vec<CString>,
}

/// What kind of report [`cb_sync_next`] filled in.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum cb_sync_kind {
    /// A book reconciled. Failures *inside* the book — an unreachable
    /// service, a refused write — live in the report's position and mark
    /// fields, because one dead host must not read as a dead batch.
    CB_SYNC_BOOK = 0,
    /// A book did not reconcile at all — removed from the shelf, or no
    /// service to talk to. The rest of the batch still ran.
    CB_SYNC_BOOK_FAILED = 1,
    /// A batch finished; `books` says how many reports preceded this.
    /// The signal to stop showing a spinner.
    CB_SYNC_FINISHED = 2,
    /// The batch itself could not start — the library would not answer.
    /// `detail` says why. Only [`cb_app_sync_next`](crate::cb_app_sync_next)
    /// reports it; a worker opened with [`cb_sync_open`] has its library
    /// by then.
    CB_SYNC_BROKEN = 3,
}

/// What happened to a book's reading position.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum cb_sync_position {
    /// Nothing to do: no service, or nothing had changed on either side.
    CB_SYNC_POSITION_IDLE = 0,
    /// This device's position reached the service.
    CB_SYNC_POSITION_PUSHED = 1,
    /// The service's position was adopted locally.
    CB_SYNC_POSITION_PULLED = 2,
    /// The service declined; what it holds is newer. Not a failure — the
    /// next pull brings it down if the local copy is clean by then.
    CB_SYNC_POSITION_REFUSED = 3,
    /// Both sides moved since they last agreed. Nothing was overwritten.
    CB_SYNC_POSITION_CONFLICT = 4,
    /// The service could not be reached, or answered something unusable.
    /// Nothing local changed.
    CB_SYNC_POSITION_FAILED = 5,
}

/// One report from the worker. Plain data; the strings are borrowed from
/// the handle and stay valid until the next [`cb_sync_next`] or
/// [`cb_sync_close`] — copy them before either.
///
/// Which fields mean anything depends on `kind`: a `CB_SYNC_BOOK` fills
/// `book`, `position` and the mark counts; `CB_SYNC_BOOK_FAILED` fills
/// `book` and `detail`; `CB_SYNC_FINISHED` fills `books`. Everything
/// else is zeroed, so a host may also just read what it prints.
#[repr(C)]
pub struct cb_sync_report {
    pub kind: cb_sync_kind,
    /// The library row, [`cb_library_query`](crate::cb_library_query)'s
    /// id space.
    pub book: i64,
    pub position: cb_sync_position,
    /// The refusal or failure being reported — the position's for
    /// `CB_SYNC_BOOK` with a refused or failed position, the book's for
    /// `CB_SYNC_BOOK_FAILED`. Null when there is nothing to explain.
    pub detail: *const c_char,
    /// Marks: written to the container as new.
    pub marks_created: usize,
    /// Marks: this device's edits written over the container's copy.
    pub marks_updated: usize,
    /// Marks: deletes carried out on the container.
    pub marks_deleted: usize,
    /// Marks: pulled from the container as marks this device had not
    /// seen.
    pub marks_adopted: usize,
    /// Marks: already known here, brought up to date with what another
    /// device wrote.
    pub marks_refreshed: usize,
    /// Marks: another device's deletions arriving — taken off this shelf
    /// because a complete container listing no longer holds them.
    /// Distinct from `marks_deleted`, which is this device's own
    /// deletions reaching the container.
    pub marks_withdrawn: usize,
    /// Marks: conflicts settled by re-reading the container — both edits
    /// survive, nothing overwritten.
    pub marks_merged: usize,
    /// Marks: still owing a write after a merge was attempted. The next
    /// pass tries again.
    pub marks_conflicts: usize,
    /// The container had more pages than one pass reads, so the pull saw
    /// a prefix and no deletion was inferred — a mark another device
    /// removed may still be sitting here. Worth saying to the reader,
    /// because it changes what the counts above mean.
    pub listing_truncated: bool,
    /// The container could not be reached; whatever was pushed before it
    /// failed stands. Null when the mark half ran to the end.
    pub marks_error: *const c_char,
    /// `CB_SYNC_FINISHED` only: how many books the batch reported.
    pub books: usize,
}

/// Start a sync worker over the library at `library_dir`.
///
/// `device_id` and `device_name` identify this device to a progression
/// service: the id is stable and host-minted (mint once, store, reuse
/// forever — it is how the service tells this device's positions from
/// another's), the name is for people.
///
/// The transport: pass `get` and `send` callbacks —
/// [`cb_http_get_fn`](crate::cb_http_get_fn) and
/// [`cb_http_send_fn`](crate::cb_http_send_fn), same contracts as the
/// session's transport plus the write half — or pass both null to use
/// the bundled transport where this build carries one
/// (`CB_CAP_BUNDLED_HTTP`). A get without a send is refused: sync
/// writes, and a transport that cannot is not a sync transport.
/// `finalize` releases `transport_user` exactly once, on the same
/// ownership rule as
/// [`cb_config_set_http_transport`](crate::cb_config_set_http_transport)
/// — including on every failure path of this call.
///
/// `wake` may be null; the host then polls [`cb_sync_next`] on its own
/// clock. Given, it fires on the worker thread once per queued report
/// and must only nudge the host's main loop.
///
/// The worker holds its own connection to the library, so the handle
/// coexists with open sessions and a `cb_library` on the same directory.
///
/// Without sync in this build (`cb_capabilities()` lacks `CB_CAP_SYNC`)
/// this reports `CB_ERR_FORMAT_NOT_BUILT` — after running `finalize`,
/// keeping the ownership rule true.
#[no_mangle]
pub unsafe extern "C" fn cb_sync_open(
    library_dir: *const c_char,
    device_id: *const c_char,
    device_name: *const c_char,
    get: crate::http::cb_http_get_fn,
    send: crate::http::cb_http_send_fn,
    finalize: crate::http::cb_http_finalize_fn,
    transport_user: *mut c_void,
    wake: cb_wake_fn,
    wake_user: *mut c_void,
    out: *mut *mut cb_sync,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        // Failing before taking ownership would split the finalize
        // contract in two; run it on every path that does not store it.
        let decline = |code, message: &str| {
            if let Some(finalize) = finalize {
                // SAFETY: the host's own finalizer with the host's own
                // pointer, called exactly once.
                unsafe { finalize(transport_user) };
            }
            fail(code, message)
        };
        #[cfg(feature = "sync")]
        {
            use chapbook_reader::chapbook_library::Library;
            use chapbook_reader::HttpClient;

            clear_last_error();
            if out.is_null() {
                return decline(cb_status::CB_ERR_NULL_ARGUMENT, "out is null");
            }
            // SAFETY: the header's contract for all three strings.
            let (Some(dir), Some(id), Some(name)) = (
                unsafe { crate::abi::str_in(library_dir, "library_dir") },
                unsafe { crate::abi::str_in(device_id, "device_id") },
                unsafe { crate::abi::str_in(device_name, "device_name") },
            ) else {
                return decline(cb_status::CB_ERR_NULL_ARGUMENT, "a required string is null");
            };

            let transport: std::sync::Arc<dyn HttpClient> = match (get, send) {
                (Some(get), Some(send)) => std::sync::Arc::new(crate::http::host::HostTransport {
                    get,
                    send: Some(send),
                    finalize,
                    user: transport_user as usize,
                }),
                (Some(_), None) => {
                    return decline(
                        cb_status::CB_ERR_NULL_ARGUMENT,
                        "send callback is null: sync writes, and a transport \
                         that cannot is not a sync transport",
                    );
                }
                (None, Some(_)) => {
                    return decline(
                        cb_status::CB_ERR_NULL_ARGUMENT,
                        "get callback is null: the engine reads a service \
                         before it writes to one",
                    );
                }
                (None, None) => {
                    #[cfg(feature = "ureq")]
                    {
                        // Nothing to finalize was handed over, but the rule
                        // is unconditional and running it costs nothing.
                        if let Some(finalize) = finalize {
                            // SAFETY: as above, exactly once.
                            unsafe { finalize(transport_user) };
                        }
                        std::sync::Arc::new(chapbook_sync::UreqHttp::new())
                    }
                    #[cfg(not(feature = "ureq"))]
                    {
                        return decline(
                            cb_status::CB_ERR_FORMAT_NOT_BUILT,
                            "this build bundles no transport; pass get and \
                             send callbacks",
                        );
                    }
                }
            };

            let library = match Library::open(std::path::Path::new(dir)) {
                Ok(library) => library,
                // The transport owns itself now; dropping it runs finalize.
                Err(e) => return crate::error::from_error(&e),
            };
            let engine = chapbook_sync::SyncEngine::new(
                library,
                transport,
                chapbook_sync::Device {
                    id: id.to_string(),
                    name: name.to_string(),
                },
            );
            let wake_user = wake_user as usize;
            let waker: std::sync::Arc<dyn Fn() + Send + Sync> = match wake {
                Some(wake) => std::sync::Arc::new(move || wake(wake_user as *mut c_void)),
                None => std::sync::Arc::new(|| {}),
            };
            let handle = Box::new(cb_sync {
                worker: chapbook_sync::SyncWorker::spawn(engine, waker),
                pending: VecDeque::new(),
                strings: Vec::new(),
            });
            // SAFETY: checked non-null above.
            unsafe { *out = Box::into_raw(handle) };
            cb_status::CB_OK
        }
        #[cfg(not(feature = "sync"))]
        {
            let _ = (
                library_dir,
                device_id,
                device_name,
                get,
                send,
                wake,
                wake_user,
                out,
            );
            decline(
                cb_status::CB_ERR_FORMAT_NOT_BUILT,
                "this build has no sync support",
            )
        }
    })
}

/// Ask for every book with a service to reconcile. Reports arrive per
/// book through [`cb_sync_next`], then one `CB_SYNC_FINISHED`; a shelf
/// where nothing syncs finishes immediately with zero books, which is a
/// fact to tell the reader rather than a spinner to show them.
#[no_mangle]
pub unsafe extern "C" fn cb_sync_request_all(sync: *mut cb_sync) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        #[cfg(feature = "sync")]
        {
            clear_last_error();
            // SAFETY: a handle from `cb_sync_open`, not yet closed.
            let Some(sync) = (unsafe { sync.as_mut() }) else {
                return fail(cb_status::CB_ERR_NULL_ARGUMENT, "sync is null");
            };
            if sync.worker.request(chapbook_sync::SyncCommand::All) {
                cb_status::CB_OK
            } else {
                fail(cb_status::CB_ERR_UNAVAILABLE, "the sync worker has gone")
            }
        }
        #[cfg(not(feature = "sync"))]
        {
            let _ = sync;
            fail(
                cb_status::CB_ERR_FORMAT_NOT_BUILT,
                "this build has no sync support",
            )
        }
    })
}

/// Ask for one book to reconcile. A book with no service, or one that
/// has left the shelf, reports `CB_SYNC_BOOK_FAILED` rather than being
/// silently skipped — the caller named it, so the answer names it back.
#[no_mangle]
pub unsafe extern "C" fn cb_sync_request_book(sync: *mut cb_sync, book: i64) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        #[cfg(feature = "sync")]
        {
            use chapbook_reader::chapbook_library::BookId;
            clear_last_error();
            // SAFETY: a handle from `cb_sync_open`, not yet closed.
            let Some(sync) = (unsafe { sync.as_mut() }) else {
                return fail(cb_status::CB_ERR_NULL_ARGUMENT, "sync is null");
            };
            if sync
                .worker
                .request(chapbook_sync::SyncCommand::Book(BookId(book)))
            {
                cb_status::CB_OK
            } else {
                fail(cb_status::CB_ERR_UNAVAILABLE, "the sync worker has gone")
            }
        }
        #[cfg(not(feature = "sync"))]
        {
            let _ = (sync, book);
            fail(
                cb_status::CB_ERR_FORMAT_NOT_BUILT,
                "this build has no sync support",
            )
        }
    })
}

/// Take the next report, oldest first. `CB_ERR_UNAVAILABLE` when there
/// is none, which is the ordinary answer between wakes, not an error
/// worth surfacing.
///
/// The strings the filled report borrows live in the handle and are
/// replaced by the next call — copy anything worth keeping before
/// calling again.
#[no_mangle]
pub unsafe extern "C" fn cb_sync_next(sync: *mut cb_sync, out: *mut cb_sync_report) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        #[cfg(feature = "sync")]
        {
            clear_last_error();
            // SAFETY: a handle from `cb_sync_open`, not yet closed.
            let Some(sync) = (unsafe { sync.as_mut() }) else {
                return fail(cb_status::CB_ERR_NULL_ARGUMENT, "sync is null");
            };
            if out.is_null() {
                return fail(cb_status::CB_ERR_NULL_ARGUMENT, "out is null");
            }
            if sync.pending.is_empty() {
                sync.pending.extend(sync.worker.drain());
            }
            let Some(event) = sync.pending.pop_front() else {
                return fail(cb_status::CB_ERR_UNAVAILABLE, "no report waiting");
            };

            // The strings the previous report borrowed die here, which is
            // the documented lifetime.
            let report = report_of(event, &mut sync.strings);
            // SAFETY: checked non-null above.
            unsafe { *out = report };
            cb_status::CB_OK
        }
        #[cfg(not(feature = "sync"))]
        {
            let _ = (sync, out);
            fail(
                cb_status::CB_ERR_FORMAT_NOT_BUILT,
                "this build has no sync support",
            )
        }
    })
}

/// A report with nothing in it, every field zeroed and every string
/// null, for the caller to fill what its kind means.
#[cfg(any(feature = "sync", feature = "opds"))]
pub(crate) fn blank_report() -> cb_sync_report {
    cb_sync_report {
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
    }
}

/// Keep a string for a report to borrow, NUL-free and NUL-terminated,
/// for as long as `strings` holds it.
#[cfg(any(feature = "sync", feature = "opds"))]
pub(crate) fn keep(strings: &mut Vec<CString>, text: &str) -> *const c_char {
    let owned = CString::new(text.replace('\0', " ")).expect("NULs were just replaced");
    let ptr = owned.as_ptr();
    strings.push(owned);
    ptr
}

/// One worker event as the report a host reads, its strings kept in
/// `strings` — which the caller clears first, since that is what bounds
/// the previous report's lifetime.
#[cfg(any(feature = "sync", feature = "opds"))]
pub(crate) fn report_of(
    event: sync_types::SyncEvent,
    strings: &mut Vec<CString>,
) -> cb_sync_report {
    use sync_types::{PositionReport, SyncEvent};

    strings.clear();
    let mut report = blank_report();
    match event {
        SyncEvent::Book(book) => {
            report.kind = cb_sync_kind::CB_SYNC_BOOK;
            report.book = book.book.0;
            report.position = match &book.position {
                PositionReport::Idle => cb_sync_position::CB_SYNC_POSITION_IDLE,
                PositionReport::Pushed => cb_sync_position::CB_SYNC_POSITION_PUSHED,
                PositionReport::Pulled => cb_sync_position::CB_SYNC_POSITION_PULLED,
                PositionReport::Refused(why) => {
                    report.detail = keep(strings, why);
                    cb_sync_position::CB_SYNC_POSITION_REFUSED
                }
                PositionReport::Conflict => cb_sync_position::CB_SYNC_POSITION_CONFLICT,
                PositionReport::Failed(why) => {
                    report.detail = keep(strings, why);
                    cb_sync_position::CB_SYNC_POSITION_FAILED
                }
            };
            let marks = &book.annotations;
            report.marks_created = marks.created;
            report.marks_updated = marks.updated;
            report.marks_deleted = marks.deleted;
            report.marks_adopted = marks.adopted;
            report.marks_refreshed = marks.refreshed;
            report.marks_withdrawn = marks.withdrawn;
            report.marks_merged = marks.merged;
            report.marks_conflicts = marks.conflicts;
            report.listing_truncated = marks.truncated;
            if let Some(why) = &marks.failed {
                report.marks_error = keep(strings, why);
            }
        }
        SyncEvent::Failed { book, reason } => {
            report.kind = cb_sync_kind::CB_SYNC_BOOK_FAILED;
            report.book = book.0;
            report.detail = keep(strings, &reason);
        }
        SyncEvent::Finished { books } => {
            report.kind = cb_sync_kind::CB_SYNC_FINISHED;
            report.books = books;
        }
    }
    report
}

/// Close the worker. Accepts null.
///
/// **Blocks for the book in flight**: the thread is joined, on the same
/// argument as closing a session joins its loader — a returned close
/// means nothing is still writing to the library or holding the
/// transport, so a host may free whatever its callbacks used as soon as
/// `finalize` has run.
#[no_mangle]
pub unsafe extern "C" fn cb_sync_close(sync: *mut cb_sync) {
    guard((), || {
        if !sync.is_null() {
            // SAFETY: a handle from `cb_sync_open`, freed once.
            drop(unsafe { Box::from_raw(sync) });
        }
    })
}
