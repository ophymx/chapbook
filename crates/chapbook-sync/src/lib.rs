//! Reconciling what a reader did on this device with what the services a
//! book came from already hold: its reading position over OPDS
//! Progression, its marks over a Web Annotation container.
//!
//! # What this crate is and is not
//!
//! It is the *driver*. The two mappings — `chapbook_opds::progression` and
//! `chapbook_annotations` — say what a position and a mark look like on
//! the wire, and `chapbook_library`'s v6 bookkeeping says what still owes
//! a service a write. This crate is the loop between them, plus the rules
//! about who wins.
//!
//! It holds **no schedule**. When to sync is the shell's decision: every
//! page turn is wrong on a metered radio, and on close is wrong for a
//! session that crashes. [`SyncWorker`] gives a shell a thread to ask on
//! and gets out of the way.
//!
//! # It owns its own library handle
//!
//! [`SyncEngine`] opens its own [`Library`], rather than sharing the
//! session's. A `Session` holds its `Library` by value on the thread that
//! owns the session, and the whole point here is not to be on that thread
//! — SQLite in WAL mode takes concurrent connections, which is the
//! ordinary way to have two. It also means sync is not scoped to an open
//! book: a shell can reconcile a shelf nobody is reading.
//!
//! # Never on the UI thread
//!
//! Every call here can block for as long as a network wants it to, and
//! `CredentialStore` lookups happen underneath. [`SyncWorker`] is the
//! supported way in; [`SyncEngine`] is exposed for callers that already
//! have a worker thread of their own, and for tests.

mod engine;
mod worker;

pub use engine::{AnnotationReport, BookReport, PositionReport, SyncEngine, SyncError, SyncResult};
pub use worker::{SyncCommand, SyncEvent, SyncWorker};

// What a caller constructing an engine has to name: `SyncEngine::new`
// takes a `Device` and an `HttpClient`. Re-exported so that caller depends
// on this crate alone rather than chasing the types through two more.
pub use chapbook_opds::progression::Device;
/// The bundled desktop transport, for callers that want the default.
#[cfg(feature = "ureq")]
pub use chapbook_opds::UreqHttp;
pub use chapbook_opds::{Body, HttpClient, HttpError, HttpRequest, HttpResponse};

use chapbook_annotations::REL_ANNOTATION_SERVICE;
use chapbook_opds::progression::REL_PROGRESSION;
use chapbook_opds::Entry;

/// The two service URLs a catalog entry advertises, ready for
/// [`chapbook_library::Library::set_sync_targets`].
///
/// This is where a book learns where it syncs to, and the only place it
/// can: in both protocols the URL *is* the publication's identity, so a
/// book that arrives as bare bytes — a sideloaded file, adopted content —
/// has no service until something maps it back to a catalog entry. A shell
/// that downloads from a catalog should call this with the entry it
/// downloaded and record the result against the imported book.
///
/// Both URLs are opaque and may embed a per-user key. Never log them.
pub fn targets_of(entry: &Entry) -> (Option<String>, Option<String>) {
    let href = |rel: &str| {
        entry
            .links
            .iter()
            .find(|link| link.has_rel(rel))
            .map(|link| link.href.clone())
    };
    (href(REL_PROGRESSION), href(REL_ANNOTATION_SERVICE))
}
