//! The sync driver: a worker thread that authorizes per book.
//!
//! `chapbook_sync::SyncWorker` exists and is the simpler tool, but it
//! carries one credential for the life of its engine — right for a shell
//! with one account, wrong for a shelf holding books from two catalogs.
//! The CLI's `lib sync` walks books one at a time and authorizes each from
//! the credential store for exactly that reason; this driver is the same
//! loop moved onto a thread, so an app's sync button behaves like the
//! terminal's sync command.
//!
//! Like the CLI it never *clears* an authorization: a book whose origin
//! the store cannot answer runs with whatever was set before it. The
//! engine's own docs offer the honest way out for a shell that needs
//! isolation — "a shell whose services need different credentials wants
//! two engines, which is cheap."

use std::path::Path;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;

use chapbook_core::{
    ChapbookError, CredentialKey, CredentialLookup, CredentialStore, Freshness, Result,
};
use chapbook_library::BookId;
use chapbook_sync::{AnnotationReport, BookReport, PositionReport, SyncEngine};

/// What a shell can ask the driver for.
#[derive(Debug, Clone, Copy)]
pub enum SyncRequest {
    /// Every book with a service to talk to.
    All,
    /// One book.
    Book(BookId),
}

/// What the driver reports back, one drain at a time.
#[derive(Debug)]
pub enum SyncStatus {
    /// A book reconciled; the report says what moved. Failures inside one
    /// book — an unreachable service, a refused write — live *in* the
    /// report, because one dead host must not read as a dead batch.
    Book(Box<BookReport>),
    /// A book did not reconcile at all. The rest of the batch still ran.
    Failed { book: BookId, reason: String },
    /// The batch itself could not start — the library would not answer.
    Broken(String),
    /// A batch finished, so a shell can stop showing a spinner.
    Finished { books: usize },
}

/// A sync engine on its own thread, credentialed per book.
///
/// Dropping it closes the command channel and joins the worker — the same
/// join-on-drop the loader and `SyncWorker` promise, so a dropped driver
/// means nothing is still writing to the library behind the app.
pub struct SyncDriver {
    /// `None` only while `drop` is closing the channel.
    tx: Option<mpsc::Sender<SyncRequest>>,
    rx: mpsc::Receiver<SyncStatus>,
    worker: Option<thread::JoinHandle<()>>,
}

impl SyncDriver {
    /// Spawn the worker around an engine and a credential store. `waker`
    /// is called from the worker thread after each status is queued; it
    /// should do nothing but nudge the UI thread to drain.
    pub fn spawn(
        mut engine: SyncEngine,
        credentials: Arc<dyn CredentialStore>,
        waker: Arc<dyn Fn() + Send + Sync>,
    ) -> SyncDriver {
        let (tx, requests) = mpsc::channel::<SyncRequest>();
        let (events, rx) = mpsc::channel::<SyncStatus>();
        let worker = thread::Builder::new()
            .name("chapbook-app-sync".into())
            .spawn(move || {
                while let Ok(request) = requests.recv() {
                    let books = match request {
                        SyncRequest::Book(id) => vec![id],
                        SyncRequest::All => match engine.library().books_with_sync_targets() {
                            Ok(books) => books,
                            Err(e) => {
                                if events.send(SyncStatus::Broken(e.to_string())).is_err() {
                                    return; // the receiver went away
                                }
                                waker();
                                continue;
                            }
                        },
                    };
                    let mut done = 0usize;
                    for book in books {
                        authorize(&mut engine, credentials.as_ref(), book);
                        let status = match engine.sync_book(book) {
                            Ok(report) => SyncStatus::Book(Box::new(report)),
                            Err(e) => SyncStatus::Failed {
                                book,
                                reason: e.to_string(),
                            },
                        };
                        done += 1;
                        if events.send(status).is_err() {
                            return;
                        }
                        waker();
                    }
                    if events.send(SyncStatus::Finished { books: done }).is_err() {
                        return;
                    }
                    waker();
                }
            })
            .expect("spawn sync driver");
        SyncDriver {
            tx: Some(tx),
            rx,
            worker: Some(worker),
        }
    }

    /// Ask for some work. Returns `false` once the worker has gone.
    pub fn request(&self, request: SyncRequest) -> bool {
        self.tx.as_ref().is_some_and(|tx| tx.send(request).is_ok())
    }

    /// Take everything that has happened since the last drain.
    pub fn drain(&self) -> Vec<SyncStatus> {
        self.rx.try_iter().collect()
    }

    /// Block until the next status — for a caller with nothing else to do,
    /// which is a test. `None` once the worker has finished.
    pub fn next_status(&self) -> Option<SyncStatus> {
        self.rx.recv().ok()
    }
}

impl Drop for SyncDriver {
    fn drop(&mut self) {
        // Close the channel first: the worker is parked in `recv` and that
        // is what ends it. Then join, so a dropped driver means no thread
        // is still touching the database.
        self.tx = None;
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Set the engine's `Authorization` for this book's origin, when the store
/// has one. Keyed by origin, never by the service URL itself — a catalog
/// URL's path may be a secret.
fn authorize(engine: &mut SyncEngine, store: &dyn CredentialStore, book: BookId) {
    let Ok(targets) = engine.library().sync_targets(book) else {
        return;
    };
    let Some(url) = targets
        .progression_url
        .as_deref()
        .or(targets.annotation_container.as_deref())
    else {
        return;
    };
    if let Some(key) = CredentialKey::http_origin(url) {
        if let CredentialLookup::Found(credential) = store.get(&key, Freshness::Cached) {
            engine.set_authorization(credential.authorization);
        }
    }
}

/// The device identity sync speaks as, minted once and kept beside the
/// library. `chapbook-sync` deliberately neither generates nor persists
/// one, so the application does — the same file the CLI writes, because a
/// terminal and a window on the same machine are the same device. The
/// name is the platform's: what a progression service shows beside this
/// device's position.
pub(crate) fn device_identity(
    dir: &Path,
    name: &str,
) -> Result<chapbook_opds::progression::Device> {
    let path = dir.join("device");
    let id = match std::fs::read_to_string(&path) {
        Ok(existing) if !existing.trim().is_empty() => existing.trim().to_string(),
        _ => {
            let id = mint_device_id();
            std::fs::write(&path, &id).map_err(|e| {
                ChapbookError::Library(format!(
                    "cannot write the device id to {}: {e}",
                    path.display()
                ))
            })?;
            id
        }
    };
    Ok(chapbook_opds::progression::Device {
        id,
        name: name.to_string(),
    })
}

fn mint_device_id() -> String {
    let mut bytes = [0u8; 16];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut bytes))
        .is_err()
    {
        // No `/dev` to read: hash what varies instead. Thinner entropy
        // than a UUID deserves, but it is minted once and written down,
        // and refusing to sync over it would serve nobody.
        let seed = format!("{:?}-{}", std::time::SystemTime::now(), std::process::id());
        let hex = chapbook_library::Library::fingerprint_of_bytes(seed.as_bytes());
        for (slot, pair) in bytes.iter_mut().zip(hex.as_bytes().chunks(2)) {
            *slot = std::str::from_utf8(pair)
                .ok()
                .and_then(|s| u8::from_str_radix(s, 16).ok())
                .unwrap_or(0);
        }
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A position report in the words the CLI uses.
pub(crate) fn describe_position(report: &PositionReport) -> String {
    match report {
        PositionReport::Idle => "idle".to_string(),
        PositionReport::Pushed => "pushed".to_string(),
        PositionReport::Pulled => "pulled".to_string(),
        // Not a failure: the service holds something newer, and saying so
        // is the difference between "try again" and "you lost a page".
        PositionReport::Refused(why) => format!("refused, the service is ahead ({why})"),
        PositionReport::Conflict => "conflict — both moved, nothing overwritten".to_string(),
        PositionReport::Failed(why) => format!("failed: {why}"),
    }
}

/// An annotation report as one clause, nonzero counts only.
pub(crate) fn describe_marks(report: &AnnotationReport) -> String {
    let mut parts = Vec::new();
    for (count, what) in [
        (report.created, "created"),
        (report.updated, "updated"),
        (report.deleted, "deleted"),
        (report.adopted, "adopted"),
        (report.refreshed, "refreshed"),
        (report.withdrawn, "withdrawn"),
        (report.merged, "merged"),
        (report.conflicts, "in conflict"),
    ] {
        if count > 0 {
            parts.push(format!("{count} {what}"));
        }
    }
    let mut summary = if parts.is_empty() {
        "idle".to_string()
    } else {
        parts.join(", ")
    };
    // Said out loud, because it changes what the rest of the line means:
    // no deletion was inferred, so a mark another device removed may
    // still be sitting here.
    if report.truncated {
        summary.push_str(" — container longer than one sync reads");
    }
    match &report.failed {
        Some(why) => format!("{summary} (container unreachable: {why})"),
        None => summary,
    }
}
