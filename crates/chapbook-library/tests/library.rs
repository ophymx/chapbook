//! Library persistence round-trips and the position restore chain.

use chapbook_core::{BookMetadata, LayeredLocator, SpineItem};
use chapbook_library::{restore_position, AnnotationKind, Library, RestoreTier};

fn temp_library() -> (Library, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "chapbook-lib-test-{}-{}",
        std::process::id(),
        rand_suffix()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    (Library::open(&dir).unwrap(), dir)
}

fn rand_suffix() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish()
}

fn sample_book(dir: &std::path::Path, name: &str, contents: &[u8]) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, contents).unwrap();
    path
}

/// A publication that is nothing but its metadata and, optionally, a
/// cover. `import` takes the whole publication now — it needs `cover()` —
/// so the tests need something that is one.
struct FakeBook {
    metadata: BookMetadata,
    cover: Option<chapbook_core::Resource>,
}

impl FakeBook {
    fn new(title: &str) -> FakeBook {
        FakeBook {
            metadata: metadata(title),
            cover: None,
        }
    }

    fn with_series(mut self, series: &str, index: Option<f64>) -> FakeBook {
        self.metadata.series = Some(series.to_string());
        self.metadata.series_index = index;
        self
    }

    fn by(mut self, authors: &[&str]) -> FakeBook {
        self.metadata.authors = authors.iter().map(|a| a.to_string()).collect();
        self
    }

    fn with_cover(mut self, media_type: &str, data: &[u8]) -> FakeBook {
        self.cover = Some(chapbook_core::Resource {
            media_type: media_type.to_string(),
            data: data.to_vec(),
        });
        self
    }
}

impl chapbook_core::Publication for FakeBook {
    fn kind(&self) -> chapbook_core::BookKind {
        chapbook_core::BookKind::Epub
    }
    fn metadata(&self) -> &BookMetadata {
        &self.metadata
    }
    fn spine(&self) -> &[SpineItem] {
        &[]
    }
    fn toc(&self) -> &[chapbook_core::TocEntry] {
        &[]
    }
    fn unit_bytes(&self, index: usize) -> chapbook_core::Result<Vec<u8>> {
        Err(chapbook_core::ChapbookError::SpineOutOfRange(index))
    }
    fn cover(&self) -> chapbook_core::Result<Option<chapbook_core::Resource>> {
        Ok(self.cover.as_ref().map(|c| chapbook_core::Resource {
            media_type: c.media_type.clone(),
            data: c.data.clone(),
        }))
    }
}

fn metadata(title: &str) -> BookMetadata {
    BookMetadata {
        title: Some(title.to_string()),
        authors: vec!["Ada Fixture".into(), "Co Author".into()],
        language: Some("en".into()),
        identifier: Some("urn:uuid:test".into()),
        description: None,
        format_version: "3.0".into(),
        ..Default::default()
    }
}

const TEXT: &str = "It was a truth universally acknowledged that a reader in \
                    possession of a position must be in want of restoring it.";

fn locator_at(offset: u32) -> LayeredLocator {
    LayeredLocator::capture("OEBPS/ch2.xhtml", 1, TEXT, offset, 100, 1000)
}

#[test]
fn import_ls_and_fingerprint_dedup() {
    let (mut lib, dir) = temp_library();
    let source = sample_book(&dir, "src.epub", b"fake epub bytes one");
    let id = lib.import(&source, &FakeBook::new("Book One")).unwrap();

    // Managed copy exists and the original path is not it.
    let books = lib.books(None).unwrap();
    assert_eq!(books.len(), 1);
    assert_eq!(books[0].title, "Book One");
    assert_eq!(books[0].authors, vec!["Ada Fixture", "Co Author"]);
    assert!(books[0].file_path.exists());
    assert_ne!(books[0].file_path, source);

    // Same bytes re-imported: same book, no duplicate.
    let again = lib
        .import(&source, &FakeBook::new("Book One Again"))
        .unwrap();
    assert_eq!(id, again);
    assert_eq!(lib.books(None).unwrap().len(), 1);

    // Different bytes: a new book.
    let other = sample_book(&dir, "other.epub", b"fake epub bytes two");
    let other_id = lib.import(&other, &FakeBook::new("Book Two")).unwrap();
    assert_ne!(id, other_id);
    assert_eq!(lib.books(None).unwrap().len(), 2);

    // Filter by author substring.
    assert_eq!(lib.books(Some("Fixture")).unwrap().len(), 2);
    assert_eq!(lib.books(Some("Book Two")).unwrap().len(), 1);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn position_roundtrip_and_update() {
    let (mut lib, dir) = temp_library();
    let source = sample_book(&dir, "b.epub", b"bytes");
    let id = lib.import(&source, &FakeBook::new("B")).unwrap();

    assert!(lib.position(id).unwrap().is_none());
    let loc = locator_at(40);
    lib.set_position(id, &loc).unwrap();
    let stored = lib.position(id).unwrap().unwrap();
    assert_eq!(stored.locator, loc);

    // Upsert replaces.
    let later = locator_at(80);
    lib.set_position(id, &later).unwrap();
    assert_eq!(lib.position(id).unwrap().unwrap().locator, later);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn annotations_roundtrip_and_soft_delete() {
    let (mut lib, dir) = temp_library();
    let source = sample_book(&dir, "b.epub", b"bytes");
    let id = lib.import(&source, &FakeBook::new("B")).unwrap();

    let start = locator_at(10);
    let end = locator_at(30);
    let highlight = lib
        .add_annotation(
            id,
            AnnotationKind::Highlight,
            &start,
            Some(&end),
            Some("a highlighted passage"),
            Some("#ffff00"),
        )
        .unwrap();
    lib.add_annotation(
        id,
        AnnotationKind::Bookmark,
        &locator_at(90),
        None,
        None,
        None,
    )
    .unwrap();

    let annotations = lib.annotations(id).unwrap();
    assert_eq!(annotations.len(), 2);
    assert_eq!(annotations[0].kind, AnnotationKind::Highlight);
    assert_eq!(annotations[0].start, start);
    assert_eq!(annotations[0].end.as_ref(), Some(&end));
    assert_eq!(annotations[1].kind, AnnotationKind::Bookmark);
    assert!(annotations[1].end.is_none());

    lib.delete_annotation(highlight).unwrap();
    let after = lib.annotations(id).unwrap();
    assert_eq!(after.len(), 1, "soft delete hides but keeps the row");
    assert_eq!(after[0].kind, AnnotationKind::Bookmark);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn opds_sources_roundtrip_and_hold_no_secret() {
    let (mut lib, dir) = temp_library();
    let id = lib
        .add_opds_source(
            "https://cat.example.com/opds/abc123secret/",
            Some("Example"),
            Some("user"),
        )
        .unwrap();
    let sources = lib.opds_sources().unwrap();
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].title.as_deref(), Some("Example"));
    assert_eq!(sources[0].auth_user.as_deref(), Some("user"));

    // The id is the credential key, and it must not be secret-bearing.
    // (That the secret column is gone from the schema is db.rs's test.)
    let key = chapbook_core::CredentialKey::opds_source(id);
    assert!(!key.as_str().contains("abc123secret"));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn reopen_persists() {
    let (mut lib, dir) = temp_library();
    let source = sample_book(&dir, "b.epub", b"bytes");
    let id = lib.import(&source, &FakeBook::new("Persistent")).unwrap();
    lib.set_position(id, &locator_at(55)).unwrap();
    drop(lib);

    let lib = Library::open(&dir).unwrap();
    assert_eq!(lib.books(None).unwrap()[0].title, "Persistent");
    assert_eq!(lib.position(id).unwrap().unwrap().locator.char_offset, 55);
    std::fs::remove_dir_all(&dir).ok();
}

// ---- Restore chain ----

fn spine() -> Vec<SpineItem> {
    ["OEBPS/ch1.xhtml", "OEBPS/ch2.xhtml", "OEBPS/ch3.xhtml"]
        .iter()
        .map(|href| SpineItem {
            id: href.to_string(),
            href: href.to_string(),
            media_type: "application/xhtml+xml".into(),
            linear: true,
        })
        .collect()
}

#[test]
fn restore_same_edition_is_exact() {
    let stored = locator_at(40);
    let (locator, tier) = restore_position(&stored, true, &spine(), |i| {
        (i == 1).then(|| TEXT.to_string())
    });
    assert_eq!(tier, RestoreTier::Exact);
    assert_eq!(locator.spine_index, 1);
    assert_eq!(locator.char_offset, 40);
}

#[test]
fn restore_new_edition_reanchors_via_quote() {
    let stored = locator_at(40);
    // New edition prepends a translator's note to chapter 2.
    let drifted = format!("A note from the translator. {TEXT}");
    let (locator, tier) = restore_position(&stored, false, &spine(), move |i| {
        (i == 1).then(|| drifted.clone())
    });
    assert_eq!(tier, RestoreTier::Quote);
    assert_eq!(locator.spine_index, 1);
    assert_eq!(
        locator.char_offset,
        40 + "A note from the translator. ".chars().count() as u32
    );
}

#[test]
fn restore_finds_content_moved_to_neighbor_chapter() {
    let stored = locator_at(40);
    // The new edition re-split chapters: the passage now lives in ch3.
    let (locator, tier) = restore_position(&stored, false, &spine(), |i| match i {
        1 => Some("Completely different content in chapter two now.".to_string()),
        2 => Some(TEXT.to_string()),
        _ => Some("Front matter.".to_string()),
    });
    assert_eq!(tier, RestoreTier::Quote);
    assert_eq!(
        locator.spine_index, 2,
        "quote found in the neighbor chapter"
    );
    assert_eq!(locator.char_offset, 40);
}

#[test]
fn restore_spine_reorder_follows_href() {
    let stored = locator_at(40);
    // Same edition claim, but spine order changed: href wins over index.
    let mut reordered = spine();
    reordered.swap(0, 1); // ch2 is now index 0
    let (locator, tier) = restore_position(&stored, true, &reordered, |i| {
        (i == 0).then(|| TEXT.to_string())
    });
    assert_eq!(tier, RestoreTier::Exact);
    assert_eq!(locator.spine_index, 0);
}

#[test]
fn restore_degrades_to_chapter_start() {
    let stored = locator_at(40);
    let (locator, tier) = restore_position(&stored, false, &spine(), |_| None);
    assert_eq!(tier, RestoreTier::ChapterStart);
    assert!(locator.spine_index < 3);
    assert_eq!(locator.char_offset, 0);
}

#[test]
fn reading_settings_resolve_book_then_default_then_builtin() {
    use chapbook_core::{ReadingSettings, Theme};

    let (mut lib, dir) = temp_library();
    let path = sample_book(&dir, "settings.epub", b"settings fixture");
    let id = lib.import(&path, &FakeBook::new("Settings")).unwrap();

    // Nothing stored: the built-in defaults.
    assert_eq!(lib.effective_settings(Some(id)), ReadingSettings::default());
    assert_eq!(lib.reading_settings(None).unwrap(), None);

    let global = ReadingSettings {
        base_font_px: 21.0,
        theme: Theme::Dark,
        ..Default::default()
    };
    lib.set_reading_settings(None, &global).unwrap();
    assert_eq!(
        lib.effective_settings(Some(id)),
        global,
        "book follows the default"
    );

    let mut mine = global.clone();
    mine.base_font_px = 15.0;
    mine.justify = true;
    lib.set_reading_settings(Some(id), &mine).unwrap();
    assert_eq!(lib.effective_settings(Some(id)), mine);
    assert_eq!(
        lib.effective_settings(None),
        global,
        "the default is untouched"
    );

    // A later change to the default leaves the override alone.
    let mut moved = global.clone();
    moved.base_font_px = 30.0;
    lib.set_reading_settings(None, &moved).unwrap();
    assert_eq!(lib.effective_settings(Some(id)).base_font_px, 15.0);

    lib.clear_reading_settings(id).unwrap();
    assert_eq!(
        lib.effective_settings(Some(id)),
        moved,
        "back to the default"
    );
}

/// A 1x1 PNG, so the cover bytes are a real image and not a marker.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae,
    0x42, 0x60, 0x82,
];

#[test]
fn a_cover_is_kept_at_import_so_a_shelf_has_something_to_draw() {
    let (mut lib, dir) = temp_library();
    let source = sample_book(&dir, "with-cover.epub", b"bytes");
    let id = lib
        .import(
            &source,
            &FakeBook::new("Illustrated").with_cover("image/png", PNG),
        )
        .unwrap();

    let record = lib.book(id).unwrap().unwrap();
    let cover = record.cover_path.expect("a cover was offered and kept");
    assert_eq!(cover.extension().unwrap(), "png", "named for its type");
    assert_eq!(std::fs::read(&cover).unwrap(), PNG, "bytes land intact");

    // A book with no cover says so rather than pointing at nothing.
    let bare = sample_book(&dir, "bare.epub", b"other bytes");
    let bare_id = lib.import(&bare, &FakeBook::new("Bare")).unwrap();
    assert!(lib.book(bare_id).unwrap().unwrap().cover_path.is_none());

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn recent_puts_what_you_were_reading_first() {
    let (mut lib, dir) = temp_library();
    let first = lib
        .import(
            &sample_book(&dir, "first.epub", b"one"),
            &FakeBook::new("Added First"),
        )
        .unwrap();
    let second = lib
        .import(
            &sample_book(&dir, "second.epub", b"two"),
            &FakeBook::new("Added Second"),
        )
        .unwrap();

    // Nothing read yet: newest addition leads, same as a plain listing.
    let shelf = lib.recent(None).unwrap();
    assert_eq!(shelf[0].id, second);
    assert!(shelf.iter().all(|b| b.last_read.is_none()));
    assert!(shelf.iter().all(|b| b.progress.is_none()));

    // Open the older one. It goes to the front, and brings its progress.
    let mut locator = locator_at(10);
    locator.book_progression = 0.42;
    lib.set_position(first, &locator).unwrap();

    let shelf = lib.recent(None).unwrap();
    assert_eq!(shelf[0].id, first, "the book you are in the middle of");
    assert!(shelf[0].last_read.is_some());
    assert!((shelf[0].progress.unwrap() - 0.42).abs() < 1e-9);
    assert_eq!(shelf[1].id, second, "unread still sorts by when it arrived");

    // And a plain listing is unmoved: the two orders answer different
    // questions and must not have become the same method.
    assert_eq!(lib.books(None).unwrap()[0].id, second);

    // The limit is a limit.
    assert_eq!(lib.recent(Some(1)).unwrap().len(), 1);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_removed_book_leaves_the_shelf_and_keeps_its_annotations() {
    let (mut lib, dir) = temp_library();
    let id = lib
        .import(
            &sample_book(&dir, "removable.epub", b"bytes"),
            &FakeBook::new("Removable"),
        )
        .unwrap();
    let start = locator_at(10);
    lib.add_annotation(id, AnnotationKind::Bookmark, &start, None, None, None)
        .unwrap();

    lib.delete_book(id).unwrap();
    assert!(lib.recent(None).unwrap().is_empty(), "gone from the shelf");
    assert!(lib.books(None).unwrap().is_empty(), "and from the listing");
    assert!(lib.book(id).unwrap().is_none());

    // Soft, so re-importing the same file finds its highlights again —
    // which is why the column was in the schema before anything set it.
    assert_eq!(lib.annotations(id).unwrap().len(), 1);

    std::fs::remove_dir_all(&dir).ok();
}

// ---- Sync bookkeeping ----

fn book_with_position(library: &mut Library, dir: &std::path::Path) -> chapbook_library::BookId {
    let path = sample_book(dir, "synced.epub", b"synced");
    let id = library.import(&path, &FakeBook::new("Synced")).unwrap();
    library.set_position(id, &locator(0, 0.1)).unwrap();
    id
}

fn locator(offset: u32, progression: f64) -> LayeredLocator {
    LayeredLocator {
        spine_href: "OEBPS/ch1.xhtml".into(),
        spine_index: 0,
        char_offset: offset,
        locator_version: chapbook_core::LOCATOR_VERSION,
        quote: chapbook_core::Quote {
            prefix: "before ".into(),
            exact: String::new(),
            suffix: "after".into(),
        },
        spine_fraction: progression,
        book_progression: progression,
    }
}

/// A book with no catalog behind it has nowhere to sync, and asking is
/// not an error — it is most books.
#[test]
fn a_sideloaded_book_has_no_sync_targets_and_owes_nothing() {
    let (mut library, dir) = temp_library();
    let id = book_with_position(&mut library, &dir);

    assert_eq!(
        library.sync_targets(id).unwrap(),
        chapbook_library::SyncTargets::default()
    );
    // Dirty in itself — it has never synced — but not in the work list,
    // because there is no service to push it to.
    assert!(library.position_needs_push(id).unwrap());
    assert!(library.positions_needing_push().unwrap().is_empty());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn sync_targets_round_trip_and_can_be_cleared() {
    let (mut library, dir) = temp_library();
    let id = book_with_position(&mut library, &dir);

    library
        .set_sync_targets(
            id,
            Some("https://cat.example.com/opds/progression/abc"),
            Some("https://cat.example.com/annotations/?target=abc"),
        )
        .unwrap();
    let targets = library.sync_targets(id).unwrap();
    assert_eq!(
        targets.progression_url.as_deref(),
        Some("https://cat.example.com/opds/progression/abc")
    );
    assert_eq!(
        targets.annotation_container.as_deref(),
        Some("https://cat.example.com/annotations/?target=abc")
    );

    // A book that moved catalogs must stop talking to the old one.
    library.set_sync_targets(id, None, None).unwrap();
    let targets = library.sync_targets(id).unwrap();
    assert_eq!(targets.progression_url, None);
    assert_eq!(targets.annotation_container, None);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_position_is_dirty_until_it_is_marked_synced() {
    let (mut library, dir) = temp_library();
    let id = book_with_position(&mut library, &dir);
    library
        .set_sync_targets(id, Some("https://cat.example.com/p/abc"), None)
        .unwrap();

    let pending = library.positions_needing_push().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].book, id);
    assert_eq!(pending[0].progression_url, "https://cat.example.com/p/abc");

    library
        .mark_position_synced(id, pending[0].revision, "2026-08-30T12:00:00Z")
        .unwrap();
    assert!(!library.position_needs_push(id).unwrap());
    assert!(library.positions_needing_push().unwrap().is_empty());
    assert_eq!(
        library.sync_targets(id).unwrap().remote_modified.as_deref(),
        Some("2026-08-30T12:00:00Z")
    );

    // Reading on moves it again.
    library.set_position(id, &locator(500, 0.5)).unwrap();
    assert!(library.position_needs_push(id).unwrap());
    std::fs::remove_dir_all(&dir).ok();
}

/// The race the revision exists for. `updated_at` has one-second
/// resolution, so a page turn landing in the same second as the sync mark
/// would compare equal and look clean; a revision cannot.
#[test]
fn a_position_written_while_the_request_was_in_flight_stays_dirty() {
    let (mut library, dir) = temp_library();
    let id = book_with_position(&mut library, &dir);
    library
        .set_sync_targets(id, Some("https://cat.example.com/p/abc"), None)
        .unwrap();

    // The push takes the position as it stands.
    let pushed = library.positions_needing_push().unwrap().remove(0);

    // The reader turns a page before the response lands. Same second —
    // this whole test runs inside one.
    library.set_position(id, &locator(900, 0.9)).unwrap();

    // Now the response arrives and marks the revision that went out.
    library
        .mark_position_synced(id, pushed.revision, "2026-08-30T12:00:00Z")
        .unwrap();

    assert!(
        library.position_needs_push(id).unwrap(),
        "the position the service has never seen was marked clean"
    );
    let still = library.positions_needing_push().unwrap();
    assert_eq!(still.len(), 1);
    assert!(
        still[0].revision > pushed.revision,
        "the newer revision should be the one now owed"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn an_annotation_owes_the_container_a_write_until_it_is_marked() {
    let (mut library, dir) = temp_library();
    let path = sample_book(&dir, "marks.epub", b"marks");
    let id = library.import(&path, &FakeBook::new("Marks")).unwrap();
    let annotation = library
        .add_annotation(
            id,
            AnnotationKind::Highlight,
            &locator(10, 0.1),
            Some(&locator(20, 0.2)),
            None,
            Some("#ffcc00"),
        )
        .unwrap();

    let pending = library.annotations_needing_push(id).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, annotation);
    assert_eq!(pending[0].remote_iri, None, "never pushed");
    assert!(!pending[0].deleted);

    library
        .mark_annotation_synced(
            annotation,
            pending[0].revision,
            "https://cat.example.com/annotations/abc",
            Some("\"v1\""),
        )
        .unwrap();
    assert!(library.annotations_needing_push(id).unwrap().is_empty());
    assert_eq!(
        library
            .annotation_by_remote_iri("https://cat.example.com/annotations/abc")
            .unwrap(),
        Some(annotation)
    );

    // Editing it puts it back in the queue, with the tag to guard the PUT.
    library
        .set_annotation_color(annotation, Some("#00ccff"))
        .unwrap();
    let pending = library.annotations_needing_push(id).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].remote_etag.as_deref(), Some("\"v1\""));
    std::fs::remove_dir_all(&dir).ok();
}

/// Why deletes were soft from v1: the row has to outlive the reader's
/// action long enough to tell the server.
#[test]
fn a_deleted_annotation_still_owes_the_container_a_delete() {
    let (mut library, dir) = temp_library();
    let path = sample_book(&dir, "marks.epub", b"marks");
    let id = library.import(&path, &FakeBook::new("Marks")).unwrap();
    let annotation = library
        .add_annotation(
            id,
            AnnotationKind::Bookmark,
            &locator(10, 0.1),
            None,
            None,
            None,
        )
        .unwrap();
    let revision = library.annotations_needing_push(id).unwrap()[0].revision;
    library
        .mark_annotation_synced(
            annotation,
            revision,
            "https://cat.example.com/annotations/abc",
            None,
        )
        .unwrap();

    library.delete_annotation(annotation).unwrap();
    assert!(
        library.annotations(id).unwrap().is_empty(),
        "the reader should not see it any more"
    );
    let pending = library.annotations_needing_push(id).unwrap();
    assert_eq!(pending.len(), 1, "but the container has not been told");
    assert!(pending[0].deleted);
    assert_eq!(
        pending[0].remote_iri.as_deref(),
        Some("https://cat.example.com/annotations/abc")
    );

    // Once told, the row can go.
    library.purge_annotation(annotation).unwrap();
    assert!(library.annotations_needing_push(id).unwrap().is_empty());
    assert_eq!(
        library
            .annotation_by_remote_iri("https://cat.example.com/annotations/abc")
            .unwrap(),
        None
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// A mark deleted before it ever reached the container owes nothing —
/// there is nothing there to remove.
#[test]
fn an_annotation_deleted_before_it_synced_owes_nothing() {
    let (mut library, dir) = temp_library();
    let path = sample_book(&dir, "marks.epub", b"marks");
    let id = library.import(&path, &FakeBook::new("Marks")).unwrap();
    let annotation = library
        .add_annotation(
            id,
            AnnotationKind::Bookmark,
            &locator(10, 0.1),
            None,
            None,
            None,
        )
        .unwrap();
    library.delete_annotation(annotation).unwrap();
    assert!(library.annotations_needing_push(id).unwrap().is_empty());
    std::fs::remove_dir_all(&dir).ok();
}

/// `purge_annotation` is for rows the reader has already deleted. A live
/// mark must survive it, or a sync bug becomes data loss.
#[test]
fn purging_refuses_a_mark_the_reader_still_has() {
    let (mut library, dir) = temp_library();
    let path = sample_book(&dir, "marks.epub", b"marks");
    let id = library.import(&path, &FakeBook::new("Marks")).unwrap();
    let annotation = library
        .add_annotation(
            id,
            AnnotationKind::Highlight,
            &locator(10, 0.1),
            None,
            None,
            None,
        )
        .unwrap();

    library.purge_annotation(annotation).unwrap();
    assert_eq!(
        library.annotations(id).unwrap().len(),
        1,
        "a live annotation was purged"
    );
    std::fs::remove_dir_all(&dir).ok();
}

// ---- Browsing: search, collections, series, reading state ----

/// A shelf with enough in it that narrowing means something.
fn stocked_shelf() -> (Library, std::path::PathBuf) {
    let (mut lib, dir) = temp_library();
    for (file, title, authors, series, index) in [
        ("a.epub", "Jane Eyre", &["Charlotte Brontë"][..], None, None),
        ("b.epub", "Villette", &["Charlotte Brontë"][..], None, None),
        (
            "c.epub",
            "The Fellowship of the Ring",
            &["J. R. R. Tolkien"][..],
            Some("The Lord of the Rings"),
            Some(1.0),
        ),
        (
            "d.epub",
            "The Two Towers",
            &["J. R. R. Tolkien"][..],
            Some("The Lord of the Rings"),
            Some(2.0),
        ),
        (
            "e.epub",
            "The Hobbit",
            &["J. R. R. Tolkien"][..],
            Some("The Lord of the Rings"),
            None,
        ),
    ] {
        let mut book = FakeBook::new(title).by(authors);
        if let Some(series) = series {
            book = book.with_series(series, index);
        }
        lib.import(&sample_book(&dir, file, file.as_bytes()), &book)
            .unwrap();
    }
    (lib, dir)
}

fn titles(books: &[chapbook_library::BookRecord]) -> Vec<&str> {
    books.iter().map(|b| b.title.as_str()).collect()
}

/// The reason search is FTS5 and not a better `LIKE`: SQLite folds case
/// for ASCII only, so the substring version answered nothing to a reader
/// who could not type the diaeresis in their own author's name.
#[test]
fn a_search_matches_an_author_the_reader_cannot_spell() {
    let (lib, dir) = stocked_shelf();

    let found = lib.books(Some("bronte")).unwrap();
    assert_eq!(found.len(), 2, "{:?}", titles(&found));

    // And the other direction, for a reader who can.
    assert_eq!(lib.books(Some("Brontë")).unwrap().len(), 2);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_search_is_every_word_at_once_and_reaches_the_series() {
    let (lib, dir) = stocked_shelf();

    // Conjunctive: both words have to land, on any of the three fields.
    assert_eq!(
        titles(&lib.books(Some("tolk two")).unwrap()),
        ["The Two Towers"]
    );
    // Series is searchable even though no title says "Rings" but two do
    // — the third, The Hobbit, is reachable only through its series.
    let rings = lib.books(Some("lord rings hobbit")).unwrap();
    assert_eq!(titles(&rings), ["The Hobbit"]);
    // Nothing matches everything.
    assert!(lib.books(Some("tolkien bronte")).unwrap().is_empty());

    std::fs::remove_dir_all(&dir).ok();
}

/// A reader typing a real title must not be composing an FTS5 expression:
/// the hyphen in `Eighty-Four` is `NOT` in that language.
#[test]
fn punctuation_a_reader_types_is_text_and_not_syntax() {
    let (lib, dir) = stocked_shelf();

    // Would be "fellowship NOT ring" if the text went through raw, and
    // would match nothing.
    assert_eq!(
        titles(&lib.books(Some("Fellowship-Ring")).unwrap()),
        ["The Fellowship of the Ring"]
    );
    // Quotes and stars are query syntax; here they are typing.
    assert!(lib.books(Some("\"")).unwrap().is_empty());
    assert!(lib.books(Some("*")).unwrap().is_empty());
    // A search that says nothing narrows to nothing, rather than
    // silently returning the whole shelf as if it had been ignored.
    assert!(lib.books(Some("!!!")).unwrap().is_empty());
    // But an empty box is not a search at all.
    assert_eq!(lib.books(Some("  ")).unwrap().len(), 5);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_collection_holds_books_and_survives_being_renamed() {
    let (mut lib, dir) = stocked_shelf();
    let all = lib.books(None).unwrap();
    let hobbit = all.iter().find(|b| b.title == "The Hobbit").unwrap().id;
    let eyre = all.iter().find(|b| b.title == "Jane Eyre").unwrap().id;

    let shelf = lib.create_collection("To Reread").unwrap();
    // Idempotent on the name: a shell adding to a collection should not
    // have to ask whether it exists first.
    assert_eq!(lib.create_collection("To Reread").unwrap(), shelf);

    lib.add_to_collection(hobbit, shelf).unwrap();
    lib.add_to_collection(eyre, shelf).unwrap();
    lib.add_to_collection(eyre, shelf).unwrap();

    let listed = lib.collections().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "To Reread");
    assert_eq!(listed[0].books, 2, "adding twice added one");

    let members = lib
        .query(&chapbook_library::BookQuery {
            collection: Some(shelf),
            sort: chapbook_library::Sort::Title,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(titles(&members), ["Jane Eyre", "The Hobbit"]);
    // And a record knows what it is in, without a query per book.
    assert_eq!(members[0].collections.len(), 1);
    assert_eq!(members[0].collections[0].name, "To Reread");

    lib.rename_collection(shelf, "Favourites").unwrap();
    assert_eq!(lib.collections().unwrap()[0].name, "Favourites");

    lib.remove_from_collection(eyre, shelf).unwrap();
    assert_eq!(lib.collections().unwrap()[0].books, 1);

    std::fs::remove_dir_all(&dir).ok();
}

/// Deleting a collection takes the grouping, not the books — and frees
/// the name, because a reader who cannot see the old row should not be
/// told it owns the word.
#[test]
fn deleting_a_collection_keeps_the_books_and_frees_the_name() {
    let (mut lib, dir) = stocked_shelf();
    let hobbit = lib.books(Some("hobbit")).unwrap()[0].id;

    let first = lib.create_collection("Sci-Fi").unwrap();
    lib.add_to_collection(hobbit, first).unwrap();
    lib.delete_collection(first).unwrap();

    assert!(lib.collections().unwrap().is_empty());
    assert_eq!(lib.books(None).unwrap().len(), 5, "the books stayed");
    assert!(
        lib.book(hobbit).unwrap().unwrap().collections.is_empty(),
        "a deleted collection is not a collection the book is in"
    );

    let second = lib.create_collection("Sci-Fi").unwrap();
    assert_ne!(second, first, "a new row, not the dead one");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn series_group_and_sort_with_the_unplaced_volume_last() {
    let (lib, dir) = stocked_shelf();

    assert_eq!(
        lib.series().unwrap(),
        vec![("The Lord of the Rings".to_string(), 3)]
    );

    let ordered = lib
        .query(&chapbook_library::BookQuery {
            series: Some("the lord of the rings"),
            sort: chapbook_library::Sort::Series,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        titles(&ordered),
        [
            "The Fellowship of the Ring",
            "The Two Towers",
            // No group-position, so it sorts after everything numbered
            // rather than ahead of volume one.
            "The Hobbit"
        ]
    );

    // Books in no series come last in a whole-shelf series sort: they are
    // not a series called nothing.
    let whole = lib
        .query(&chapbook_library::BookQuery {
            sort: chapbook_library::Sort::Series,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(titles(&whole)[3..], ["Jane Eyre", "Villette"]);

    std::fs::remove_dir_all(&dir).ok();
}

/// Finished is stored, not derived, and the three states are exclusive.
#[test]
fn reading_state_separates_never_opened_from_finished_and_reopened() {
    let (mut lib, dir) = stocked_shelf();
    let all = lib.books(None).unwrap();
    let reading = all.iter().find(|b| b.title == "Villette").unwrap().id;
    let done = all.iter().find(|b| b.title == "Jane Eyre").unwrap().id;

    lib.set_position(reading, &locator_at(10)).unwrap();
    lib.set_position(done, &locator_at(10)).unwrap();
    lib.set_finished(done, true).unwrap();

    fn by_state(
        lib: &Library,
        state: chapbook_library::ReadingState,
    ) -> Vec<chapbook_library::BookRecord> {
        lib.query(&chapbook_library::BookQuery {
            state: Some(state),
            sort: chapbook_library::Sort::Title,
            ..Default::default()
        })
        .unwrap()
    }

    assert_eq!(
        titles(&by_state(&lib, chapbook_library::ReadingState::Finished)),
        ["Jane Eyre"]
    );
    assert_eq!(
        titles(&by_state(&lib, chapbook_library::ReadingState::Reading)),
        ["Villette"]
    );
    assert_eq!(
        by_state(&lib, chapbook_library::ReadingState::Unread).len(),
        3
    );

    // Reopening a finished book does not un-finish it — the case a shelf
    // deriving the state from progress gets wrong.
    lib.set_position(done, &locator_at(0)).unwrap();
    assert_eq!(
        lib.book(done).unwrap().unwrap().state(),
        chapbook_library::ReadingState::Finished
    );
    assert_eq!(
        titles(&by_state(&lib, chapbook_library::ReadingState::Reading)),
        ["Villette"]
    );

    // And finishing twice keeps the first answer to "when".
    let first = lib.book(done).unwrap().unwrap().finished_at.unwrap();
    lib.set_finished(done, true).unwrap();
    assert_eq!(lib.book(done).unwrap().unwrap().finished_at, Some(first));

    lib.set_finished(done, false).unwrap();
    assert_eq!(
        lib.book(done).unwrap().unwrap().state(),
        chapbook_library::ReadingState::Reading
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// The claim `delete_book` has made since it was written, now actually
/// tested: soft-deleting keeps the annotations *and* re-adding the file
/// gets them back. It did not — the fingerprint lookup could not see the
/// removed row and made a second one beside it.
#[test]
fn re_adding_a_removed_book_returns_the_same_record() {
    let (mut lib, dir) = temp_library();
    let path = sample_book(&dir, "returning.epub", b"bytes");
    let id = lib.import(&path, &FakeBook::new("Returning")).unwrap();
    lib.add_annotation(
        id,
        AnnotationKind::Bookmark,
        &locator_at(10),
        None,
        None,
        None,
    )
    .unwrap();

    lib.delete_book(id).unwrap();
    let again = lib.import(&path, &FakeBook::new("Returning")).unwrap();

    assert_eq!(again, id, "a second row would orphan the annotations");
    assert_eq!(lib.books(None).unwrap().len(), 1, "and not two rows");
    assert_eq!(lib.annotations(id).unwrap().len(), 1);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_long_shelf_pages() {
    let (lib, dir) = stocked_shelf();
    let page = |offset| {
        lib.query(&chapbook_library::BookQuery {
            sort: chapbook_library::Sort::Title,
            limit: Some(2),
            offset,
            ..Default::default()
        })
        .unwrap()
    };
    assert_eq!(
        titles(&page(0)),
        ["Jane Eyre", "The Fellowship of the Ring"]
    );
    assert_eq!(titles(&page(2)), ["The Hobbit", "The Two Towers"]);
    assert_eq!(titles(&page(4)), ["Villette"]);
    assert!(page(6).is_empty());

    std::fs::remove_dir_all(&dir).ok();
}

// ---- v8: what the application layer keeps beside the shelf ----

#[test]
fn a_grant_is_kept_by_fingerprint_and_forgotten() {
    let (mut library, dir) = temp_library();
    assert_eq!(library.grant("fp-1").unwrap(), None);
    library.set_grant("fp-1", b"content://provider/42").unwrap();
    assert_eq!(
        library.grant("fp-1").unwrap().as_deref(),
        Some(&b"content://provider/42"[..])
    );
    // A token is bytes, not text: a bookmark is binary and may hold NULs.
    library.set_grant("fp-1", &[0, 1, 2, 0, 255]).unwrap();
    assert_eq!(library.grant("fp-1").unwrap(), Some(vec![0, 1, 2, 0, 255]));
    library.clear_grant("fp-1").unwrap();
    assert_eq!(library.grant("fp-1").unwrap(), None);
    library.clear_grant("fp-1").unwrap();
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_preference_round_trips_and_clears() {
    let (mut library, dir) = temp_library();
    assert_eq!(library.preference("progress_label").unwrap(), None);
    library
        .set_preference("progress_label", "pages_left")
        .unwrap();
    assert_eq!(
        library.preference("progress_label").unwrap().as_deref(),
        Some("pages_left")
    );
    library.set_preference("progress_label", "percent").unwrap();
    assert_eq!(
        library.preference("progress_label").unwrap().as_deref(),
        Some("percent")
    );
    library.clear_preference("progress_label").unwrap();
    assert_eq!(library.preference("progress_label").unwrap(), None);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_catalog_is_listed_in_order_renamed_and_removed() {
    let (mut library, dir) = temp_library();
    let a = library
        .add_opds_source("https://a.test/opds/", None, None)
        .unwrap();
    let b = library
        .add_opds_source("https://b.test/opds/", Some("B"), None)
        .unwrap();
    let urls: Vec<String> = library
        .opds_sources()
        .unwrap()
        .into_iter()
        .map(|s| s.url)
        .collect();
    assert_eq!(urls, ["https://a.test/opds/", "https://b.test/opds/"]);
    assert!(library.rename_opds_source(a, Some("A")).unwrap());
    assert_eq!(
        library.opds_source(a).unwrap().unwrap().title.as_deref(),
        Some("A")
    );
    assert!(library.remove_opds_source(b).unwrap());
    assert!(!library.remove_opds_source(b).unwrap(), "already gone");
    assert!(library.opds_source(b).unwrap().is_none());
    let ids: Vec<i64> = library
        .opds_sources()
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect();
    assert_eq!(ids, [a]);
    let _ = std::fs::remove_dir_all(dir);
}
