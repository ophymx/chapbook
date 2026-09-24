//! Schema and migrations. `PRAGMA user_version` tracks the schema version;
//! migrations apply in order inside a transaction.
//!
//! Schema hygiene per docs/LOCATORS.md: stable ids, updated-at timestamps,
//! and soft deletes on positions/annotations — single-device today, but
//! sync later becomes serialization, not redesign.

use rusqlite::Connection;

use chapbook_core::{ChapbookError, Result};

const MIGRATIONS: &[&str] = &[
    // v1
    "
    CREATE TABLE books (
        id INTEGER PRIMARY KEY,
        title TEXT NOT NULL,
        language TEXT,
        identifier TEXT,
        file_path TEXT NOT NULL,
        source_path TEXT,
        -- Edition fingerprint (SHA-1 of file bytes). Book identity is the
        -- library id; a changed fingerprint triggers the re-anchor chain,
        -- never orphans state.
        fingerprint TEXT NOT NULL,
        added_at INTEGER NOT NULL,
        deleted INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX idx_books_fingerprint ON books(fingerprint);

    CREATE TABLE authors (
        id INTEGER PRIMARY KEY,
        name TEXT NOT NULL UNIQUE
    );
    CREATE TABLE book_authors (
        book_id INTEGER NOT NULL REFERENCES books(id),
        author_id INTEGER NOT NULL REFERENCES authors(id),
        position INTEGER NOT NULL,
        PRIMARY KEY (book_id, author_id)
    );

    -- One reading position per book: the full layered locator record.
    CREATE TABLE positions (
        book_id INTEGER PRIMARY KEY REFERENCES books(id),
        spine_href TEXT NOT NULL,
        spine_index INTEGER NOT NULL,
        char_offset INTEGER NOT NULL,
        locator_version INTEGER NOT NULL,
        quote_prefix TEXT NOT NULL,
        quote_exact TEXT NOT NULL,
        quote_suffix TEXT NOT NULL,
        spine_fraction REAL NOT NULL,
        book_progression REAL NOT NULL,
        updated_at INTEGER NOT NULL
    );

    -- Annotations carry layered locators per endpoint (a drifted highlight
    -- silently marks the wrong text, so the quote layer is mandatory).
    CREATE TABLE annotations (
        id INTEGER PRIMARY KEY,
        book_id INTEGER NOT NULL REFERENCES books(id),
        kind TEXT NOT NULL CHECK (kind IN ('bookmark','highlight','note')),
        start_spine_href TEXT NOT NULL,
        start_spine_index INTEGER NOT NULL,
        start_char_offset INTEGER NOT NULL,
        start_locator_version INTEGER NOT NULL,
        start_quote_prefix TEXT NOT NULL,
        start_quote_exact TEXT NOT NULL,
        start_quote_suffix TEXT NOT NULL,
        start_spine_fraction REAL NOT NULL,
        start_book_progression REAL NOT NULL,
        end_spine_href TEXT,
        end_spine_index INTEGER,
        end_char_offset INTEGER,
        end_locator_version INTEGER,
        end_quote_prefix TEXT,
        end_quote_exact TEXT,
        end_quote_suffix TEXT,
        end_spine_fraction REAL,
        end_book_progression REAL,
        note_text TEXT,
        color TEXT,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL,
        deleted INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX idx_annotations_book ON annotations(book_id, deleted);

    -- Catalog URLs may embed per-user API keys: opaque, never logged.
    CREATE TABLE opds_sources (
        id INTEGER PRIMARY KEY,
        url TEXT NOT NULL,
        title TEXT,
        auth_user TEXT,
        auth_secret TEXT,
        added_at INTEGER NOT NULL,
        deleted INTEGER NOT NULL DEFAULT 0
    );
    ",
    // v2
    "
    -- Reading settings, stored complete per scope: book_id 0 is the
    -- reader's default and any other id is that book's override, so a
    -- customized book keeps its own settings when the default changes.
    -- No foreign key: 0 is deliberately not a book.
    CREATE TABLE reading_settings (
        book_id INTEGER PRIMARY KEY,
        base_font_px REAL NOT NULL,
        line_height REAL NOT NULL,
        justify INTEGER NOT NULL,
        publisher_styles INTEGER NOT NULL,
        theme TEXT NOT NULL,
        updated_at INTEGER NOT NULL
    );
    ",
    // v3
    "
    -- Secrets leave the library. A password sitting in plaintext in a
    -- SQLite file is a desktop habit that does not survive a phone, and
    -- keeping the column would mean two stores with the insecure one
    -- winning by accident. Credentials now live behind an injected
    -- `chapbook_core::CredentialStore` — Keychain, Keystore, Secret
    -- Service — keyed by this row's id. `auth_user` stays: an account name
    -- is a label, not a secret, and a settings screen needs it without
    -- unlocking anything.
    ALTER TABLE opds_sources DROP COLUMN auth_secret;
    ",
    // v4
    "
    -- What a shelf needs and could not ask for. A cover is the thing a
    -- reader recognises a book by, and every `Publication` can already
    -- produce one — but `import` only ever saw the metadata, so nothing
    -- captured it and a browsing UI would have had to reopen every book on
    -- every paint. NULL means no cover; the file lives beside the managed
    -- copy under `covers/`.
    ALTER TABLE books ADD COLUMN cover_path TEXT;
    ",
    // v5
    "
    -- The reader's chosen typeface. NULL means the publisher's, which is
    -- what every existing row means and why this needs no backfill: the
    -- column defaults to NULL and an untouched settings row keeps behaving
    -- exactly as it did.
    --
    -- A family *name*, not a path or an id. What resolves it is the
    -- session's own font database, which differs per device and per book —
    -- a book's `@font-face` families join it as chapters load — so a name
    -- that resolves on one device and not another is normal, and is
    -- handled the same way an unknown family in a publisher's stylesheet
    -- is: the cascade moves on to the next one.
    ALTER TABLE reading_settings ADD COLUMN font_family TEXT;
    ",
    // v6
    "
    -- Where a book's position and marks sync to, and how far they have
    -- got. Separate from `opds_sources`: that row is a catalog the reader
    -- browses, and these are per-*publication* service URLs, because in
    -- both protocols the URL *is* the publication's identity. There is no
    -- id in a Progression document and a Web Annotation container is
    -- scoped by the link that named it, so a book sideloaded from disk has
    -- no service at all until something maps it back to a catalog entry.
    -- Both columns are therefore NULL for most books and that is the
    -- normal case, not a gap.
    --
    -- Opaque and possibly secret-bearing, exactly like `opds_sources.url`:
    -- a service URL may embed a per-user key. Never log or normalize.
    CREATE TABLE book_sync (
        book_id INTEGER PRIMARY KEY REFERENCES books(id),
        progression_url TEXT,
        annotation_container TEXT,
        -- The `modified` of the progression document last seen from the
        -- service, verbatim. Stored as the string it arrived as and
        -- compared for equality, never parsed: chapbook has no date type,
        -- the draft only promises ISO 8601, and equality is the only
        -- question being asked - did the service's copy change since we
        -- looked. Ordering is the service's job, and it does it (409).
        remote_modified TEXT,
        -- The `positions.revision` that last went out, NOT a timestamp.
        -- See the revision columns below.
        position_synced_revision INTEGER,
        updated_at INTEGER NOT NULL
    );

    -- Dirty tracking is a revision, never a clock.
    --
    -- `updated_at` is `strftime('%s','now')`, which has one-second
    -- resolution, so an edit landing in the same second as a sync mark
    -- compares equal and looks clean. For a position that costs one page
    -- turn; for an annotation it is an edit that is never pushed and
    -- silently differs from the container for good. Wall clocks also run
    -- backwards - NTP steps, a device whose user changes the date - and
    -- \"has this changed since we synced\" should not be able to answer
    -- \"no\" because of any of that. A counter that only ever goes up
    -- answers exactly the question and nothing else.
    --
    -- `updated_at` stays: it is what a reader's \"recently annotated\"
    -- list orders by, which is a different job.
    ALTER TABLE positions ADD COLUMN revision INTEGER NOT NULL DEFAULT 1;
    ALTER TABLE annotations ADD COLUMN revision INTEGER NOT NULL DEFAULT 1;

    -- The remote half of one annotation. NULL `remote_iri` means it has
    -- never been pushed; the container mints the IRI and it is the sync
    -- identity from then on.
    ALTER TABLE annotations ADD COLUMN remote_iri TEXT;
    -- The entity tag the IRI was last read or written at, sent back as
    -- `If-Match`. NULL means unguarded - either never pushed, or a
    -- container that issues no tags.
    ALTER TABLE annotations ADD COLUMN remote_etag TEXT;
    -- The `revision` that last agreed with the container.
    ALTER TABLE annotations ADD COLUMN synced_revision INTEGER;

    -- Finding what still owes the server a write, including the deletes:
    -- a soft-deleted row with a `remote_iri` is a DELETE that has not
    -- happened yet, which is the whole reason deletes were soft from v1.
    CREATE INDEX idx_annotations_sync ON annotations(book_id, synced_revision, revision);
    CREATE UNIQUE INDEX idx_annotations_remote ON annotations(remote_iri)
        WHERE remote_iri IS NOT NULL;
    ",
    // v7
    "
    -- What a book belongs to, and how far through it the reader got.
    --
    -- Everything here is a *browsing* fact. v4 gave the shelf a cover and
    -- v6 gave it sync; what it still could not do is answer the three
    -- questions a reader asks of a shelf that has grown past one screen:
    -- what is this part of, what have I finished, and where is the one I
    -- am thinking of.

    -- A series is the book's own claim about itself, off its metadata,
    -- so it is a column rather than a table: nothing else refers to it,
    -- and two books in \"the same\" series agree only as far as their
    -- publishers spelled it the same way. Normalizing that into rows
    -- would promise an identity the data does not have.
    ALTER TABLE books ADD COLUMN series TEXT;
    -- Fractional, because a novella between books two and three is 2.5 in
    -- every catalogue that has one. NULL beside a set series is ordinary
    -- and sorts last, not first: an unplaced volume is not volume zero.
    ALTER TABLE books ADD COLUMN series_index REAL;
    CREATE INDEX idx_books_series ON books(series) WHERE series IS NOT NULL;

    -- When the reader reached the end. NULL is \"not finished\", which is
    -- what every existing row means, so there is nothing to backfill.
    --
    -- A flag and not a derivation. Progress can say 1.0 for a book
    -- skimmed to the last page, and a book genuinely finished and then
    -- reopened has its progress reset to the beginning by the next
    -- position write - so a shelf deriving \"finished\" from progress gets
    -- it wrong in both directions. It is also a fact about the reader,
    -- not about the file: it survives a re-anchor and a changed edition,
    -- both of which move every locator.
    ALTER TABLE books ADD COLUMN finished_at INTEGER;

    -- A named set of books. One concept, whether the reader's app calls
    -- it a shelf, a collection or a tag: all three are a name with books
    -- in it, and a library that models them separately makes the reader
    -- choose which drawer a word goes in before knowing what the drawers
    -- do.
    --
    -- Soft-deleted like everything else here, for the reason the module
    -- doc gives: nothing syncs collections today, and the schema should
    -- not be what makes that a redesign.
    CREATE TABLE collections (
        id INTEGER PRIMARY KEY,
        name TEXT NOT NULL,
        added_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL,
        deleted INTEGER NOT NULL DEFAULT 0
    );
    -- Unique among the living only: a reader who deletes \"Sci-Fi\" and
    -- makes it again should not be told the name is taken by a row they
    -- cannot see.
    CREATE UNIQUE INDEX idx_collections_name ON collections(name)
        WHERE deleted = 0;

    CREATE TABLE book_collections (
        book_id INTEGER NOT NULL REFERENCES books(id),
        collection_id INTEGER NOT NULL REFERENCES collections(id),
        added_at INTEGER NOT NULL,
        PRIMARY KEY (book_id, collection_id)
    );
    -- The primary key already serves \"what is this book in\"; this is the
    -- other direction, \"what is in this collection\", which is the one a
    -- shelf filters by.
    CREATE INDEX idx_book_collections_members
        ON book_collections(collection_id, book_id);

    -- Search, as a reader means it.
    --
    -- The previous answer was `title LIKE '%x%' OR name LIKE '%x%'`,
    -- which cannot use an index and, worse, cannot match: SQLite's LIKE
    -- folds case for ASCII only, so a shelf holding Charlotte Brontë
    -- answers nothing to \"bronte\", and a reader who cannot type the
    -- diaeresis cannot find their own book. `remove_diacritics 2` is the
    -- fix and is the whole reason this is FTS5 rather than a better LIKE.
    --
    -- Contentless would save the copy, but the copy is three short
    -- strings per book and an ordinary table is one that DELETE and
    -- UPDATE work on normally. The rows are maintained in Rust rather
    -- than by triggers because the authors of a book live one join away
    -- and a trigger would have to be written three times over three
    -- tables to see them.
    CREATE VIRTUAL TABLE book_search USING fts5(
        title, authors, series,
        tokenize = 'unicode61 remove_diacritics 2'
    );
    INSERT INTO book_search (rowid, title, authors, series)
    SELECT b.id,
           b.title,
           COALESCE((SELECT group_concat(a.name, ' ')
                       FROM authors a
                       JOIN book_authors ba ON ba.author_id = a.id
                      WHERE ba.book_id = b.id), ''),
           COALESCE(b.series, '')
      FROM books b;
    ",
    // v8
    "
    -- Two small tables the *application* layer keeps, beside the shelf
    -- rather than in a platform's preference store, so every front end
    -- over this library answers the same questions the same way.

    -- How an adopted book is reached again. An adopted row has an empty
    -- file_path: the platform owns the file, and what it holds is a
    -- token that reopens it — a persisted content URI on Android, a
    -- security-scoped bookmark on iOS, a plain path on a desktop that
    -- adopts. The token is the platform's and opaque here; the library
    -- only keeps it under the fingerprint, which is the key that survives
    -- a reinstall (the row id is the reader's history of the book, not
    -- its identity across installs).
    CREATE TABLE grants (
        fingerprint TEXT PRIMARY KEY,
        token BLOB NOT NULL,
        updated_at INTEGER NOT NULL
    );

    -- The app's own display preferences: how a reader *shows* a thing,
    -- not how a page is laid out (that is reading_settings, which crosses
    -- into the engine). Free-form key and value, nothing secret, and no
    -- schema per preference — a front end that gains a preference gains a
    -- key, not a migration.
    CREATE TABLE preferences (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL,
        updated_at INTEGER NOT NULL
    );
    ",
];

pub(crate) fn open_and_migrate(path: &std::path::Path) -> Result<Connection> {
    let conn = Connection::open(path)
        .map_err(|e| ChapbookError::Library(format!("open {}: {e}", path.display())))?;
    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(db_err)?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(db_err)?;

    let version: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(db_err)?;
    let target = MIGRATIONS.len() as i64;
    if version > target {
        return Err(ChapbookError::Library(format!(
            "database schema v{version} is newer than this build supports (v{target})"
        )));
    }
    for (i, migration) in MIGRATIONS.iter().enumerate().skip(version as usize) {
        let next = i as i64 + 1;
        conn.execute_batch(&format!(
            "BEGIN;\n{migration}\nPRAGMA user_version = {next};\nCOMMIT;"
        ))
        .map_err(|e| ChapbookError::Library(format!("migration to v{next}: {e}")))?;
    }
    Ok(conn)
}

pub(crate) fn db_err(e: rusqlite::Error) -> ChapbookError {
    ChapbookError::Library(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        use std::hash::{BuildHasher, Hasher};
        let suffix = std::collections::hash_map::RandomState::new()
            .build_hasher()
            .finish();
        let dir = std::env::temp_dir().join(format!(
            "chapbook-db-test-{}-{name}-{suffix}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn columns(conn: &Connection, table: &str) -> Vec<String> {
        conn.prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    #[test]
    fn a_fresh_database_has_nowhere_to_put_a_secret() {
        let dir = scratch("fresh");
        let conn = open_and_migrate(&dir.join("library.db")).unwrap();
        let names = columns(&conn, "opds_sources");
        assert!(
            !names.iter().any(|c| c == "auth_secret"),
            "secrets belong in the credential store: {names:?}"
        );
        assert!(names.iter().any(|c| c == "auth_user"), "{names:?}");
        drop(conn);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn upgrading_an_old_database_takes_the_plaintext_password_with_it() {
        // A v1 file written before the credential store existed, with a
        // password sitting in it. The migration has to remove the value,
        // not just stop reading it.
        let dir = scratch("upgrade");
        let path = dir.join("library.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(&format!(
                "BEGIN;\n{}\nPRAGMA user_version = 1;\nCOMMIT;",
                MIGRATIONS[0]
            ))
            .unwrap();
            conn.execute(
                "INSERT INTO opds_sources (url, title, auth_user, auth_secret, added_at)
                 VALUES ('https://cat.example.com/opds/', 'Old', 'user', 'hunter2', 0)",
                [],
            )
            .unwrap();
        }

        let conn = open_and_migrate(&path).unwrap();
        let names = columns(&conn, "opds_sources");
        assert!(!names.iter().any(|c| c == "auth_secret"), "{names:?}");

        // The row survives; the account label survives; the password does
        // not exist anywhere in the file's page contents.
        let (url, user): (String, String) = conn
            .query_row("SELECT url, auth_user FROM opds_sources", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!(url, "https://cat.example.com/opds/");
        assert_eq!(user, "user");
        conn.execute_batch("VACUUM").unwrap();
        drop(conn);
        let bytes = std::fs::read(&path).unwrap();
        assert!(
            !bytes.windows(7).any(|w| w == b"hunter2"),
            "the old plaintext password is still in the database file"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Everything in a library that predates sync has never synced, and
    /// has to come out of the migration saying so — a row defaulting to
    /// "clean" would mean a reader's existing marks silently never
    /// reached the container they later connected.
    #[test]
    fn a_library_written_before_sync_owes_the_server_everything() {
        let dir = scratch("sync-upgrade");
        let path = dir.join("library.db");
        {
            let conn = Connection::open(&path).unwrap();
            let upto_v5 = MIGRATIONS[..5].join("\n");
            conn.execute_batch(&format!(
                "BEGIN;\n{upto_v5}\nPRAGMA user_version = 5;\nCOMMIT;"
            ))
            .unwrap();
            conn.execute(
                "INSERT INTO books (id, title, file_path, fingerprint, added_at)
                 VALUES (1, 'Old Book', '/books/old.epub', 'abc123', 0)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO positions (book_id, spine_href, spine_index, char_offset,
                        locator_version, quote_prefix, quote_exact, quote_suffix,
                        spine_fraction, book_progression, updated_at)
                 VALUES (1, 'ch1.xhtml', 0, 42, 2, 'a', '', 'b', 0.5, 0.25, 1000)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO annotations (book_id, kind,
                        start_spine_href, start_spine_index, start_char_offset,
                        start_locator_version, start_quote_prefix, start_quote_exact,
                        start_quote_suffix, start_spine_fraction, start_book_progression,
                        created_at, updated_at)
                 VALUES (1, 'highlight', 'ch1.xhtml', 0, 10, 2, 'a', 'b', 'c', 0.1, 0.1,
                         1000, 1000)",
                [],
            )
            .unwrap();
        }

        let conn = open_and_migrate(&path).unwrap();

        // The old rows survived, with their positions intact.
        let offset: i64 = conn
            .query_row(
                "SELECT char_offset FROM positions WHERE book_id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(offset, 42, "the stored position did not survive");

        // And both start at revision 1 with nothing synced, which is what
        // "dirty" is: revision > COALESCE(synced, 0).
        let (revision, synced): (i64, Option<i64>) = conn
            .query_row(
                "SELECT a.revision, a.synced_revision FROM annotations a WHERE a.id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(revision, 1);
        assert_eq!(synced, None);
        assert!(
            revision > synced.unwrap_or(0),
            "an old mark looks already synced"
        );

        let position_revision: i64 = conn
            .query_row(
                "SELECT revision FROM positions WHERE book_id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(position_revision, 1);

        // Nothing was invented about where it syncs to.
        let rows: i64 = conn
            .query_row("SELECT count(*) FROM book_sync", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 0, "a book with no catalog was given a service");

        drop(conn);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A library that predates the shelf has to come out of the
    /// migration *findable*. The columns are the easy half; the search
    /// index is the half that can silently do nothing, because an empty
    /// FTS table is a perfectly valid FTS table and every query against
    /// it succeeds and returns nothing.
    #[test]
    fn books_added_before_the_search_index_are_in_it() {
        let dir = scratch("search-backfill");
        let path = dir.join("library.db");
        {
            let conn = Connection::open(&path).unwrap();
            let upto_v6 = MIGRATIONS[..6].join("\n");
            conn.execute_batch(&format!(
                "BEGIN;\n{upto_v6}\nPRAGMA user_version = 6;\nCOMMIT;"
            ))
            .unwrap();
            conn.execute(
                "INSERT INTO books (id, title, file_path, fingerprint, added_at)
                 VALUES (1, 'Villette', '/books/1.epub', 'abc123', 0)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO authors (id, name) VALUES (1, 'Charlotte Brontë')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO book_authors (book_id, author_id, position) VALUES (1, 1, 0)",
                [],
            )
            .unwrap();
        }

        let conn = open_and_migrate(&path).unwrap();

        // The book kept everything it had, and gained the columns with
        // the meaning an untouched row is supposed to have.
        let (title, series, finished): (String, Option<String>, Option<i64>) = conn
            .query_row(
                "SELECT title, series, finished_at FROM books WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(title, "Villette");
        assert_eq!(series, None, "nothing invented a series");
        assert_eq!(finished, None, "an old book is not a finished book");

        // And it is findable, by its title and by the author it took a
        // join to reach — including without the diaeresis, which is the
        // whole reason the index exists.
        for query in [
            "\"Villette\"*",
            "\"bronte\"*",
            "\"charlotte\"* AND \"villette\"*",
        ] {
            let hits: i64 = conn
                .query_row(
                    "SELECT count(*) FROM book_search WHERE book_search MATCH ?1",
                    [query],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(hits, 1, "{query} found nothing");
        }

        drop(conn);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A settings row written before the typeface could be chosen has to
    /// keep meaning what it meant. NULL is the publisher's font, so the
    /// column needs no backfill — but "needs no backfill" is a claim about
    /// a real file, not about the DDL.
    #[test]
    fn a_settings_row_written_before_v5_still_means_the_publishers_font() {
        let dir = scratch("font-upgrade");
        let path = dir.join("library.db");
        {
            let conn = Connection::open(&path).unwrap();
            let upto_v4 = MIGRATIONS[..4].join("\n");
            conn.execute_batch(&format!(
                "BEGIN;\n{upto_v4}\nPRAGMA user_version = 4;\nCOMMIT;"
            ))
            .unwrap();
            conn.execute(
                "INSERT INTO reading_settings
                    (book_id, base_font_px, line_height, justify, publisher_styles,
                     theme, updated_at)
                 VALUES (0, 21.0, 1.4, 1, 1, 'sepia', 0)",
                [],
            )
            .unwrap();
        }

        let conn = open_and_migrate(&path).unwrap();
        let names = columns(&conn, "reading_settings");
        assert!(names.iter().any(|c| c == "font_family"), "{names:?}");

        let (size, theme, family): (f64, String, Option<String>) = conn
            .query_row(
                "SELECT base_font_px, theme, font_family FROM reading_settings WHERE book_id = 0",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(size, 21.0, "the old row survived the migration");
        assert_eq!(theme, "sepia");
        assert_eq!(family, None, "an untouched row still means the publisher's");
    }
}
