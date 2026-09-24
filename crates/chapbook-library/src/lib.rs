//! Local bookshelf persistence: imported books and their metadata, reading
//! positions (full [`LayeredLocator`] records), bookmarks/highlights/notes,
//! and saved OPDS sources. SQLite via rusqlite (bundled, WAL), with
//! `user_version`-pragma migrations.
//!
//! Book identity ≠ file identity: books are keyed by library id, the file
//! hash is an *edition fingerprint*. A changed fingerprint routes position
//! restore through the re-anchor chain in [`restore_position`] — state is
//! re-anchored, never orphaned.

mod db;
mod restore;
mod shelf;

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};
use sha1::{Digest, Sha1};

use chapbook_core::{
    ChapbookError, LayeredLocator, Publication, Quote, ReadingSettings, Result, Theme,
};

use crate::db::db_err;
pub use crate::restore::{restore_position, RestoreTier};
pub use crate::shelf::{BookQuery, Collection, CollectionId, CollectionRef, ReadingState, Sort};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BookId(pub i64);

/// One book as a shelf needs it.
///
/// The last four fields exist because a browsing UI asked for them and had
/// nowhere to look: it wanted a cover to draw, a progress bar, and an order
/// that puts what you were reading first. Reading position lived one query
/// away per book, which is fine for `lib ls` over a handful and quadratic
/// for a shelf.
#[derive(Debug, Clone)]
pub struct BookRecord {
    pub id: BookId,
    pub title: String,
    pub authors: Vec<String>,
    pub language: Option<String>,
    pub identifier: Option<String>,
    /// The library-managed copy of the file.
    pub file_path: PathBuf,
    pub fingerprint: String,
    pub added_at: i64,
    /// Cover image on disk, captured at import. `None` if the book had
    /// none — not every CBZ or PDF does.
    pub cover_path: Option<PathBuf>,
    /// The series the book claims, and where in it. Off the publication's
    /// own metadata at import, so `None` is both "no series" and "a book
    /// whose file never said" — which the shelf treats the same way.
    pub series: Option<String>,
    pub series_index: Option<f64>,
    /// When the reader reached the end, if they have. See
    /// [`Self::state`].
    pub finished_at: Option<i64>,
    /// When the position was last written: "recently read", and `None` for
    /// a book that has never been opened.
    pub last_read: Option<i64>,
    /// How far through, 0.0..=1.0, from the stored position.
    pub progress: Option<f64>,
    /// The collections this book is in, by the time a shelf sees it.
    /// Attached in one query per page of results, not one per book.
    pub collections: Vec<CollectionRef>,
}

impl BookRecord {
    /// Which of the three piles a shelf puts this book in.
    ///
    /// Only `Finished` is stored; the other two are the presence or
    /// absence of a reading position, which is already here.
    pub fn state(&self) -> ReadingState {
        match (self.finished_at, self.last_read) {
            (Some(_), _) => ReadingState::Finished,
            (None, Some(_)) => ReadingState::Reading,
            (None, None) => ReadingState::Unread,
        }
    }
}

#[derive(Debug, Clone)]
pub struct StoredPosition {
    pub locator: LayeredLocator,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotationKind {
    Bookmark,
    Highlight,
    Note,
}

impl AnnotationKind {
    fn as_str(&self) -> &'static str {
        match self {
            AnnotationKind::Bookmark => "bookmark",
            AnnotationKind::Highlight => "highlight",
            AnnotationKind::Note => "note",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "highlight" => AnnotationKind::Highlight,
            "note" => AnnotationKind::Note,
            _ => AnnotationKind::Bookmark,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Annotation {
    pub id: i64,
    pub kind: AnnotationKind,
    pub start: LayeredLocator,
    /// Range end for highlights; bookmarks are points.
    pub end: Option<LayeredLocator>,
    pub text: Option<String>,
    pub color: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// A catalog the library knows about. Deliberately holds no secret: the
/// password or token for this source lives in the host's credential store,
/// keyed by `chapbook_core::CredentialKey::opds_source(id)`.
#[derive(Debug, Clone)]
pub struct OpdsSource {
    pub id: i64,
    /// Opaque, possibly secret-bearing — never log or normalize.
    pub url: String,
    pub title: Option<String>,
    /// Account label for display. Not a secret and not sent anywhere; the
    /// credential that goes with it is in the credential store.
    pub auth_user: Option<String>,
}

/// Where one book's position and marks sync to.
///
/// Both URLs are per-*publication*, not per-catalog: in OPDS Progression
/// and in the Web Annotation Protocol alike, the URL is the publication's
/// identity, so they are discovered from a catalog entry's links and
/// belong to the book rather than to the source it came from. `None` is
/// the ordinary case — a sideloaded book has no service to talk to.
///
/// The URLs are opaque and may embed a per-user key. Never log them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SyncTargets {
    pub progression_url: Option<String>,
    pub annotation_container: Option<String>,
    /// The `modified` of the progression document last seen from the
    /// service, verbatim. Compared for equality, never parsed — see the
    /// v6 migration for why.
    pub remote_modified: Option<String>,
    /// The position `revision` that last agreed with the service.
    pub position_synced_revision: Option<i64>,
}

/// One annotation's remote half, for a caller reconciling with a
/// container.
#[derive(Debug, Clone, PartialEq)]
pub struct AnnotationSync {
    /// The local row.
    pub id: i64,
    /// The container's IRI. `None` means never pushed.
    pub remote_iri: Option<String>,
    /// The tag to send as `If-Match`.
    pub remote_etag: Option<String>,
    /// A soft-deleted row: the server owes a DELETE, not a PUT.
    pub deleted: bool,
    /// The revision being pushed. Hand it back to
    /// [`Library::mark_annotation_synced`] so an edit made while the
    /// request was in flight stays dirty instead of being marked clean.
    pub revision: i64,
}

/// A book whose stored position owes its service a write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PositionPush {
    pub book: BookId,
    /// The service to push to — non-null by construction here.
    pub progression_url: String,
    /// The revision being pushed; hand it back to
    /// [`Library::mark_position_synced`].
    pub revision: i64,
}

pub struct Library {
    conn: Connection,
    books_dir: PathBuf,
    covers_dir: PathBuf,
}

impl Library {
    /// Open (creating if needed) the library at `dir`.
    pub fn open(dir: &Path) -> Result<Self> {
        let books_dir = dir.join("books");
        let covers_dir = dir.join("covers");
        std::fs::create_dir_all(&books_dir)?;
        std::fs::create_dir_all(&covers_dir)?;
        let conn = db::open_and_migrate(&dir.join("chapbook.db"))?;
        Ok(Library {
            conn,
            books_dir,
            covers_dir,
        })
    }

    /// Where this platform keeps a per-user library.
    ///
    /// `$CHAPBOOK_LIBRARY_DIR` wins everywhere — tests, packagers and
    /// anyone with two libraries need one override that does not vary by
    /// target. Failing that, each desktop platform gets its own
    /// convention, because the previous single answer was the Linux one
    /// and only Linux was right:
    ///
    /// | | |
    /// |---|---|
    /// | Linux, BSD | `$XDG_DATA_HOME/chapbook`, else `~/.local/share/chapbook` |
    /// | macOS | `~/Library/Application Support/chapbook` |
    /// | Windows | `%APPDATA%\chapbook`, else `%LOCALAPPDATA%\chapbook` |
    ///
    /// **Everything else is an error, deliberately.** Android, iOS and
    /// wasm have no such variables, and the old code fell through to a
    /// relative `"."` — writing a library into whatever directory the
    /// process happened to start in, which is silent, wrong, and very hard
    /// to diagnose. A platform with a sandbox knows its own answer and
    /// should say it, which is what `SessionConfig::with_library_dir` is
    /// for. The same applies to a Unix daemon with no `HOME`.
    pub fn default_dir() -> Result<PathBuf> {
        // An empty variable is a mistake, not a request for the CWD.
        if let Some(dir) = std::env::var_os("CHAPBOOK_LIBRARY_DIR")
            .map(PathBuf::from)
            .filter(|dir| !dir.as_os_str().is_empty())
        {
            return Ok(dir);
        }
        platform_dir()
            .map(|dir| dir.join("chapbook"))
            .ok_or_else(|| {
                ChapbookError::Library(
                    "no default library location on this platform; set \
                 CHAPBOOK_LIBRARY_DIR or pass SessionConfig::with_library_dir"
                        .into(),
                )
            })
    }

    /// Import a book: copy the file into the library, index its metadata,
    /// and keep its cover. Re-importing a file with a known fingerprint
    /// returns the existing book instead of duplicating it.
    ///
    /// Takes the whole publication rather than just its metadata, which is
    /// what it used to take. The reason is the cover: every `Publication`
    /// can produce one and nothing was asking, so a shelf had no image to
    /// draw and would have had to reopen every book to get one.
    pub fn import(&mut self, source: &Path, publication: &dyn Publication) -> Result<BookId> {
        let fingerprint = Self::fingerprint_of_file(source)?;
        let fallback_title = source
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        self.record(
            publication,
            &fingerprint,
            &fallback_title,
            &source.to_string_lossy(),
            Some(source),
        )
    }

    /// Record a book the library holds no copy of.
    ///
    /// The custody answer for a book that arrives as a descriptor or as
    /// bytes — an Android `content://` resolve, an iOS security-scoped
    /// bookmark: the platform owns the file and the shell owns the means
    /// of reaching it again, so the library keeps a *record* — identity,
    /// metadata, cover, and everything positions and annotations key on —
    /// and copies nothing. `file_path` stays empty, which the schema has
    /// always allowed and [`BookRecord`] callers already tolerate.
    ///
    /// The caller supplies the fingerprint because only the caller still
    /// has the stream — see [`Self::fingerprint_of_reader`]. Adopting a
    /// fingerprint the shelf already knows returns the existing book, so a
    /// book imported by path on one platform and opened by descriptor on
    /// another resolves to the same record.
    pub fn adopt(&mut self, fingerprint: &str, publication: &dyn Publication) -> Result<BookId> {
        self.record(publication, fingerprint, "Untitled", "", None)
    }

    fn record(
        &mut self,
        publication: &dyn Publication,
        fingerprint: &str,
        fallback_title: &str,
        source_label: &str,
        copy_from: Option<&Path>,
    ) -> Result<BookId> {
        let metadata = publication.metadata();

        // Deliberately not filtered by `deleted`. [`Self::delete_book`]
        // is soft so that a book removed and added back finds its own
        // annotations again — and it did not, because this lookup could
        // not see the row holding them and made a second one with the
        // same fingerprint. Re-adding a removed book un-removes it.
        if let Some((existing, deleted)) = self
            .conn
            .query_row(
                "SELECT id, deleted FROM books WHERE fingerprint = ?1",
                params![fingerprint],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)? != 0)),
            )
            .optional()
            .map_err(db_err)?
        {
            if deleted {
                self.conn
                    .execute(
                        "UPDATE books SET deleted = 0 WHERE id = ?1",
                        params![existing],
                    )
                    .map_err(db_err)?;
            }
            return Ok(BookId(existing));
        }

        let title = metadata
            .title
            .clone()
            .unwrap_or_else(|| fallback_title.to_owned());
        let tx = self.conn.transaction().map_err(db_err)?;
        tx.execute(
            "INSERT INTO books (title, language, identifier, file_path, source_path,
                    fingerprint, series, series_index, added_at)
             VALUES (?1, ?2, ?3, '', ?4, ?5, ?6, ?7, strftime('%s','now'))",
            params![
                title,
                metadata.language,
                metadata.identifier,
                source_label,
                fingerprint,
                metadata.series,
                metadata.series_index,
            ],
        )
        .map_err(db_err)?;
        let id = tx.last_insert_rowid();

        for (i, author) in metadata.authors.iter().enumerate() {
            tx.execute(
                "INSERT OR IGNORE INTO authors (name) VALUES (?1)",
                params![author],
            )
            .map_err(db_err)?;
            tx.execute(
                "INSERT INTO book_authors (book_id, author_id, position)
                 SELECT ?1, id, ?2 FROM authors WHERE name = ?3",
                params![id, i as i64, author],
            )
            .map_err(db_err)?;
        }

        if let Some(source) = copy_from {
            // Managed copy named by id; extension preserved for format
            // sniffing.
            let extension = source
                .extension()
                .map(|e| e.to_string_lossy().into_owned())
                .unwrap_or_else(|| "epub".into());
            let file_path = self.books_dir.join(format!("{id}.{extension}"));
            std::fs::copy(source, &file_path)?;
            tx.execute(
                "UPDATE books SET file_path = ?1 WHERE id = ?2",
                params![file_path.to_string_lossy(), id],
            )
            .map_err(db_err)?;
        }

        // A missing or unwritable cover is not an import failure: the book
        // is fine and a shelf can fall back to a title card.
        let cover_path = match publication.cover() {
            Ok(Some(cover)) => {
                let name = format!("{id}.{}", cover_extension(&cover.media_type));
                let path = self.covers_dir.join(name);
                std::fs::write(&path, &cover.data)
                    .map_err(|e| log::warn!("keeping no cover for #{id}: {e}"))
                    .ok()
                    .map(|()| path)
            }
            Ok(None) => None,
            Err(e) => {
                log::warn!("keeping no cover for #{id}: {e}");
                None
            }
        };
        if let Some(path) = &cover_path {
            tx.execute(
                "UPDATE books SET cover_path = ?1 WHERE id = ?2",
                params![path.to_string_lossy(), id],
            )
            .map_err(db_err)?;
        }

        // The search index, in the same transaction as the row it
        // indexes: a book that exists but cannot be found is worse than a
        // failed import, and this is the only write site.
        tx.execute(
            "INSERT INTO book_search (rowid, title, authors, series)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                id,
                title,
                metadata.authors.join(" "),
                metadata.series.clone().unwrap_or_default(),
            ],
        )
        .map_err(db_err)?;

        tx.commit().map_err(db_err)?;
        Ok(BookId(id))
    }

    /// All (non-deleted) books, optionally narrowed by a search over
    /// title, authors and series, newest first.
    ///
    /// The two named shapes over [`Library::query`], which is where
    /// anything more specific goes.
    ///
    /// Note that `filter` is a *search*, not a substring: it matches whole
    /// words with a prefix, so "brid" finds *The Bridge* and "ridge" no
    /// longer does. That is the trade for matching "bronte" against
    /// Brontë, which the substring version could not do — see the v7
    /// migration.
    pub fn books(&self, filter: Option<&str>) -> Result<Vec<BookRecord>> {
        self.query(&BookQuery {
            search: filter,
            ..Default::default()
        })
    }

    /// Books in the order a shelf wants them: what you were reading last,
    /// then what you added last for anything never opened.
    pub fn recent(&self, limit: Option<usize>) -> Result<Vec<BookRecord>> {
        self.query(&BookQuery {
            sort: Sort::Read,
            limit,
            ..Default::default()
        })
    }

    fn authors_of(&self, id: BookId) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT a.name FROM authors a
                 JOIN book_authors ba ON ba.author_id = a.id
                 WHERE ba.book_id = ?1 ORDER BY ba.position",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![id.0], |row| row.get::<_, String>(0))
            .map_err(db_err)?;
        rows.collect::<rusqlite::Result<_>>().map_err(db_err)
    }

    /// Remove a book from the shelf.
    ///
    /// Soft: the row keeps its id and its annotations, so a re-import of
    /// the same file finds its highlights again. The `deleted` column has
    /// been in the schema since v1 with nothing to set it — a shelf with no
    /// way to remove a book is the thing that noticed.
    ///
    /// **Local, and deliberately so.** Nothing here reaches a service. The
    /// marks this book has already pushed stay in their container, because
    /// removing a book from *this* shelf is not a statement about the
    /// reader's other devices — deleting the remote copies would destroy
    /// highlights everywhere over a tidy-up here. Deleting an individual
    /// annotation is the action that propagates; this is not.
    ///
    /// What it does do is *freeze* the book's remote half: a removed book
    /// is offered by neither `books_with_sync_targets` nor
    /// `positions_needing_push`, and `SyncEngine::sync_book` refuses one.
    /// Anything it still owed a service — a position, a mark, or the
    /// delete of a mark — stays owed rather than being sent or discarded,
    /// and resumes if the same file is imported again, which restores the
    /// row and its sync targets together. The one lasting consequence is a
    /// mark deleted here and never flushed: its container keeps it until
    /// the book comes back.
    pub fn delete_book(&mut self, id: BookId) -> Result<()> {
        self.conn
            .execute("UPDATE books SET deleted = 1 WHERE id = ?1", params![id.0])
            .map_err(db_err)?;
        Ok(())
    }

    /// Find a book by its publication identifier — the cross-edition match:
    /// a re-downloaded edition has a new fingerprint but usually the same
    /// dc:identifier.
    pub fn find_by_identifier(&self, identifier: &str) -> Result<Option<BookId>> {
        self.conn
            .query_row(
                "SELECT id FROM books WHERE identifier = ?1 AND deleted = 0
                 ORDER BY added_at DESC LIMIT 1",
                params![identifier],
                |row| row.get::<_, i64>(0).map(BookId),
            )
            .optional()
            .map_err(db_err)
    }

    /// Adopt a new edition of an existing book: replace the managed copy and
    /// fingerprint. Positions/annotations stay keyed to the book id and get
    /// re-anchored by [`restore_position`], never orphaned.
    pub fn update_edition(&mut self, id: BookId, source: &Path) -> Result<()> {
        let fingerprint = Self::fingerprint_of_file(source)?;
        let file_path: String = self
            .conn
            .query_row(
                "SELECT file_path FROM books WHERE id = ?1",
                params![id.0],
                |row| row.get(0),
            )
            .map_err(db_err)?;
        // An adopted book has no managed copy to refresh.
        if !file_path.is_empty() {
            std::fs::copy(source, &file_path)?;
        }
        self.conn
            .execute(
                "UPDATE books SET fingerprint = ?1, source_path = ?2 WHERE id = ?3",
                params![fingerprint, source.to_string_lossy(), id.0],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// [`Self::update_edition`] for a book with no path: a new edition
    /// arrived through a handle, so there are no bytes to copy — only the
    /// identity to move, so the next open matches by fingerprint instead
    /// of re-anchoring through the identifier every time. A managed copy,
    /// if the book was once imported by path, stays on the previous
    /// edition; the platform owns the current one.
    pub fn update_edition_fingerprint(&mut self, id: BookId, fingerprint: &str) -> Result<()> {
        self.conn
            .execute(
                "UPDATE books SET fingerprint = ?1 WHERE id = ?2",
                params![fingerprint, id.0],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// One book record by id.
    pub fn book(&self, id: BookId) -> Result<Option<BookRecord>> {
        let sql = format!(
            "SELECT {}
             FROM books b
             LEFT JOIN positions p ON p.book_id = b.id
             WHERE b.id = ?1 AND b.deleted = 0",
            crate::shelf::RECORD_COLUMNS
        );
        let record = self
            .conn
            .query_row(&sql, params![id.0], crate::shelf::record_from_row)
            .optional()
            .map_err(db_err)?;
        match record {
            Some(mut record) => {
                record.authors = self.authors_of(record.id)?;
                record.collections = self.collections_of(record.id)?;
                Ok(Some(record))
            }
            None => Ok(None),
        }
    }

    /// Find a book by its edition fingerprint (SHA-1 hex of the file bytes).
    pub fn find_by_fingerprint(&self, fingerprint: &str) -> Result<Option<BookId>> {
        self.conn
            .query_row(
                "SELECT id FROM books WHERE fingerprint = ?1 AND deleted = 0",
                params![fingerprint],
                |row| row.get::<_, i64>(0).map(BookId),
            )
            .optional()
            .map_err(db_err)
    }

    pub fn fingerprint_of_file(path: &Path) -> Result<String> {
        Ok(Self::fingerprint_stream(std::fs::File::open(path)?)?)
    }

    /// Fingerprint a seekable handle, rewound to the start on both sides
    /// of the hash so the format reader gets an untouched stream.
    ///
    /// This is how a descriptor-opened book gets the same edition identity
    /// a path import gets, which is what lets it reach the shelf at all —
    /// see [`Self::adopt`].
    pub fn fingerprint_of_reader(reader: &mut dyn chapbook_core::ReadSeek) -> Result<String> {
        reader.seek(std::io::SeekFrom::Start(0))?;
        let fingerprint = Self::fingerprint_stream(&mut *reader)?;
        reader.seek(std::io::SeekFrom::Start(0))?;
        Ok(fingerprint)
    }

    /// Fingerprint bytes a host already holds whole.
    pub fn fingerprint_of_bytes(bytes: &[u8]) -> String {
        hex(&Sha1::digest(bytes))
    }

    /// SHA-1 of a stream, a buffer at a time. The whole-file
    /// `std::fs::read` this replaced was a transient allocation the size
    /// of the book — for a large comic, a real spike against a phone's
    /// memory ceiling, spent on a hash that streams.
    fn fingerprint_stream(mut reader: impl std::io::Read) -> std::io::Result<String> {
        let mut hasher = Sha1::new();
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        Ok(hex(&hasher.finalize()))
    }

    pub fn position(&self, id: BookId) -> Result<Option<StoredPosition>> {
        self.conn
            .query_row(
                "SELECT spine_href, spine_index, char_offset, locator_version,
                        quote_prefix, quote_exact, quote_suffix,
                        spine_fraction, book_progression, updated_at
                 FROM positions WHERE book_id = ?1",
                params![id.0],
                |row| {
                    Ok(StoredPosition {
                        locator: LayeredLocator {
                            spine_href: row.get(0)?,
                            spine_index: row.get::<_, i64>(1)? as usize,
                            char_offset: row.get::<_, i64>(2)? as u32,
                            locator_version: row.get::<_, i64>(3)? as u32,
                            quote: Quote {
                                prefix: row.get(4)?,
                                exact: row.get(5)?,
                                suffix: row.get(6)?,
                            },
                            spine_fraction: row.get(7)?,
                            book_progression: row.get(8)?,
                        },
                        updated_at: row.get(9)?,
                    })
                },
            )
            .optional()
            .map_err(db_err)
    }

    pub fn set_position(&mut self, id: BookId, locator: &LayeredLocator) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO positions (book_id, spine_href, spine_index, char_offset,
                        locator_version, quote_prefix, quote_exact, quote_suffix,
                        spine_fraction, book_progression, updated_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10, strftime('%s','now'))
                 ON CONFLICT(book_id) DO UPDATE SET
                        spine_href=?2, spine_index=?3, char_offset=?4,
                        locator_version=?5, quote_prefix=?6, quote_exact=?7,
                        quote_suffix=?8, spine_fraction=?9, book_progression=?10,
                        updated_at=strftime('%s','now'),
                        revision=positions.revision + 1",
                params![
                    id.0,
                    locator.spine_href,
                    locator.spine_index as i64,
                    locator.char_offset as i64,
                    locator.locator_version as i64,
                    locator.quote.prefix,
                    locator.quote.exact,
                    locator.quote.suffix,
                    locator.spine_fraction,
                    locator.book_progression,
                ],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn add_annotation(
        &mut self,
        id: BookId,
        kind: AnnotationKind,
        start: &LayeredLocator,
        end: Option<&LayeredLocator>,
        text: Option<&str>,
        color: Option<&str>,
    ) -> Result<i64> {
        self.conn
            .execute(
                "INSERT INTO annotations (book_id, kind,
                    start_spine_href, start_spine_index, start_char_offset,
                    start_locator_version, start_quote_prefix, start_quote_exact,
                    start_quote_suffix, start_spine_fraction, start_book_progression,
                    end_spine_href, end_spine_index, end_char_offset,
                    end_locator_version, end_quote_prefix, end_quote_exact,
                    end_quote_suffix, end_spine_fraction, end_book_progression,
                    note_text, color, created_at, updated_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,
                         ?12,?13,?14,?15,?16,?17,?18,?19,?20,
                         ?21,?22, strftime('%s','now'), strftime('%s','now'))",
                params![
                    id.0,
                    kind.as_str(),
                    start.spine_href,
                    start.spine_index as i64,
                    start.char_offset as i64,
                    start.locator_version as i64,
                    start.quote.prefix,
                    start.quote.exact,
                    start.quote.suffix,
                    start.spine_fraction,
                    start.book_progression,
                    end.map(|e| e.spine_href.clone()),
                    end.map(|e| e.spine_index as i64),
                    end.map(|e| e.char_offset as i64),
                    end.map(|e| e.locator_version as i64),
                    end.map(|e| e.quote.prefix.clone()),
                    end.map(|e| e.quote.exact.clone()),
                    end.map(|e| e.quote.suffix.clone()),
                    end.map(|e| e.spine_fraction),
                    end.map(|e| e.book_progression),
                    text,
                    color,
                ],
            )
            .map_err(db_err)?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Replace an annotation's content with what another device wrote.
    ///
    /// The adopting half of sync, and every field a mark carries, because
    /// a peer may have moved the anchor as well as the words. `created_at`
    /// is left alone: the mark was made once, by whoever made it, and this
    /// is the same mark saying something else.
    ///
    /// Bumps `revision` as any other edit does, which leaves the row owing
    /// a write it does not owe — a change adopted *from* the container is
    /// already there. Follow with [`Self::mark_annotation_synced`] at the
    /// revision this produced.
    pub fn update_annotation(
        &mut self,
        annotation_id: i64,
        kind: AnnotationKind,
        start: &LayeredLocator,
        end: Option<&LayeredLocator>,
        text: Option<&str>,
        color: Option<&str>,
    ) -> Result<()> {
        self.conn
            .execute(
                "UPDATE annotations SET kind = ?2,
                    start_spine_href = ?3, start_spine_index = ?4, start_char_offset = ?5,
                    start_locator_version = ?6, start_quote_prefix = ?7, start_quote_exact = ?8,
                    start_quote_suffix = ?9, start_spine_fraction = ?10,
                    start_book_progression = ?11,
                    end_spine_href = ?12, end_spine_index = ?13, end_char_offset = ?14,
                    end_locator_version = ?15, end_quote_prefix = ?16, end_quote_exact = ?17,
                    end_quote_suffix = ?18, end_spine_fraction = ?19,
                    end_book_progression = ?20,
                    note_text = ?21, color = ?22,
                    updated_at = strftime('%s','now'), revision = revision + 1
                 WHERE id = ?1",
                params![
                    annotation_id,
                    kind.as_str(),
                    start.spine_href,
                    start.spine_index as i64,
                    start.char_offset as i64,
                    start.locator_version as i64,
                    start.quote.prefix,
                    start.quote.exact,
                    start.quote.suffix,
                    start.spine_fraction,
                    start.book_progression,
                    end.map(|e| e.spine_href.clone()),
                    end.map(|e| e.spine_index as i64),
                    end.map(|e| e.char_offset as i64),
                    end.map(|e| e.locator_version as i64),
                    end.map(|e| e.quote.prefix.clone()),
                    end.map(|e| e.quote.exact.clone()),
                    end.map(|e| e.quote.suffix.clone()),
                    end.map(|e| e.spine_fraction),
                    end.map(|e| e.book_progression),
                    text,
                    color,
                ],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn annotations(&self, id: BookId) -> Result<Vec<Annotation>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, kind,
                        start_spine_href, start_spine_index, start_char_offset,
                        start_locator_version, start_quote_prefix, start_quote_exact,
                        start_quote_suffix, start_spine_fraction, start_book_progression,
                        end_spine_href, end_spine_index, end_char_offset,
                        end_locator_version, end_quote_prefix, end_quote_exact,
                        end_quote_suffix, end_spine_fraction, end_book_progression,
                        note_text, color, created_at, updated_at
                 FROM annotations
                 WHERE book_id = ?1 AND deleted = 0
                 ORDER BY start_book_progression, id",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![id.0], |row| {
                let start = LayeredLocator {
                    spine_href: row.get(2)?,
                    spine_index: row.get::<_, i64>(3)? as usize,
                    char_offset: row.get::<_, i64>(4)? as u32,
                    locator_version: row.get::<_, i64>(5)? as u32,
                    quote: Quote {
                        prefix: row.get(6)?,
                        exact: row.get(7)?,
                        suffix: row.get(8)?,
                    },
                    spine_fraction: row.get(9)?,
                    book_progression: row.get(10)?,
                };
                let end = match row.get::<_, Option<String>>(11)? {
                    Some(href) => Some(LayeredLocator {
                        spine_href: href,
                        spine_index: row.get::<_, i64>(12)? as usize,
                        char_offset: row.get::<_, i64>(13)? as u32,
                        locator_version: row.get::<_, i64>(14)? as u32,
                        quote: Quote {
                            prefix: row.get(15)?,
                            exact: row.get(16)?,
                            suffix: row.get(17)?,
                        },
                        spine_fraction: row.get(18)?,
                        book_progression: row.get(19)?,
                    }),
                    None => None,
                };
                Ok(Annotation {
                    id: row.get(0)?,
                    kind: AnnotationKind::parse(&row.get::<_, String>(1)?),
                    start,
                    end,
                    text: row.get(20)?,
                    color: row.get(21)?,
                    created_at: row.get(22)?,
                    updated_at: row.get(23)?,
                })
            })
            .map_err(db_err)?;
        rows.collect::<rusqlite::Result<_>>().map_err(db_err)
    }

    /// Every live mark on this book the container has a copy of, as
    /// `(id, IRI)`.
    ///
    /// The set a pull subtracts what it saw from. An IRI here and missing
    /// from a *complete* listing is a mark another device deleted; missing
    /// from a listing that stopped early is no evidence of anything, which
    /// is why the completeness has to travel with the listing.
    pub fn synced_annotations(&self, id: BookId) -> Result<Vec<(i64, String)>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, remote_iri
                 FROM annotations
                 WHERE book_id = ?1 AND deleted = 0 AND remote_iri IS NOT NULL
                 ORDER BY id",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![id.0], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(db_err)?;
        rows.collect::<rusqlite::Result<_>>().map_err(db_err)
    }

    /// Soft-delete an annotation (kept for future sync; never hard-deleted).
    /// Recolor an annotation. `None` hands it back to the reader's theme
    /// color.
    pub fn set_annotation_color(&mut self, annotation_id: i64, color: Option<&str>) -> Result<()> {
        self.conn
            .execute(
                "UPDATE annotations SET color = ?2, updated_at = strftime('%s','now'), revision = revision + 1
                 WHERE id = ?1",
                params![annotation_id, color],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn delete_annotation(&mut self, annotation_id: i64) -> Result<()> {
        self.conn
            .execute(
                "UPDATE annotations SET deleted = 1, updated_at = strftime('%s','now'), revision = revision + 1
                 WHERE id = ?1",
                params![annotation_id],
            )
            .map_err(db_err)?;
        Ok(())
    }

    // ---- Reading settings ----

    /// Settings for a scope: `None` is the reader's default, `Some(id)` is
    /// that book's override. `Ok(None)` when the scope has never been set.
    pub fn reading_settings(&self, book: Option<BookId>) -> Result<Option<ReadingSettings>> {
        let scope = book.map_or(0, |b| b.0);
        self.conn
            .query_row(
                "SELECT base_font_px, line_height, justify, publisher_styles, theme,
                        font_family
                 FROM reading_settings WHERE book_id = ?1",
                params![scope],
                |row| {
                    Ok(ReadingSettings {
                        base_font_px: row.get::<_, f64>(0)? as f32,
                        line_height: row.get::<_, f64>(1)? as f32,
                        justify: row.get::<_, i64>(2)? != 0,
                        publisher_styles: row.get::<_, i64>(3)? != 0,
                        theme: row
                            .get::<_, String>(4)
                            .map(|name| Theme::from_name(&name).unwrap_or_default())?,
                        // NULL is the publisher's font, which is what every
                        // row written before v5 means.
                        font_family: row.get::<_, Option<String>>(5)?,
                    })
                },
            )
            .optional()
            .map_err(db_err)
    }

    /// The settings a book should open with: its own override, else the
    /// reader's default, else the built-in defaults. Never fails — a
    /// database that can't answer falls back rather than blocking reading.
    pub fn effective_settings(&self, book: Option<BookId>) -> ReadingSettings {
        book.and_then(|id| self.reading_settings(Some(id)).ok().flatten())
            .or_else(|| self.reading_settings(None).ok().flatten())
            .unwrap_or_default()
    }

    pub fn set_reading_settings(
        &mut self,
        book: Option<BookId>,
        settings: &ReadingSettings,
    ) -> Result<()> {
        let scope = book.map_or(0, |b| b.0);
        self.conn
            .execute(
                "INSERT INTO reading_settings
                    (book_id, base_font_px, line_height, justify, publisher_styles,
                     theme, font_family, updated_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7, strftime('%s','now'))
                 ON CONFLICT(book_id) DO UPDATE SET
                    base_font_px = excluded.base_font_px,
                    line_height = excluded.line_height,
                    justify = excluded.justify,
                    publisher_styles = excluded.publisher_styles,
                    theme = excluded.theme,
                    font_family = excluded.font_family,
                    updated_at = excluded.updated_at",
                params![
                    scope,
                    settings.base_font_px as f64,
                    settings.line_height as f64,
                    settings.justify as i64,
                    settings.publisher_styles as i64,
                    settings.theme.name(),
                    settings.font_family.as_deref(),
                ],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// Drop a book's override so it follows the reader's default again.
    pub fn clear_reading_settings(&mut self, book: BookId) -> Result<()> {
        self.conn
            .execute(
                "DELETE FROM reading_settings WHERE book_id = ?1",
                params![book.0],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// Record a catalog. Takes no secret by design — store the credential
    /// under `CredentialKey::opds_source(id)` with the returned id.
    pub fn add_opds_source(
        &mut self,
        url: &str,
        title: Option<&str>,
        auth_user: Option<&str>,
    ) -> Result<i64> {
        self.conn
            .execute(
                "INSERT INTO opds_sources (url, title, auth_user, added_at)
                 VALUES (?1, ?2, ?3, strftime('%s','now'))",
                params![url, title, auth_user],
            )
            .map_err(db_err)?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn opds_sources(&self) -> Result<Vec<OpdsSource>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, url, title, auth_user
                 FROM opds_sources WHERE deleted = 0 ORDER BY id",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok(OpdsSource {
                    id: row.get(0)?,
                    url: row.get(1)?,
                    title: row.get(2)?,
                    auth_user: row.get(3)?,
                })
            })
            .map_err(db_err)?;
        rows.collect::<rusqlite::Result<_>>().map_err(db_err)
    }

    /// One catalog by id, `None` once it has been removed.
    pub fn opds_source(&self, id: i64) -> Result<Option<OpdsSource>> {
        self.conn
            .query_row(
                "SELECT id, url, title, auth_user
                 FROM opds_sources WHERE id = ?1 AND deleted = 0",
                params![id],
                |row| {
                    Ok(OpdsSource {
                        id: row.get(0)?,
                        url: row.get(1)?,
                        title: row.get(2)?,
                        auth_user: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(db_err)
    }

    /// Give a catalog a title — the one the reader typed, or the one its
    /// feed announced once it was fetched. `None` clears it.
    pub fn rename_opds_source(&mut self, id: i64, title: Option<&str>) -> Result<bool> {
        let changed = self
            .conn
            .execute(
                "UPDATE opds_sources SET title = ?2 WHERE id = ?1 AND deleted = 0",
                params![id, title],
            )
            .map_err(db_err)?;
        Ok(changed > 0)
    }

    /// Take a catalog off the list. Soft, like every delete here; the
    /// books that came from it stay, and so do their sync targets — a
    /// book's services are its own, not the catalog's.
    pub fn remove_opds_source(&mut self, id: i64) -> Result<bool> {
        let changed = self
            .conn
            .execute(
                "UPDATE opds_sources SET deleted = 1 WHERE id = ?1 AND deleted = 0",
                params![id],
            )
            .map_err(db_err)?;
        Ok(changed > 0)
    }

    // ---- Grants ----
    //
    // How an adopted book is found again. The library records an adopted
    // book by content and keeps no copy; the token that reopens the
    // platform's file is the platform's own shape — a content URI, a
    // security-scoped bookmark — and opaque here. Keyed by fingerprint,
    // which identifies the file across a reinstall, where the row id only
    // identifies the reader's history of it.

    /// The token that reopens an adopted book, if one was remembered.
    pub fn grant(&self, fingerprint: &str) -> Result<Option<Vec<u8>>> {
        self.conn
            .query_row(
                "SELECT token FROM grants WHERE fingerprint = ?1",
                params![fingerprint],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()
            .map_err(db_err)
    }

    /// Remember how to reach an adopted book. Replaces any earlier token.
    pub fn set_grant(&mut self, fingerprint: &str, token: &[u8]) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO grants (fingerprint, token, updated_at)
                 VALUES (?1, ?2, strftime('%s','now'))
                 ON CONFLICT(fingerprint) DO UPDATE SET
                        token = ?2, updated_at = strftime('%s','now')",
                params![fingerprint, token],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// Forget how to reach an adopted book. Succeeds when nothing was
    /// remembered.
    pub fn clear_grant(&mut self, fingerprint: &str) -> Result<()> {
        self.conn
            .execute(
                "DELETE FROM grants WHERE fingerprint = ?1",
                params![fingerprint],
            )
            .map_err(db_err)?;
        Ok(())
    }

    // ---- Preferences ----
    //
    // The application's own display preferences: what a front end shows,
    // not what the engine lays out. Free-form and never secret. Kept here
    // so a preference is one row for every front end rather than one
    // platform store per app.

    /// A preference's value, if set.
    pub fn preference(&self, key: &str) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT value FROM preferences WHERE key = ?1",
                params![key],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(db_err)
    }

    /// Set a preference, replacing any earlier value.
    pub fn set_preference(&mut self, key: &str, value: &str) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO preferences (key, value, updated_at)
                 VALUES (?1, ?2, strftime('%s','now'))
                 ON CONFLICT(key) DO UPDATE SET
                        value = ?2, updated_at = strftime('%s','now')",
                params![key, value],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// Unset a preference. Succeeds when it was never set.
    pub fn clear_preference(&mut self, key: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM preferences WHERE key = ?1", params![key])
            .map_err(db_err)?;
        Ok(())
    }

    // ---- Sync bookkeeping ----
    //
    // What this deliberately does *not* hold: a schedule. When to push is
    // the shell's decision, not the engine's — every page turn is wrong on
    // a metered radio and on close is wrong for a session that crashes —
    // so what the library owes a caller is a cheap answer to "what still
    // owes the server a write", and nothing about when to ask.
    //
    // Nor a device identity. `opds_client::progression::Device` is
    // host-owned by contract, like a credential: the host mints one and
    // keeps it.

    /// Where this book syncs to, if anywhere.
    pub fn sync_targets(&self, id: BookId) -> Result<SyncTargets> {
        self.conn
            .query_row(
                "SELECT progression_url, annotation_container, remote_modified,
                        position_synced_revision
                 FROM book_sync WHERE book_id = ?1",
                params![id.0],
                |row| {
                    Ok(SyncTargets {
                        progression_url: row.get(0)?,
                        annotation_container: row.get(1)?,
                        remote_modified: row.get(2)?,
                        position_synced_revision: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(db_err)
            .map(Option::unwrap_or_default)
    }

    /// Record where a book syncs, from the links on the catalog entry it
    /// came from. Passing `None` for either clears it — a book that moved
    /// catalogs should stop talking to the old one.
    pub fn set_sync_targets(
        &mut self,
        id: BookId,
        progression_url: Option<&str>,
        annotation_container: Option<&str>,
    ) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO book_sync (book_id, progression_url, annotation_container,
                        updated_at)
                 VALUES (?1, ?2, ?3, strftime('%s','now'))
                 ON CONFLICT(book_id) DO UPDATE SET
                        progression_url = ?2, annotation_container = ?3,
                        updated_at = strftime('%s','now')",
                params![id.0, progression_url, annotation_container],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// Every live book with a service to sync against, in id order.
    ///
    /// What a shell hands to a reconcile loop: books with no catalog
    /// behind them are not in it, which is most of them.
    pub fn books_with_sync_targets(&self) -> Result<Vec<BookId>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT s.book_id
                 FROM book_sync s
                 JOIN books b ON b.id = s.book_id
                 WHERE b.deleted = 0
                   AND (s.progression_url IS NOT NULL
                        OR s.annotation_container IS NOT NULL)
                 ORDER BY s.book_id",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], |row| Ok(BookId(row.get(0)?)))
            .map_err(db_err)?;
        rows.collect::<rusqlite::Result<_>>().map_err(db_err)
    }

    /// True when the stored position has moved since it last agreed with
    /// the service.
    ///
    /// A book with no position is not dirty; a position that has never
    /// synced is.
    pub fn position_needs_push(&self, id: BookId) -> Result<bool> {
        self.conn
            .query_row(
                "SELECT p.revision > COALESCE(s.position_synced_revision, 0)
                 FROM positions p
                 LEFT JOIN book_sync s ON s.book_id = p.book_id
                 WHERE p.book_id = ?1",
                params![id.0],
                |row| row.get::<_, bool>(0),
            )
            .optional()
            .map_err(db_err)
            .map(|dirty| dirty.unwrap_or(false))
    }

    /// Every book whose position owes a service a write and has one to
    /// talk to. What a shell iterates when it decides the moment is right.
    pub fn positions_needing_push(&self) -> Result<Vec<PositionPush>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT p.book_id, s.progression_url, p.revision
                 FROM positions p
                 JOIN book_sync s ON s.book_id = p.book_id
                 JOIN books b ON b.id = p.book_id
                 WHERE s.progression_url IS NOT NULL
                   AND b.deleted = 0
                   AND p.revision > COALESCE(s.position_synced_revision, 0)
                 ORDER BY p.book_id",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok(PositionPush {
                    book: BookId(row.get(0)?),
                    progression_url: row.get(1)?,
                    revision: row.get(2)?,
                })
            })
            .map_err(db_err)?;
        rows.collect::<rusqlite::Result<_>>().map_err(db_err)
    }

    /// Record that `revision` of this book's position reached the service,
    /// and what the service's `modified` was when it did.
    ///
    /// `revision` is the one that was *pushed*, from
    /// [`PositionPush::revision`] — not whatever the row holds now. A
    /// reader who turns a page while the request is in flight has written
    /// a newer revision, and stamping that would mark a position clean
    /// that the service has never seen.
    pub fn mark_position_synced(
        &mut self,
        id: BookId,
        revision: i64,
        remote_modified: &str,
    ) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO book_sync (book_id, remote_modified,
                        position_synced_revision, updated_at)
                 VALUES (?1, ?2, ?3, strftime('%s','now'))
                 ON CONFLICT(book_id) DO UPDATE SET
                        remote_modified = ?2,
                        position_synced_revision = ?3,
                        updated_at = strftime('%s','now')",
                params![id.0, remote_modified, revision],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// Annotations that owe the container a write: never-pushed rows,
    /// edited rows, and soft-deleted rows whose remote copy is still
    /// there.
    ///
    /// A deleted row that was never pushed owes nothing and is not
    /// returned — there is nothing on the server to remove.
    pub fn annotations_needing_push(&self, id: BookId) -> Result<Vec<AnnotationSync>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, remote_iri, remote_etag, deleted, revision
                 FROM annotations
                 WHERE book_id = ?1
                   AND (deleted = 0 OR remote_iri IS NOT NULL)
                   AND revision > COALESCE(synced_revision, 0)
                 ORDER BY id",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![id.0], |row| {
                Ok(AnnotationSync {
                    id: row.get(0)?,
                    remote_iri: row.get(1)?,
                    remote_etag: row.get(2)?,
                    deleted: row.get::<_, i64>(3)? != 0,
                    revision: row.get(4)?,
                })
            })
            .map_err(db_err)?;
        rows.collect::<rusqlite::Result<_>>().map_err(db_err)
    }

    /// Record that `revision` of this annotation reached the container.
    ///
    /// `revision` is the one that was pushed, from
    /// [`AnnotationSync::revision`], for the reason
    /// [`Library::mark_position_synced`] gives — and it matters more here,
    /// because a position corrects itself on the next page turn and a
    /// missed annotation edit differs from the container for good.
    pub fn mark_annotation_synced(
        &mut self,
        annotation_id: i64,
        revision: i64,
        remote_iri: &str,
        remote_etag: Option<&str>,
    ) -> Result<()> {
        self.conn
            .execute(
                "UPDATE annotations
                 SET remote_iri = ?3, remote_etag = ?4, synced_revision = ?2
                 WHERE id = ?1",
                params![annotation_id, revision, remote_iri, remote_etag],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// The local row a container IRI belongs to, for reconciling a pull.
    pub fn annotation_by_remote_iri(&self, remote_iri: &str) -> Result<Option<i64>> {
        self.conn
            .query_row(
                "SELECT id FROM annotations WHERE remote_iri = ?1",
                params![remote_iri],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err)
    }

    /// Forget an annotation for good, once the container has confirmed the
    /// delete.
    ///
    /// [`Library::delete_annotation`] is the soft delete a reader's action
    /// produces; the row has to stay until the server has been told, which
    /// is what made deletes soft in the first place. This is the other end
    /// of that, and it refuses a row the reader still has.
    pub fn purge_annotation(&mut self, annotation_id: i64) -> Result<()> {
        self.conn
            .execute(
                "DELETE FROM annotations WHERE id = ?1 AND deleted = 1",
                params![annotation_id],
            )
            .map_err(db_err)?;
        Ok(())
    }
}

/// The per-user data directory this platform uses, before the app name.
///
/// `None` where there is no convention to follow — every mobile and wasm
/// target, and any Unix with no `HOME`.
#[cfg(all(
    unix,
    not(target_os = "macos"),
    not(target_os = "android"),
    not(target_os = "ios")
))]
fn platform_dir() -> Option<PathBuf> {
    // XDG Base Directory: a relative `XDG_DATA_HOME` is invalid and the
    // spec says to ignore it rather than resolve it against the CWD.
    if let Some(dir) = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
    {
        return Some(dir);
    }
    home().map(|home| home.join(".local/share"))
}

#[cfg(target_os = "macos")]
fn platform_dir() -> Option<PathBuf> {
    home().map(|home| home.join("Library/Application Support"))
}

#[cfg(windows)]
fn platform_dir() -> Option<PathBuf> {
    // Roaming first: a library is user data worth following the user to
    // another machine. `LOCALAPPDATA` is the fallback for a profile with
    // roaming disabled.
    std::env::var_os("APPDATA")
        .or_else(|| std::env::var_os("LOCALAPPDATA"))
        .map(PathBuf::from)
        .filter(|dir| !dir.as_os_str().is_empty())
}

/// Android, iOS, wasm, and anything else: the host has to say.
#[cfg(not(any(
    windows,
    target_os = "macos",
    all(unix, not(target_os = "android"), not(target_os = "ios"))
)))]
fn platform_dir() -> Option<PathBuf> {
    None
}

// Exactly the platforms whose `platform_dir` consults it: the XDG arm and
// macOS. Wider and it compiles as dead code on iOS — where only a cross
// build ever warns, because no gate runs clippy for that target.
#[cfg(all(unix, not(target_os = "android"), not(target_os = "ios")))]
fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|dir| !dir.as_os_str().is_empty())
}

/// A file extension for a cover's media type. Covers are jpeg or png in
/// practice; anything else keeps its bytes under a generic name, since the
/// shelf decodes by content and not by name anyway.
fn cover_extension(media_type: &str) -> &'static str {
    match media_type {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => "img",
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod default_dir_tests {
    use super::*;

    /// These tests *are* about process-global environment, so unlike the
    /// session tests they still serialize. That is the right place for a
    /// lock: around the code that reads the environment, not around every
    /// caller that merely wants a library somewhere.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Set several variables at once, run, restore.
    ///
    /// Takes the whole set rather than one at a time because the lock is
    /// not reentrant, and a per-variable helper deadlocks the moment a
    /// test needs to pin two of them.
    fn with_vars<T>(vars: &[(&str, Option<&str>)], f: impl FnOnce() -> T) -> T {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let previous: Vec<_> = vars
            .iter()
            .map(|(name, _)| (*name, std::env::var_os(name)))
            .collect();
        for (name, value) in vars {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
        let out = f();
        for (name, value) in previous {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
        out
    }

    #[test]
    fn the_override_wins_over_every_platform_convention() {
        let dir = with_vars(
            &[("CHAPBOOK_LIBRARY_DIR", Some("/tmp/chapbook-override"))],
            || Library::default_dir().unwrap(),
        );
        assert_eq!(dir, PathBuf::from("/tmp/chapbook-override"));
    }

    #[test]
    fn an_empty_override_is_a_mistake_and_not_a_request_for_the_cwd() {
        let dir = with_vars(&[("CHAPBOOK_LIBRARY_DIR", Some(""))], Library::default_dir);
        // Whatever this platform answers, it is not the current directory.
        if let Ok(dir) = dir {
            assert!(dir.is_absolute(), "{}", dir.display());
        }
    }

    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn linux_follows_xdg_and_ignores_a_relative_data_home() {
        let dir = with_vars(
            &[
                ("CHAPBOOK_LIBRARY_DIR", None),
                ("XDG_DATA_HOME", Some("/xdg/data")),
            ],
            || Library::default_dir().unwrap(),
        );
        assert_eq!(dir, PathBuf::from("/xdg/data/chapbook"));

        // The spec says a relative XDG_DATA_HOME is invalid: ignore it
        // rather than resolve it against the working directory.
        let dir = with_vars(
            &[
                ("CHAPBOOK_LIBRARY_DIR", None),
                ("XDG_DATA_HOME", Some("relative/data")),
            ],
            || Library::default_dir().unwrap(),
        );
        assert!(dir.is_absolute(), "{}", dir.display());
        assert!(dir.ends_with(".local/share/chapbook"), "{}", dir.display());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn macos_uses_application_support_and_not_the_linux_path() {
        let dir = with_vars(&[("CHAPBOOK_LIBRARY_DIR", None)], || {
            Library::default_dir().unwrap()
        });
        assert!(
            dir.ends_with("Library/Application Support/chapbook"),
            "{}",
            dir.display()
        );
    }

    #[test]
    #[cfg(windows)]
    fn windows_uses_appdata_rather_than_falling_into_the_working_directory() {
        let dir = with_vars(
            &[
                ("CHAPBOOK_LIBRARY_DIR", None),
                ("APPDATA", Some(r"C:\Users\test\AppData\Roaming")),
            ],
            || Library::default_dir().unwrap(),
        );
        assert_eq!(
            dir,
            PathBuf::from(r"C:\Users\test\AppData\Roaming\chapbook")
        );
    }

    /// The old code ended `.unwrap_or_else(|_| ".".into())`, so a process
    /// with no home wrote a library into wherever it happened to start.
    #[test]
    #[cfg(unix)]
    fn no_home_is_an_error_rather_than_the_working_directory() {
        let result = with_vars(
            &[
                ("CHAPBOOK_LIBRARY_DIR", None),
                ("XDG_DATA_HOME", None),
                ("HOME", None),
            ],
            Library::default_dir,
        );
        let Err(err) = result else {
            panic!("nowhere to put a library is an error, not a path");
        };
        assert!(err.to_string().contains("CHAPBOOK_LIBRARY_DIR"), "{err}");
    }
}
