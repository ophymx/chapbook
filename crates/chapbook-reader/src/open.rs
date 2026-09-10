//! Opening a book: source and format resolution, the library handshake
//! (match/import/adopt, position restore, stored annotations), and
//! session construction.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};

#[cfg(feature = "opds")]
use chapbook_core::CredentialStore;
#[cfg(feature = "library")]
use chapbook_core::SpineItem;
use chapbook_core::{Format, PixelFormat, Publication, Result, Source};
use chapbook_paint::{FrameIntent, ImageStore};

#[cfg(feature = "library")]
use crate::annotations::StoredAnnotation;
use crate::frame::PendingDamage;
#[cfg(feature = "_image-book")]
use crate::loader::{LoadSource, Loader};
#[cfg(feature = "library")]
use crate::text_surface::unit_locator_text;
#[cfg(not(feature = "library"))]
use crate::ReadingSettings;
use crate::{FontSource, OpenedBookId, Session, SessionConfig, WakerCell, DEFAULT_CACHE_BUDGET};
#[cfg(feature = "opds")]
use chapbook_opds::http::HttpClient;

/// The open publication. Text units need the concrete EPUB surface
/// (relative resource resolution, stylesheets) that deliberately isn't on
/// the `Publication` trait, so the session keeps the concrete type.
pub(crate) enum OpenBook {
    Epub(Box<chapbook_epub::Book>),
    /// Any image-per-page publication: a local CBZ, or a streamed PSE
    /// feed. Both arrive as a trait object, so neither format needs a
    /// variant of its own.
    #[cfg(feature = "_comic")]
    Comic(Arc<dyn Publication + Send + Sync>),
    /// PDFs keep their concrete type: the loader renders straight to RGBA
    /// and extracts the text layer through it.
    #[cfg(feature = "pdf")]
    Pdf(Arc<chapbook_pdf::PdfBook>),
}

impl OpenBook {
    pub(crate) fn publication(&self) -> &dyn Publication {
        match self {
            OpenBook::Epub(book) => book.as_ref(),
            #[cfg(feature = "_comic")]
            OpenBook::Comic(comic) => comic.as_ref(),
            #[cfg(feature = "pdf")]
            OpenBook::Pdf(pdf) => pdf.as_ref(),
        }
    }

    #[cfg(feature = "_image-book")]
    fn load_source(&self) -> Option<LoadSource> {
        match self {
            OpenBook::Epub(_) => None,
            #[cfg(feature = "_comic")]
            OpenBook::Comic(comic) => Some(LoadSource::Comic(comic.clone())),
            #[cfg(feature = "pdf")]
            OpenBook::Pdf(pdf) => Some(LoadSource::Pdf(pdf.clone())),
        }
    }
}

/// Ask the host's store for this catalog's credential and apply it.
///
/// Returns whether the client came away with one. Nothing here learns the
/// scheme: the store hands back a complete `Authorization` header value and
/// `opds-client` sends it verbatim, which is what lets a bearer token
/// arrive later without touching this code or the C ABI that will wrap it.
#[cfg(feature = "opds")]
fn authorize(
    client: &mut chapbook_opds::OpdsClient,
    store: &dyn CredentialStore,
    key: Option<&chapbook_core::CredentialKey>,
    freshness: chapbook_core::Freshness,
) -> bool {
    use chapbook_core::CredentialLookup;
    let Some(key) = key else {
        return false;
    };
    // The key is safe to print: it is an origin or a row id, never the
    // secret-bearing path a catalog URL may carry.
    match store.get(key, freshness) {
        CredentialLookup::Found(credential) => {
            client.set_authorization(credential.authorization);
            true
        }
        CredentialLookup::Missing => false,
        CredentialLookup::Locked => {
            log::warn!("credentials for {key} are stored but locked right now");
            false
        }
        CredentialLookup::Failed(reason) => {
            log::warn!("credential store failed for {key}: {reason}");
            false
        }
    }
}

/// Decide a format from the bytes, falling back to the name and then to
/// EPUB.
///
/// Bytes first is the point: an extension is a claim by whoever named the
/// file, and a `.epub` that is really a comic archive is a parse error
/// today. The name is consulted only when the bytes say nothing, and the
/// final fallback to EPUB preserves an existing behaviour worth keeping —
/// rbook opens an *unzipped* EPUB directory, which has no leading bytes to
/// read at all.
fn format_of_path(path: &Path) -> Format {
    let sniffed = std::fs::File::open(path).ok().and_then(|mut file| {
        let mut head = vec![0u8; chapbook_core::FORMAT_SNIFF_BYTES];
        let read = std::io::Read::read(&mut file, &mut head).ok()?;
        head.truncate(read);
        Format::sniff(&head)
    });
    sniffed
        .or_else(|| Format::from_extension(path))
        .unwrap_or(Format::Epub)
}

/// Honour a format the host stated; sniff only when it said `Guess`.
///
/// A host that resolved a `content://` URI already asked the resolver for a
/// MIME type, and that answer is better than ours — it may know things the
/// first 64 bytes cannot say.
fn resolve_format(stated: Format, head: &[u8]) -> Result<Format> {
    match stated {
        Format::Guess => Format::sniff(head).ok_or_else(|| {
            chapbook_core::ChapbookError::BookMalformed(
                "not an EPUB, CBZ or PDF: the leading bytes match no format chapbook reads".into(),
            )
        }),
        stated => Ok(stated),
    }
}

/// The first bytes of a handle, rewound afterwards so the format reader
/// gets an untouched stream.
fn peek(reader: &mut dyn chapbook_core::ReadSeek) -> Result<Vec<u8>> {
    let mut head = vec![0u8; chapbook_core::FORMAT_SNIFF_BYTES];
    let read = reader.read(&mut head)?;
    head.truncate(read);
    reader.seek(std::io::SeekFrom::Start(0))?;
    Ok(head)
}

/// The format is not compiled in. Its own function so every arm reports it
/// the same way — and `cfg`'d, because with every format built there is no
/// arm left to call it.
#[cfg(any(not(feature = "cbz"), not(feature = "pdf")))]
fn not_built(format: Format) -> chapbook_core::ChapbookError {
    chapbook_core::ChapbookError::FormatNotBuilt(format.as_str())
}

fn book_from_bytes(format: Format, bytes: Vec<u8>) -> Result<OpenBook> {
    match format {
        Format::Epub | Format::Guess => Ok(OpenBook::Epub(Box::new(
            chapbook_epub::Book::from_bytes(bytes)?,
        ))),
        #[cfg(feature = "cbz")]
        Format::Cbz => Ok(OpenBook::Comic(Arc::new(
            chapbook_cbz::ComicBook::from_bytes(bytes)?,
        ))),
        #[cfg(feature = "pdf")]
        Format::Pdf => Ok(OpenBook::Pdf(Arc::new(chapbook_pdf::PdfBook::from_bytes(
            bytes,
        )?))),
        #[cfg(not(feature = "cbz"))]
        Format::Cbz => Err(not_built(format)),
        #[cfg(not(feature = "pdf"))]
        Format::Pdf => Err(not_built(format)),
    }
}

fn book_from_reader(format: Format, reader: Box<dyn chapbook_core::ReadSeek>) -> Result<OpenBook> {
    match format {
        Format::Epub | Format::Guess => {
            Ok(OpenBook::Epub(Box::new(chapbook_epub::Book::read(reader)?)))
        }
        #[cfg(feature = "cbz")]
        Format::Cbz => Ok(OpenBook::Comic(Arc::new(chapbook_cbz::ComicBook::read(
            reader,
        )?))),
        #[cfg(feature = "pdf")]
        Format::Pdf => Ok(OpenBook::Pdf(Arc::new(chapbook_pdf::PdfBook::read(
            reader,
        )?))),
        #[cfg(not(feature = "cbz"))]
        Format::Cbz => Err(not_built(format)),
        #[cfg(not(feature = "pdf"))]
        Format::Pdf => Err(not_built(format)),
    }
}

/// Open a book file as a [`Publication`], deciding its format from its
/// bytes and falling back to the name only when they say nothing.
///
/// The reader has always sniffed this way — "a book is its bytes, not
/// its name" is one of the workspace's own invariants — but the logic
/// was private, so every caller that wanted a `Publication` without a
/// whole session went off and dispatched on the file extension instead,
/// which is the thing the invariant exists to prevent. This is that
/// logic, lent out: metadata for an import, a cover, a format check.
///
/// A session does not need this — it opens for itself — but importing a
/// downloaded file does, and so does anything that wants to know what a
/// file *is* before committing to it.
pub fn open_publication(path: &Path) -> Result<Box<dyn chapbook_core::Publication>> {
    Ok(match book_at_path(format_of_path(path), path)? {
        OpenBook::Epub(book) => book as Box<dyn chapbook_core::Publication>,
        #[cfg(feature = "_comic")]
        OpenBook::Comic(book) => Box::new(ArcPublication(book)),
        #[cfg(feature = "pdf")]
        OpenBook::Pdf(book) => Box::new(ArcPublication(book)),
    })
}

/// A shared publication, borrowed as an owned one. Image-book readers
/// are held behind `Arc` because the loader thread shares them; a
/// caller that only wants metadata should not have to know that.
#[cfg(feature = "_image-book")]
struct ArcPublication<T: ?Sized>(std::sync::Arc<T>);

#[cfg(feature = "_image-book")]
impl<T: chapbook_core::Publication + ?Sized> chapbook_core::Publication for ArcPublication<T> {
    fn kind(&self) -> chapbook_core::BookKind {
        self.0.kind()
    }
    fn metadata(&self) -> &chapbook_core::BookMetadata {
        self.0.metadata()
    }
    fn spine(&self) -> &[chapbook_core::SpineItem] {
        self.0.spine()
    }
    fn toc(&self) -> &[chapbook_core::TocEntry] {
        self.0.toc()
    }
    fn unit_bytes(&self, spine_index: usize) -> Result<Vec<u8>> {
        self.0.unit_bytes(spine_index)
    }
    fn cover(&self) -> Result<Option<chapbook_core::Resource>> {
        self.0.cover()
    }
}

fn book_at_path(format: Format, path: &Path) -> Result<OpenBook> {
    match format {
        Format::Epub | Format::Guess => {
            Ok(OpenBook::Epub(Box::new(chapbook_epub::Book::open(path)?)))
        }
        #[cfg(feature = "cbz")]
        Format::Cbz => Ok(OpenBook::Comic(Arc::new(chapbook_cbz::ComicBook::open(
            path,
        )?))),
        #[cfg(feature = "pdf")]
        Format::Pdf => Ok(OpenBook::Pdf(Arc::new(chapbook_pdf::PdfBook::open(path)?))),
        #[cfg(not(feature = "cbz"))]
        Format::Cbz => Err(not_built(format)),
        #[cfg(not(feature = "pdf"))]
        Format::Pdf => Err(not_built(format)),
    }
}

/// Match an opened book into the library: by edition fingerprint, then by
/// publication identifier for a replaced edition, else as a new record —
/// imported (copied) when there is a path, adopted (recorded, not copied)
/// when there is not. Returns the book's id and whether the stored state
/// was written against this same edition; `false` routes restore through
/// the re-anchor chain.
#[cfg(feature = "library")]
fn shelve(
    library: &mut chapbook_library::Library,
    book: &OpenBook,
    fingerprint: &str,
    path: Option<&Path>,
) -> (Option<chapbook_library::BookId>, bool) {
    if let Ok(Some(id)) = library.find_by_fingerprint(fingerprint) {
        return (Some(id), true);
    }
    if let Some(identifier) = &book.publication().metadata().identifier {
        if let Ok(Some(id)) = library.find_by_identifier(identifier) {
            let _ = match path {
                Some(path) => library.update_edition(id, path),
                None => library.update_edition_fingerprint(id, fingerprint),
            };
            return (Some(id), false);
        }
    }
    let id = match path {
        Some(path) => library.import(path, book.publication()).ok(),
        None => library.adopt(fingerprint, book.publication()).ok(),
    };
    (id, true)
}

/// Where the reader left off, from the library — the start unit and the
/// offset held pending until that unit lays out. `(0, None)` wherever
/// there is nothing stored or nobody to ask.
#[cfg(feature = "library")]
fn restored_start(
    library: &Option<chapbook_library::Library>,
    book: &OpenBook,
    book_id: OpenedBookId,
    same_edition: bool,
) -> (usize, Option<u32>) {
    match (library, book_id) {
        (Some(lib), Some(id)) => match lib.position(id) {
            Ok(Some(stored)) => {
                let (locator, tier) = chapbook_library::restore_position(
                    &stored.locator,
                    same_edition,
                    book.publication().spine(),
                    |i| unit_locator_text(book.publication(), i),
                );
                log::info!("resuming at unit {} ({tier:?})", locator.spine_index + 1);
                (locator.spine_index, Some(locator.char_offset))
            }
            _ => (0, None),
        },
        _ => (0, None),
    }
}

impl Session {
    /// Open a book from a path or an `http(s)://` OPDS URL (resolved to a
    /// PSE page stream). Local books are matched into the library
    /// (fingerprint, then identifier for replaced editions, else imported)
    /// and their stored position restored; streams skip the library (no
    /// local file to fingerprint) but honor `pse:lastRead`.
    ///
    /// The format comes from the *bytes*, not the extension — see
    /// [`chapbook_core::Format::sniff`]. A book whose name lies about it
    /// opens correctly. A host with no path at all passes a
    /// [`Source`] to [`Session::open_with`] instead.
    ///
    /// `fonts` is required rather than defaulted. A session that finds no
    /// faces lays out, renders, paints and *conforms* — it just paginates
    /// every book to a single blank page, taking navigation, search and
    /// the table of contents down with it — and fontdb has no Android, iOS
    /// or wasm branch, so "no faces" is the ordinary case on three
    /// platforms. Desktop shells want [`FontSource::host`]; anything whose
    /// output is compared against a golden wants
    /// [`FontSource::embedded`]. See `chapbook_core::font`.
    ///
    /// Credentials default to none; a shell that can reach a host store —
    /// or just an environment — passes one through
    /// [`Session::open_with`].
    pub fn open(source: &str, fonts: FontSource) -> Result<Session> {
        Session::open_with(source, SessionConfig::new(fonts))
    }

    /// [`Session::open`], with the source typed and the host's capabilities
    /// supplied explicitly.
    ///
    /// The form every non-desktop shell wants, and the one the FFI will
    /// wrap: nothing in here is reached for behind the caller's back.
    ///
    /// `source` accepts anything that becomes a [`Source`] — a `&str` or
    /// `String` still means what it always did (`http(s)://` is a catalog,
    /// anything else a path), a `PathBuf` is a file, and
    /// [`Source::bytes`] / [`Source::reader`] are for hosts that have no
    /// path to give: an Android `content://` URI resolved to a file
    /// descriptor, an iOS security-scoped file, a WASM `ArrayBuffer`.
    ///
    /// Every local source reaches the library. A path is imported — copied
    /// into the library, which owns it from then on. Bytes and handles are
    /// *adopted*: hashed on the way in and recorded under the same edition
    /// fingerprint a path import gets, so positions, annotations and
    /// per-book settings key on the book's identity while the file stays
    /// wherever the platform keeps it. Reaching that file again on the
    /// next launch — the bookmark, the URI grant — is the shell's half of
    /// custody; see `docs/PLATFORM.md`.
    pub fn open_with(source: impl Into<Source>, config: SessionConfig) -> Result<Session> {
        let source = source.into();
        let SessionConfig {
            fonts,
            // Only the OPDS path has anything to authenticate; a build
            // without it still takes the store, so a shell's construction
            // code does not change with the feature set.
            #[cfg_attr(not(feature = "opds"), allow(unused_variables))]
            credentials,
            #[cfg(feature = "opds")]
            transport,
            library_dir,
            cache_budget,
        } = config;

        // Resolved once, and used for the library and the page cache both.
        // A platform with no default is not a failure to open a book: the
        // session reads on without a library, exactly as it does when the
        // database itself cannot be opened.
        #[cfg(feature = "library")]
        let library_dir = match library_dir {
            Some(dir) => Some(dir),
            None => chapbook_library::Library::default_dir()
                .map_err(|e| log::warn!("reading without a library: {e}"))
                .ok(),
        };
        #[cfg(not(feature = "library"))]
        let _ = library_dir;
        #[cfg(feature = "library")]
        let mut library = library_dir.as_ref().and_then(|dir| {
            chapbook_library::Library::open(dir)
                .map_err(|e| log::warn!("library unavailable: {e}"))
                .ok()
        });

        #[cfg_attr(not(feature = "library"), allow(unused_variables))]
        let (book, book_id, start_spine, pending_offset, same_edition): (
            OpenBook,
            OpenedBookId,
            usize,
            Option<u32>,
            bool,
        ) = match source {
            Source::Url(_source) => {
                #[cfg(not(feature = "opds"))]
                return Err(chapbook_core::ChapbookError::FormatNotBuilt("OPDS"));
                #[cfg(feature = "opds")]
                {
                    let source = _source.as_str();
                    use chapbook_core::Freshness;

                    // Keyed by origin, not by the URL: the path may carry a
                    // per-user API key, and a catalog that moves its path must
                    // not lose its login. See `chapbook_core::credential`.
                    let key = chapbook_core::CredentialKey::http_origin(source);
                    // Unlike the library, a page stream cannot do without
                    // this: every page is a fetch that has to land somewhere.
                    let Some(cache) = library_dir.as_ref().map(|dir| dir.join("pse-cache")) else {
                        return Err(chapbook_core::ChapbookError::Library(
                            "no library location for the page cache; pass \
                         SessionConfig::with_library_dir"
                                .into(),
                        ));
                    };
                    let store = credentials.as_ref();

                    // One transport, however many clients the auth flow needs.
                    let http: Arc<dyn HttpClient> = match transport {
                        Some(host) => host,
                        #[cfg(feature = "ureq")]
                        None => Arc::new(chapbook_opds::UreqHttp::new()),
                        #[cfg(not(feature = "ureq"))]
                        None => {
                            return Err(chapbook_core::ChapbookError::Network(
                                "this build has no bundled HTTP transport; pass one with \
                             SessionConfig::with_transport"
                                    .into(),
                            ))
                        }
                    };

                    let mut client = chapbook_opds::OpdsClient::new(http.clone());
                    authorize(&mut client, store, key.as_ref(), Freshness::Cached);
                    let opened = chapbook_opds::StreamedComic::open(client, source, &cache);

                    // One retry, and only on a 401. `Freshness::Renewed` is
                    // what makes an expiring secret work: a store backed by a
                    // refreshable token can produce a new one here, and a
                    // store that cannot says so by returning nothing, which
                    // costs exactly one skipped retry. Prompting is not our
                    // job — the error carries the server's Authentication
                    // Document so the shell can do it.
                    let comic = match opened {
                        Err(chapbook_opds::OpdsError::AuthRequired(doc)) => {
                            let mut retry = chapbook_opds::OpdsClient::new(http.clone());
                            if !authorize(&mut retry, store, key.as_ref(), Freshness::Renewed) {
                                return Err(chapbook_opds::to_chapbook_error(
                                    chapbook_opds::OpdsError::AuthRequired(doc),
                                ));
                            }
                            chapbook_opds::StreamedComic::open(retry, source, &cache)
                                .map_err(chapbook_opds::to_chapbook_error)?
                        }
                        other => other.map_err(chapbook_opds::to_chapbook_error)?,
                    };
                    let resume = comic.resume_page().unwrap_or(0);
                    (OpenBook::Comic(Arc::new(comic)), None, resume, None, true)
                }
            }

            // A source with no file behind it: bytes a host already holds,
            // or a handle it resolved from a `content://` URI or a
            // security-scoped bookmark. No path — but the library keys
            // identity by edition fingerprint, not by file, so the bytes
            // are hashed on the way in and the book is *adopted*: a
            // record, a position, annotations, no copy. Custody of the
            // file — the bookmark that reaches it again — stays with the
            // shell; see `docs/PLATFORM.md`.
            Source::Bytes { format, bytes } => {
                #[cfg(feature = "library")]
                let fingerprint = library
                    .is_some()
                    .then(|| chapbook_library::Library::fingerprint_of_bytes(&bytes));
                let format = resolve_format(format, &bytes)?;
                let book = book_from_bytes(format, bytes)?;

                #[cfg(not(feature = "library"))]
                let (book_id, start_spine, pending_offset, same_edition) = (None, 0, None, true);
                #[cfg(feature = "library")]
                let (book_id, same_edition) = match (library.as_mut(), &fingerprint) {
                    (Some(lib), Some(fp)) => shelve(lib, &book, fp, None),
                    _ => (None, true),
                };
                #[cfg(feature = "library")]
                let (start_spine, pending_offset) =
                    restored_start(&library, &book, book_id, same_edition);
                (book, book_id, start_spine, pending_offset, same_edition)
            }
            Source::Reader { format, mut reader } => {
                // Hashed before it is opened, while the stream is still
                // ours to rewind.
                #[cfg(feature = "library")]
                let fingerprint = match library.is_some() {
                    true => chapbook_library::Library::fingerprint_of_reader(reader.as_mut()).ok(),
                    false => None,
                };
                let format = resolve_format(format, &peek(reader.as_mut())?)?;
                let book = book_from_reader(format, reader)?;

                #[cfg(not(feature = "library"))]
                let (book_id, start_spine, pending_offset, same_edition) = (None, 0, None, true);
                #[cfg(feature = "library")]
                let (book_id, same_edition) = match (library.as_mut(), &fingerprint) {
                    (Some(lib), Some(fp)) => shelve(lib, &book, fp, None),
                    _ => (None, true),
                };
                #[cfg(feature = "library")]
                let (start_spine, pending_offset) =
                    restored_start(&library, &book, book_id, same_edition);
                (book, book_id, start_spine, pending_offset, same_edition)
            }

            Source::Path(ref source_path) => {
                let path = source_path.as_path();
                let book = book_at_path(format_of_path(path), path)?;

                // Matching a book into the library, restoring where the
                // reader was, and importing it if it is new: all of it is
                // the library's, and without one the book simply opens at
                // the beginning.
                #[cfg(not(feature = "library"))]
                let (book_id, start_spine, pending_offset, same_edition) = (None, 0, None, true);

                #[cfg(feature = "library")]
                let (book_id, same_edition) = match library.as_mut() {
                    Some(lib) => match chapbook_library::Library::fingerprint_of_file(path) {
                        Ok(fp) => shelve(lib, &book, &fp, Some(path)),
                        Err(_) => (None, true),
                    },
                    None => (None, true),
                };
                #[cfg(feature = "library")]
                let (start_spine, pending_offset) =
                    restored_start(&library, &book, book_id, same_edition);
                (book, book_id, start_spine, pending_offset, same_edition)
            }
        };

        // Highlights load with the book; endpoints resolve lazily, per
        // unit, once that unit's locator text is available.
        #[cfg(feature = "library")]
        let stored = match (&library, book_id) {
            (Some(lib), Some(id)) => lib
                .annotations(id)
                .map_err(|e| log::warn!("failed to read annotations: {e}"))
                .unwrap_or_default()
                .into_iter()
                .map(|a| {
                    let by_href = book
                        .publication()
                        .spine()
                        .iter()
                        .position(|s: &SpineItem| s.href == a.start.spine_href);
                    let len = book.publication().spine().len();
                    StoredAnnotation {
                        id: a.id,
                        kind: a.kind,
                        target: by_href
                            .unwrap_or(a.start.spine_index)
                            .min(len.saturating_sub(1)),
                        href_matched: by_href.is_some(),
                        start: a.start,
                        end: a.end,
                        text: a.text,
                        color: a.color,
                    }
                })
                .collect(),
            _ => Vec::new(),
        };

        let title = book
            .publication()
            .metadata()
            .title
            .clone()
            .unwrap_or_else(|| "chapbook".to_string());
        let waker: WakerCell = Arc::new(Mutex::new(None));
        #[cfg(feature = "_image-book")]
        let loader = book.load_source().map(|source| {
            let cell = waker.clone();
            Loader::spawn(
                source,
                Arc::new(move || {
                    if let Some(wake) = &*cell.lock().unwrap() {
                        wake();
                    }
                }),
            )
        });
        // The book's override if it has one, else the reader's default,
        // else the built-in defaults. Without a library there is nowhere
        // for an override to have been stored, so the defaults it is.
        #[cfg(feature = "library")]
        let settings = library
            .as_ref()
            .map(|lib| lib.effective_settings(book_id))
            .unwrap_or_default();
        #[cfg(not(feature = "library"))]
        let settings = ReadingSettings::default();

        let (fonts, font_report) = chapbook_layout::build_font_system(&fonts)?;

        Ok(Session {
            book,
            title,
            fonts,
            font_report,
            renderer: chapbook_render_tinyskia::Renderer::new(),
            settings,
            metrics: None,
            view: None,
            units: HashMap::new(),
            cache_budget: cache_budget.unwrap_or(DEFAULT_CACHE_BUDGET),
            use_clock: 0,
            registered_fonts: HashSet::new(),
            spine: start_spine,
            page: 0,
            pending_offset: pending_offset.map(|offset| (start_spine, offset)),
            #[cfg(feature = "_image-book")]
            loader,
            #[cfg(feature = "_image-book")]
            load_errors: HashMap::new(),
            waker,
            events: Vec::new(),
            // Seeded with where the book actually opens, so the first
            // drain reports a move only if one happened. A restored
            // position resolves later, in `frame`, and is a real move.
            reported_position: crate::Position {
                spine: start_spine,
                page: 0,
            },
            reported_finished: false,
            selection: None,
            #[cfg(feature = "library")]
            library,
            #[cfg(feature = "library")]
            library_dir,
            #[cfg(feature = "library")]
            suspended: false,
            #[cfg(feature = "library")]
            book_id,
            #[cfg(feature = "library")]
            stored,
            #[cfg(feature = "library")]
            same_edition,
            empty_images: ImageStore::default(),
            pending: FrameIntent::default(),
            pending_damage: PendingDamage::default(),
            painted_selection: None,
            pixel_format: PixelFormat::default(),
            back_stack: Vec::new(),
            pending_anchor: None,
            #[cfg(feature = "library")]
            char_counts: std::cell::OnceCell::new(),
            unit_text_cache: std::cell::RefCell::new(None),
        })
    }
}
