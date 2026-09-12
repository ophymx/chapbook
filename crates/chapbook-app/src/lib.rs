//! The application model behind a full chapbook app.
//!
//! Everything a shelf-and-reader application decides that is not a widget
//! lives here: which books a shelf shows and in what order, how a reading
//! session is configured, how sync is driven and credentialed, and the
//! words an app uses to say what happened. A front end — `chapbook-app-gtk`
//! today — owns widgets, gestures and the main loop, and asks this crate
//! everything else, so a second toolkit is a second front end rather than
//! a second application.
//!
//! The split follows the reference shells' discipline one level up:
//! `chapbook-reader` already keeps everything book-shaped out of a shell,
//! and this crate keeps everything *app*-shaped out of one — the CLI's
//! `lib` subcommands are the same decisions written for a terminal, and
//! where a choice here has a precedent there (default sort, credential
//! lookup, the wording of a sync report), this crate matches it so the two
//! doors agree.
//!
//! This crate is part of the application, not the platform: downstreams
//! build on `chapbook-reader`, and `docs/STABILITY.md` places this pair
//! accordingly.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chapbook_core::{ChapbookError, CredentialStore, FontSource, Result};
use chapbook_library::{BookId, BookQuery, BookRecord, Library, ReadingState, Sort};
use chapbook_reader::{open_publication, Session, SessionConfig};
use chapbook_sync::SyncEngine;

pub use chapbook_library;
pub use chapbook_opds;
pub use chapbook_reader;
pub use chapbook_sync;

mod sync;

pub use sync::{SyncDriver, SyncRequest, SyncStatus};

/// What the shelf shows. Every field narrows independently; `Default` is
/// the whole shelf in reading order.
#[derive(Debug, Clone)]
pub struct ShelfFilter {
    /// Substring match over title, authors and series; whitespace-only
    /// means "everything". Diacritic folding is the library's.
    pub search: String,
    pub sort: Sort,
    pub state: Option<ReadingState>,
    pub series: Option<String>,
}

impl Default for ShelfFilter {
    fn default() -> ShelfFilter {
        ShelfFilter {
            search: String::new(),
            // Reading order, not insertion order: someone opening the app
            // is looking for what they were reading, the same default the
            // CLI's `lib ls` settled on.
            sort: Sort::Read,
            state: None,
            series: None,
        }
    }
}

/// One running application: a library, its directory, and the sync driver
/// once something has asked for one.
///
/// Lives on the UI thread and is not `Sync`; the threads an app needs —
/// the session's loader, the sync driver — are owned by the engine crates
/// and reach back only through wakers.
pub struct App {
    dir: PathBuf,
    library: Library,
    credentials: Arc<dyn CredentialStore>,
    sync: Option<SyncDriver>,
}

impl App {
    /// Open the application over a library directory, `None` for the
    /// platform default.
    pub fn open(dir: Option<&Path>) -> Result<App> {
        let dir = match dir {
            Some(dir) => dir.to_path_buf(),
            None => Library::default_dir()?,
        };
        let library = Library::open(&dir)?;
        Ok(App {
            dir,
            library,
            credentials: Arc::new(chapbook_core::EnvCredentials),
            sync: None,
        })
    }

    /// The library directory everything persists under.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The shelf, narrowed however the filter asks.
    pub fn shelf(&self, filter: &ShelfFilter) -> Result<Vec<BookRecord>> {
        let search = filter.search.trim();
        self.library.query(&BookQuery {
            search: (!search.is_empty()).then_some(search),
            series: filter.series.as_deref(),
            state: filter.state,
            sort: filter.sort,
            ..BookQuery::default()
        })
    }

    /// The series on the shelf, with how many books each holds.
    pub fn series(&self) -> Result<Vec<(String, usize)>> {
        self.library.series()
    }

    /// One book's record, `None` if it left the shelf.
    pub fn book(&self, id: BookId) -> Result<Option<BookRecord>> {
        self.library.book(id)
    }

    /// Import a local book into the library and return its shelf record.
    pub fn import(&mut self, path: &Path) -> Result<BookRecord> {
        let publication = open_publication(path)?;
        let id = self.library.import(path, publication.as_ref())?;
        Ok(self.library.book(id)?.expect("just imported"))
    }

    /// Remove a book from the shelf.
    pub fn remove(&mut self, id: BookId) -> Result<()> {
        self.library.delete_book(id)
    }

    /// Mark a book finished, or take it back.
    pub fn set_finished(&mut self, id: BookId, finished: bool) -> Result<()> {
        self.library.set_finished(id, finished)
    }

    /// Open a reading session on a shelf book, configured the one way this
    /// application configures sessions: host fonts, the app's library
    /// directory (which is what restores the reading position), and the
    /// app's credential store. The shell owns the session it gets back —
    /// its waker, its metrics, and calling `save_position` before letting
    /// go of it.
    pub fn open_book(&self, id: BookId) -> Result<Session> {
        let record = self
            .library
            .book(id)?
            .ok_or_else(|| ChapbookError::Library(format!("book #{} is not on the shelf", id.0)))?;
        if record.file_path.as_os_str().is_empty() {
            // An adopted book: the platform holds the file and the shell
            // holds the handle. This desktop model only imports, so a
            // record like this arrived some other way.
            return Err(ChapbookError::Library(format!(
                "book #{} is adopted — no path to open it by",
                id.0
            )));
        }
        let config = SessionConfig::new(FontSource::host())
            .with_library_dir(&self.dir)
            .with_credentials(self.credentials.clone());
        Session::open_with(record.file_path.as_path(), config)
    }

    /// Ask for every syncable book to reconcile, spawning the driver on
    /// first use. Returns `Ok(false)` — and asks for nothing — when no
    /// book has a service to sync with, which is a fact to tell the reader
    /// rather than a spinner to show them.
    ///
    /// `waker` is called from the driver's thread after each report lands;
    /// it should only nudge the UI thread to come drain
    /// [`App::sync_events`].
    pub fn sync_all(&mut self, waker: Arc<dyn Fn() + Send + Sync>) -> Result<bool> {
        if self.library.books_with_sync_targets()?.is_empty() {
            return Ok(false);
        }
        self.sync_driver(waker)?.request(SyncRequest::All);
        Ok(true)
    }

    /// Everything sync has reported since the last drain.
    pub fn sync_events(&self) -> Vec<SyncStatus> {
        self.sync
            .as_ref()
            .map(|driver| driver.drain())
            .unwrap_or_default()
    }

    /// One sync report as a line a shell can show. The words live here so
    /// every front end says the same thing.
    pub fn describe_sync(&self, status: &SyncStatus) -> String {
        match status {
            SyncStatus::Book(report) => format!(
                "{}: position {}; marks {}",
                self.title_of(report.book),
                sync::describe_position(&report.position),
                sync::describe_marks(&report.annotations)
            ),
            SyncStatus::Failed { book, reason } => {
                format!("{}: {reason}", self.title_of(*book))
            }
            SyncStatus::Broken(reason) => format!("sync could not run: {reason}"),
            SyncStatus::Finished { books } => match books {
                1 => "sync finished: 1 book".to_string(),
                n => format!("sync finished: {n} books"),
            },
        }
    }

    fn title_of(&self, id: BookId) -> String {
        self.library
            .book(id)
            .ok()
            .flatten()
            .map(|record| record.title)
            .unwrap_or_else(|| format!("book #{}", id.0))
    }

    fn sync_driver(&mut self, waker: Arc<dyn Fn() + Send + Sync>) -> Result<&SyncDriver> {
        if self.sync.is_none() {
            // The engine owns its own connection rather than sharing this
            // model's: it lives on the driver's thread.
            let library = Library::open(&self.dir)?;
            let device = sync::device_identity(&self.dir)?;
            let engine = SyncEngine::new(library, Arc::new(chapbook_opds::UreqHttp::new()), device);
            self.sync = Some(SyncDriver::spawn(engine, self.credentials.clone(), waker));
        }
        Ok(self.sync.as_ref().expect("just spawned"))
    }
}

/// A shelf row's state, in the words every front end shows: "unread",
/// "reading 42%", "finished". Matches the CLI's `describe_state`, which is
/// the point — the two doors agree.
/// A mark as a marks list shows it: where it sits, then what it says.
/// The quote is elided rather than wrapped — a list row is a reminder,
/// and the jump is how you read the rest.
pub fn describe_annotation(mark: &chapbook_reader::AnnotationSummary) -> String {
    use chapbook_library::AnnotationKind;
    let kind = match mark.kind {
        AnnotationKind::Bookmark => "bookmark",
        AnnotationKind::Highlight => "highlight",
        AnnotationKind::Note => "note",
    };
    let at = format!("{kind} \u{b7} {:.0}%", mark.progression * 100.0);
    match mark
        .text
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        Some(text) => {
            let mut quote: String = text.chars().take(60).collect();
            if text.chars().count() > 60 {
                quote.push('\u{2026}');
            }
            format!("{at} \u{b7} \u{201c}{quote}\u{201d}")
        }
        None => at,
    }
}

/// The engine's themes in cycle order, with the names a menu shows.
pub fn theme_names() -> [(&'static str, chapbook_core::Theme); 3] {
    use chapbook_core::Theme;
    [
        ("Light", Theme::Light),
        ("Sepia", Theme::Sepia),
        ("Dark", Theme::Dark),
    ]
}

/// The table of contents flattened for a menu: each entry with its
/// nesting depth, in reading order. Entries that link nowhere still
/// appear — they are section headings, and hiding them would orphan
/// their children's indentation.
pub fn flatten_toc(entries: &[chapbook_core::TocEntry]) -> Vec<(usize, chapbook_core::TocEntry)> {
    fn walk(
        entries: &[chapbook_core::TocEntry],
        depth: usize,
        out: &mut Vec<(usize, chapbook_core::TocEntry)>,
    ) {
        for entry in entries {
            out.push((depth, entry.clone()));
            walk(&entry.children, depth + 1, out);
        }
    }
    let mut out = Vec::new();
    walk(entries, 0, &mut out);
    out
}

pub fn describe_state(book: &BookRecord) -> String {
    let percent = book
        .progress
        .map(|p| format!(" {:.0}%", p * 100.0))
        .unwrap_or_default();
    match book.state() {
        ReadingState::Unread => "unread".to_string(),
        ReadingState::Reading => format!("reading{percent}"),
        // A finished book's progress is wherever the reader is now, which
        // may be the beginning again — not shown beside a word it would
        // contradict.
        ReadingState::Finished => "finished".to_string(),
    }
}
