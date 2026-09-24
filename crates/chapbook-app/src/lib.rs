//! The application policy layer: what a chapbook app *decides*, written
//! once.
//!
//! `chapbook-reader` keeps everything book-shaped out of a shell. This
//! crate keeps everything *app*-shaped out of one: which books a shelf
//! shows and in what order, how a file becomes a book and how the book is
//! found again, how a session is configured, where the reader is and how
//! much of the book that is, how much memory a page cache may take, how a
//! search walks a book, how a catalog is browsed, refused and signed into,
//! what a landed download does, and how sync is driven and credentialed.
//! A front end — GTK on a desk, Compose on Android, SwiftUI on iOS — owns
//! widgets, gestures, threads and the main loop, and asks this crate
//! everything else.
//!
//! It was written three times before it was written here: once in Rust
//! for the desktop, then in Kotlin, then in Swift, each time with the same
//! decisions and slightly different bugs. The two mobile copies could not
//! use the desktop crate because it assumed a desktop in four places —
//! host fonts, environment credentials, a bundled TLS stack, and a shelf
//! that only imports — so those four are now the [`Platform`] a front end
//! hands in, and the crate assumes nothing about where it runs.
//!
//! # Policy versus platform
//!
//! The line is drawn by one question: *could two apps on the same device
//! reasonably answer this differently?* If the answer is no — a shelf
//! opens on the book the reader was in; a credential is keyed by origin
//! and never by a catalog URL; a 401 is a login, not a failure; a download
//! is one job per entry; a position is saved before the screen goes — it
//! is policy and lives here. If the answer depends on the operating
//! system — how a file grant is expressed, which store keeps a secret,
//! which HTTP stack honours the device's trust store, when the process is
//! about to be killed — it is a platform capability, and it arrives
//! through [`Platform`] or as a plain number the policy is parameterised
//! by. `docs/APP.md` walks the whole list for mobile, desktop and e-ink.
//!
//! # Threads
//!
//! [`App`] holds a library connection and is one thread's at a time, like
//! a session; a front end keeps it on whichever thread it keeps the shelf
//! on. A [`Catalog`] is the same, on whichever thread does the blocking
//! fetches. The threads the application needs beyond that — the session's
//! loader, the sync driver — are owned by the engine crates and reach back
//! only through wakers. Nothing here spawns for a front end, because every
//! platform has a better answer to "run this off the main thread" than a
//! library could invent.
//!
//! # Words
//!
//! The crate answers in enums and numbers, not sentences: a
//! [`BrowseState`], a [`Readout`], a [`DownloadOutcome`]. The
//! `describe_*` functions at the bottom are the desktop front ends' words,
//! kept here so the GTK app and the CLI agree; a localised front end words
//! its own.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chapbook_core::{ChapbookError, CredentialStore, FontSource, NoCredentials, Result, Source};
use chapbook_library::{BookId, BookQuery, BookRecord, Library, ReadingState, Sort};
use chapbook_reader::{open_publication, Session, SessionConfig};

pub use chapbook_library;
#[cfg(feature = "catalog")]
pub use chapbook_opds;
pub use chapbook_reader;
#[cfg(feature = "catalog")]
pub use chapbook_sync;

pub mod reader;

#[cfg(feature = "catalog")]
mod catalog;
#[cfg(feature = "catalog")]
mod download;
#[cfg(feature = "catalog")]
mod sync;

#[cfg(feature = "catalog")]
pub use catalog::{BrowseState, Catalog, Download, Facet, SavedCatalog};
#[cfg(feature = "catalog")]
pub use download::DownloadOutcome;
pub use reader::{ProgressLabel, Readout};
#[cfg(feature = "catalog")]
pub use sync::{SyncDriver, SyncRequest, SyncStatus};

#[cfg(feature = "catalog")]
use chapbook_reader::HttpClient;

/// What a front end supplies and the policy never guesses.
///
/// Each field replaces an assumption the desktop crate used to make, and
/// each assumption held on exactly one platform: fonts installed on the
/// host (a desktop), credentials in the environment (a terminal), a
/// bundled TLS stack that ignores the device's trust store (a desktop
/// process), a device name a service can show (nobody's). A phone injects
/// all four; [`Platform::desktop`] fills them the desktop way.
pub struct Platform {
    /// Where faces come from. Required for the reason `Session::open`
    /// gives: a session with no faces paginates every book to one blank
    /// page and calls it success.
    pub fonts: FontSource,
    /// Where secrets live. The policy stores under an origin and reads
    /// back by origin; the store decides what a Keychain, a Keystore or a
    /// file does with that.
    pub credentials: Arc<dyn CredentialStore>,
    /// How bytes are fetched. `None` means the bundled transport, which
    /// exists only in a `bundled-http` build; anywhere else it is an error
    /// at the first fetch rather than a silently wrong trust store.
    #[cfg(feature = "catalog")]
    pub transport: Option<Arc<dyn HttpClient>>,
    /// What a progression service shows beside this device's position.
    pub device_name: String,
}

impl Platform {
    /// A platform that knows its fonts and nothing else yet: no
    /// credentials, no transport, a generic device name.
    pub fn new(fonts: FontSource) -> Platform {
        Platform {
            fonts,
            credentials: Arc::new(NoCredentials),
            #[cfg(feature = "catalog")]
            transport: None,
            device_name: "chapbook".to_string(),
        }
    }

    /// The desktop's answers: host fonts, `CHAPBOOK_OPDS_USER` and
    /// `_PASSWORD` from the environment, the bundled transport.
    #[cfg(feature = "desktop")]
    pub fn desktop() -> Platform {
        Platform::new(FontSource::host())
            .with_credentials(Arc::new(chapbook_core::EnvCredentials))
            .with_device_name("chapbook-app")
    }

    pub fn with_credentials(mut self, credentials: Arc<dyn CredentialStore>) -> Platform {
        self.credentials = credentials;
        self
    }

    #[cfg(feature = "catalog")]
    pub fn with_transport(mut self, transport: Arc<dyn HttpClient>) -> Platform {
        self.transport = Some(transport);
        self
    }

    pub fn with_device_name(mut self, name: impl Into<String>) -> Platform {
        self.device_name = name.into();
        self
    }
}

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
            // is looking for what they were reading. The CLI's `lib ls`,
            // the desktop, Android and iOS all answered this the same way
            // separately; now they answer it here.
            sort: Sort::Read,
            state: None,
            series: None,
        }
    }
}

/// What opening a shelf row came to.
///
/// Custody follows the grant, and this is where it comes back. A book the
/// library holds a copy of opens here and now. An *adopted* book — one
/// the platform owns and the library only records — comes back as the
/// token that reaches it, because only the platform can turn that token
/// into an open file: a content resolver, a security-scoped bookmark. The
/// front end resolves it and opens a session over the descriptor with
/// [`App::session_config`], then everything else is the same.
pub enum Opened {
    /// The library's own copy, open.
    Session(Box<Session>),
    /// The platform's file: here is how the app once said to reach it.
    Adopted { fingerprint: String, grant: Vec<u8> },
    /// Out of reach: the copy is gone from disk, or an adopted book whose
    /// grant was never kept or has been forgotten. The row survives; the
    /// reader is told the file is out of reach rather than shown a crash.
    Missing,
}

/// One running application: a library, its directory, the platform it
/// runs on, and the sync driver once something has asked for one.
///
/// One thread's at a time and not `Sync`, like the library connection it
/// holds; see the crate documentation for how a front end places it.
pub struct App {
    dir: PathBuf,
    library: Library,
    platform: Platform,
    #[cfg(feature = "catalog")]
    sync: Option<SyncDriver>,
}

impl App {
    /// Open the application over the library at `dir`, on `platform`.
    ///
    /// The directory is the front end's to name — a sandboxed app knows
    /// its own answer and nothing else can — and everything the app
    /// persists lives under it: the library, its book copies and covers,
    /// the grants, the preferences, the device identity.
    pub fn open(dir: &Path, platform: Platform) -> Result<App> {
        let library = Library::open(dir)?;
        Ok(App {
            dir: dir.to_path_buf(),
            library,
            platform,
            #[cfg(feature = "catalog")]
            sync: None,
        })
    }

    /// The desktop application: [`Platform::desktop`] over `dir`, or over
    /// the platform's own library directory when `None`.
    #[cfg(feature = "desktop")]
    pub fn desktop(dir: Option<&Path>) -> Result<App> {
        let dir = match dir {
            Some(dir) => dir.to_path_buf(),
            None => Library::default_dir()?,
        };
        App::open(&dir, Platform::desktop())
    }

    /// The library directory everything persists under.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn platform(&self) -> &Platform {
        &self.platform
    }

    /// The library itself, for a front end with a question the policy
    /// does not ask — a collection to manage, a series to list.
    pub fn library(&mut self) -> &mut Library {
        &mut self.library
    }

    /// The one way this application configures a session: the platform's
    /// fonts, the app's library directory (which is what restores the
    /// reading position), the platform's credentials and transport. The
    /// front end owns the session it opens with it — its waker, its
    /// metrics, its cache budget, and calling `save_position` before
    /// letting go of it.
    pub fn session_config(&self) -> SessionConfig {
        let config = SessionConfig::new(self.platform.fonts.clone())
            .with_library_dir(&self.dir)
            .with_credentials(self.platform.credentials.clone());
        #[cfg(feature = "catalog")]
        let config = match &self.platform.transport {
            Some(transport) => config.with_transport(transport.clone()),
            None => config,
        };
        config
    }

    // ---- The shelf ----

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

    /// Remove a book from the shelf. Soft: importing the same file again
    /// brings back its position and marks.
    pub fn remove(&mut self, id: BookId) -> Result<()> {
        self.library.delete_book(id)
    }

    /// Mark a book finished, or take it back.
    pub fn set_finished(&mut self, id: BookId, finished: bool) -> Result<()> {
        self.library.set_finished(id, finished)
    }

    // ---- Custody ----
    //
    // Two doors in, one door out. A file the app may keep reaching is
    // *adopted*: recorded by content, no copy, and the grant that reaches
    // it kept under the fingerprint. A file the app will never see again —
    // a one-shot share, a viewer intent, a desktop import — is copied
    // while the bytes are still ours. Which door a given file takes is the
    // platform's call, because only the platform knows what its grants
    // are worth; what each door does is the same everywhere and lives
    // here.

    /// Import: copy a file into the library and return its shelf record.
    pub fn import(&mut self, path: &Path) -> Result<BookRecord> {
        let publication = open_publication(path)?;
        let id = self.library.import(path, publication.as_ref())?;
        Ok(self.library.book(id)?.expect("just imported"))
    }

    /// Adopt: record a book the platform owns, from a source that is not
    /// a path, and remember `grant` as the way to reach it again.
    ///
    /// Opening is what records the book, so a session is opened with the
    /// app's configuration, asked which row it became, and dropped —
    /// nothing is saved on the way out, so this leaves no mark. The same
    /// bytes adopted twice are one row, and a book imported by path
    /// elsewhere and adopted here resolve to the same record.
    pub fn adopt(&mut self, source: Source, grant: &[u8]) -> Result<BookId> {
        let session = Session::open_with(source, self.session_config())?;
        self.adopt_open(&session, grant)
    }

    /// [`App::adopt`] for a session the front end already opened — over a
    /// descriptor, with [`App::session_config`].
    pub fn adopt_open(&mut self, session: &Session, grant: &[u8]) -> Result<BookId> {
        let id = session.book_id().ok_or_else(|| {
            ChapbookError::Library(
                "the session reached no shelf, so there is nothing to adopt".into(),
            )
        })?;
        let record = self
            .library
            .book(id)?
            .ok_or_else(|| ChapbookError::Library(format!("book #{} left the shelf", id.0)))?;
        self.library.set_grant(&record.fingerprint, grant)?;
        Ok(id)
    }

    /// The grant that reaches an adopted book, if one was remembered.
    pub fn grant(&self, fingerprint: &str) -> Result<Option<Vec<u8>>> {
        self.library.grant(fingerprint)
    }

    /// Remember how to reach a book, by fingerprint.
    pub fn remember_grant(&mut self, fingerprint: &str, grant: &[u8]) -> Result<()> {
        self.library.set_grant(fingerprint, grant)
    }

    /// Forget how to reach a book.
    pub fn forget_grant(&mut self, fingerprint: &str) -> Result<()> {
        self.library.clear_grant(fingerprint)
    }

    /// Open a shelf row for reading, whichever door it came in through.
    pub fn open_book(&self, id: BookId) -> Result<Opened> {
        let record = self
            .library
            .book(id)?
            .ok_or_else(|| ChapbookError::Library(format!("book #{} is not on the shelf", id.0)))?;
        if record.file_path.as_os_str().is_empty() {
            return Ok(match self.library.grant(&record.fingerprint)? {
                Some(grant) => Opened::Adopted {
                    fingerprint: record.fingerprint,
                    grant,
                },
                None => Opened::Missing,
            });
        }
        if !record.file_path.is_file() {
            return Ok(Opened::Missing);
        }
        let session = Session::open_with(record.file_path.as_path(), self.session_config())?;
        Ok(Opened::Session(Box::new(session)))
    }

    // ---- Preferences ----

    /// What the reader's progress readout says. Percent until the reader
    /// chooses otherwise.
    pub fn progress_label(&self) -> ProgressLabel {
        self.library
            .preference(PREF_PROGRESS_LABEL)
            .ok()
            .flatten()
            .and_then(|value| ProgressLabel::parse(&value))
            .unwrap_or_default()
    }

    pub fn set_progress_label(&mut self, label: ProgressLabel) -> Result<()> {
        self.library
            .set_preference(PREF_PROGRESS_LABEL, label.as_str())
    }

    // ---- Catalogs ----

    /// The catalogs the reader has added, in the order they were added.
    #[cfg(feature = "catalog")]
    pub fn catalogs(&self) -> Result<Vec<SavedCatalog>> {
        Ok(self
            .library
            .opds_sources()?
            .into_iter()
            .map(SavedCatalog::from)
            .collect())
    }

    /// One saved catalog, `None` once removed.
    #[cfg(feature = "catalog")]
    pub fn catalog(&self, id: i64) -> Result<Option<SavedCatalog>> {
        Ok(self.library.opds_source(id)?.map(SavedCatalog::from))
    }

    /// Add a catalog. `title` may be blank until its feed says what it is
    /// called; surrounding whitespace on either is the reader's typing.
    #[cfg(feature = "catalog")]
    pub fn add_catalog(&mut self, url: &str, title: &str) -> Result<SavedCatalog> {
        let (url, title) = (url.trim(), title.trim());
        let id = self
            .library
            .add_opds_source(url, (!title.is_empty()).then_some(title), None)?;
        Ok(SavedCatalog {
            id,
            title: title.to_string(),
            url: url.to_string(),
        })
    }

    #[cfg(feature = "catalog")]
    pub fn rename_catalog(&mut self, id: i64, title: &str) -> Result<bool> {
        let title = title.trim();
        self.library
            .rename_opds_source(id, (!title.is_empty()).then_some(title))
    }

    /// Take a catalog off the list. Its credential stays in the store —
    /// keyed by origin, it may serve another catalog on the same host —
    /// and its books stay on the shelf.
    #[cfg(feature = "catalog")]
    pub fn remove_catalog(&mut self, id: i64) -> Result<bool> {
        self.library.remove_opds_source(id)
    }

    /// Browse a saved catalog: a [`Catalog`] over the platform's transport
    /// and credential store, titled after the saved row until its feed
    /// says otherwise.
    #[cfg(feature = "catalog")]
    pub fn browse(&self, saved: &SavedCatalog) -> Result<Catalog> {
        Ok(Catalog::new(
            self.catalog_client()?,
            self.platform.credentials.clone(),
            saved.title.clone(),
        ))
    }

    /// A [`Catalog`] over no saved row — a URL the reader pasted, a
    /// catalog being previewed before it is added.
    #[cfg(feature = "catalog")]
    pub fn open_catalog(&self) -> Result<Catalog> {
        Ok(Catalog::new(
            self.catalog_client()?,
            self.platform.credentials.clone(),
            String::new(),
        ))
    }

    /// Everything a download's landing does, in one place: import the
    /// file the platform's transfer produced and record the sync services
    /// the catalog entry advertised, which live in that entry and nowhere
    /// else. The file is the caller's and is left where it was; importing
    /// the same bytes twice answers with the row they already have, so a
    /// retried job needs no bookkeeping of its own.
    #[cfg(feature = "catalog")]
    pub fn land_download(
        &mut self,
        file: &Path,
        progression_url: Option<&str>,
        annotation_container: Option<&str>,
    ) -> Result<BookId> {
        let publication = open_publication(file)?;
        let id = self.library.import(file, publication.as_ref())?;
        if progression_url.is_some() || annotation_container.is_some() {
            self.library
                .set_sync_targets(id, progression_url, annotation_container)?;
        }
        Ok(id)
    }

    // ---- Sync ----

    /// Ask for every syncable book to reconcile, spawning the driver on
    /// first use. Returns `Ok(false)` — and asks for nothing — when no
    /// book has a service to sync with, which is a fact to tell the reader
    /// rather than a spinner to show them.
    ///
    /// `waker` is called from the driver's thread after each report lands;
    /// it should only nudge the UI thread to come drain
    /// [`App::sync_events`].
    #[cfg(feature = "catalog")]
    pub fn sync_all(&mut self, waker: Arc<dyn Fn() + Send + Sync>) -> Result<bool> {
        if self.library.books_with_sync_targets()?.is_empty() {
            return Ok(false);
        }
        self.sync_driver(waker)?.request(SyncRequest::All);
        Ok(true)
    }

    /// Ask for one book to reconcile.
    #[cfg(feature = "catalog")]
    pub fn sync_book(&mut self, id: BookId, waker: Arc<dyn Fn() + Send + Sync>) -> Result<()> {
        self.sync_driver(waker)?.request(SyncRequest::Book(id));
        Ok(())
    }

    /// Everything sync has reported since the last drain.
    #[cfg(feature = "catalog")]
    pub fn sync_events(&self) -> Vec<SyncStatus> {
        self.sync
            .as_ref()
            .map(|driver| driver.drain())
            .unwrap_or_default()
    }

    /// One sync report as a line a shell can show. The desktop's words;
    /// see the crate documentation.
    #[cfg(feature = "catalog")]
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

    #[cfg(feature = "catalog")]
    fn title_of(&self, id: BookId) -> String {
        self.library
            .book(id)
            .ok()
            .flatten()
            .map(|record| record.title)
            .unwrap_or_else(|| format!("book #{}", id.0))
    }

    #[cfg(feature = "catalog")]
    fn sync_driver(&mut self, waker: Arc<dyn Fn() + Send + Sync>) -> Result<&SyncDriver> {
        if self.sync.is_none() {
            // The engine owns its own connection rather than sharing this
            // model's: it lives on the driver's thread.
            let library = Library::open(&self.dir)?;
            let device = sync::device_identity(&self.dir, &self.platform.device_name)?;
            let engine = chapbook_sync::SyncEngine::new(library, self.transport()?, device);
            self.sync = Some(SyncDriver::spawn(
                engine,
                self.platform.credentials.clone(),
                waker,
            ));
        }
        Ok(self.sync.as_ref().expect("just spawned"))
    }

    /// The transport every service call goes through: the platform's, or
    /// the bundled one where this build carries it.
    #[cfg(feature = "catalog")]
    fn transport(&self) -> Result<Arc<dyn HttpClient>> {
        if let Some(transport) = &self.platform.transport {
            return Ok(transport.clone());
        }
        #[cfg(feature = "bundled-http")]
        {
            Ok(Arc::new(chapbook_opds::UreqHttp::new()))
        }
        #[cfg(not(feature = "bundled-http"))]
        {
            Err(ChapbookError::Network(
                "this build bundles no transport; the platform must supply one".into(),
            ))
        }
    }

    #[cfg(feature = "catalog")]
    fn catalog_client(&self) -> Result<chapbook_opds::OpdsClient> {
        Ok(chapbook_opds::OpdsClient::new(SharedTransport(
            self.transport()?,
        )))
    }
}

const PREF_PROGRESS_LABEL: &str = "progress_label";

/// A platform's one transport, lent to a client that wants to own one.
#[cfg(feature = "catalog")]
struct SharedTransport(Arc<dyn HttpClient>);

#[cfg(feature = "catalog")]
impl HttpClient for SharedTransport {
    fn get(
        &self,
        request: chapbook_opds::HttpRequest,
    ) -> std::result::Result<chapbook_opds::HttpResponse, chapbook_opds::HttpError> {
        self.0.get(request)
    }

    fn send(
        &self,
        method: chapbook_opds::http::HttpMethod,
        request: chapbook_opds::HttpRequest,
        body: Option<Vec<u8>>,
    ) -> std::result::Result<chapbook_opds::HttpResponse, chapbook_opds::HttpError> {
        self.0.send(method, request, body)
    }
}

// ---- The desktop's words ----
//
// Every function below turns a fact into an English sentence. They are
// here rather than in the GTK shell so the desktop app and the CLI say
// the same thing; a front end with a strings table words its own.

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

/// A shelf row's state, in the words every desktop front end shows:
/// "unread", "reading 42%", "finished". Matches the CLI's `describe_state`,
/// which is the point — the two doors agree.
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
