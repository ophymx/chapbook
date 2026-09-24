//! The application model, driven the way a front end drives it.
//!
//! Every test gets its own library directory through `App::open`'s
//! explicit argument, so unlike the session tests there is no process
//! environment to serialize over. The platform is a phone's shape — the
//! fixture faces embedded, an in-memory credential store, no bundled
//! transport — because that is the profile the desktop crate used to be
//! unable to run in, and the reason this layer exists.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chapbook_app::chapbook_library::{Library, ReadingState};
use chapbook_app::chapbook_opds::progression::Device;
use chapbook_app::chapbook_opds::{HttpClient, HttpError, HttpRequest, HttpResponse};
use chapbook_app::chapbook_reader::chapbook_core::{
    Action, EdgeSizes, FontSource, MemoryCredentials, PageMetrics, Rotation, Size, Source,
};
use chapbook_app::chapbook_reader::Session;
use chapbook_app::chapbook_sync::{PositionReport, SyncEngine};
use chapbook_app::reader::{self, Place, SearchWalk};
use chapbook_app::{
    App, Opened, Platform, ProgressLabel, ShelfFilter, SyncDriver, SyncRequest, SyncStatus,
};

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let dir = std::env::temp_dir().join(format!("chapbook-app-{tag}-{}", std::process::id()));
        if dir.exists() {
            let _ = std::fs::remove_dir_all(&dir);
        }
        std::fs::create_dir_all(&dir).expect("create test dir");
        TempDir(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/epub")
        .join(name)
}

/// A phone's platform: embedded faces, a store that remembers, no
/// bundled transport.
fn platform() -> Platform {
    Platform::new(FontSource::embedded(fixture("../fonts"), "Crimson Text"))
        .with_credentials(Arc::new(MemoryCredentials::new()))
        .with_device_name("test phone")
}

fn open_app(dir: &TempDir) -> App {
    App::open(dir.path(), platform()).expect("open app")
}

fn metrics() -> PageMetrics {
    PageMetrics {
        size: Size::new(400.0, 600.0),
        margins: EdgeSizes::uniform(40.0),
        dpi_scale: 1.0,
        rotation: Rotation::None,
    }
}

/// Lay the session out so navigation and position capture have pages to
/// work with.
fn settle(session: &mut Session) {
    session.set_metrics(metrics());
    let _ = session.render();
}

/// The session a library-owned book opens as; anything else is a test
/// failure.
fn session_of(opened: Opened) -> Session {
    match opened {
        Opened::Session(session) => *session,
        Opened::Adopted { .. } => panic!("an imported book is not adopted"),
        Opened::Missing => panic!("an imported book is not missing"),
    }
}

#[test]
fn an_imported_book_is_on_the_shelf_with_its_series() {
    let dir = TempDir::new("import");
    let mut app = open_app(&dir);
    let record = app.import(&fixture("series.epub")).expect("import");
    assert!(!record.title.is_empty());
    assert!(record.series.is_some(), "series.epub declares a series");

    let shelf = app.shelf(&ShelfFilter::default()).expect("shelf");
    assert_eq!(shelf.len(), 1);
    assert_eq!(shelf[0].id, record.id);
}

#[test]
fn search_narrows_and_a_miss_is_empty() {
    let dir = TempDir::new("search");
    let mut app = open_app(&dir);
    let series = app.import(&fixture("series.epub")).expect("import series");
    app.import(&fixture("minimal.epub"))
        .expect("import minimal");

    let hit = app
        .shelf(&ShelfFilter {
            search: series.title.clone(),
            ..ShelfFilter::default()
        })
        .expect("narrowed shelf");
    assert_eq!(hit.len(), 1);
    assert_eq!(hit[0].id, series.id);

    let miss = app
        .shelf(&ShelfFilter {
            search: "no-such-book-zzz".into(),
            ..ShelfFilter::default()
        })
        .expect("missed shelf");
    assert!(miss.is_empty());
}

#[test]
fn a_reopened_book_is_where_the_reader_left_it() {
    let dir = TempDir::new("reopen");
    let mut app = open_app(&dir);
    let record = app.import(&fixture("long.epub")).expect("import");

    let mut session = session_of(app.open_book(record.id).expect("open"));
    settle(&mut session);
    assert!(session.apply(Action::NextPage).needs_redraw());
    let left_at = session.position();
    assert_ne!((left_at.spine, left_at.page), (0, 0), "the page turned");
    session.save_position();
    drop(session);

    let mut session = session_of(app.open_book(record.id).expect("reopen"));
    settle(&mut session);
    let restored = session.position();
    assert_eq!(
        (restored.spine, restored.page),
        (left_at.spine, left_at.page)
    );
}

#[test]
fn finished_shows_under_its_own_filter() {
    let dir = TempDir::new("finished");
    let mut app = open_app(&dir);
    let record = app.import(&fixture("minimal.epub")).expect("import");
    app.set_finished(record.id, true).expect("finish");

    let finished = app
        .shelf(&ShelfFilter {
            state: Some(ReadingState::Finished),
            ..ShelfFilter::default()
        })
        .expect("finished shelf");
    assert_eq!(finished.len(), 1);

    let unread = app
        .shelf(&ShelfFilter {
            state: Some(ReadingState::Unread),
            ..ShelfFilter::default()
        })
        .expect("unread shelf");
    assert!(unread.is_empty());
}

#[test]
fn sync_with_nothing_syncable_declines_without_a_driver() {
    let dir = TempDir::new("nosync");
    let mut app = open_app(&dir);
    app.import(&fixture("minimal.epub")).expect("import");
    let started = app.sync_all(Arc::new(|| {})).expect("sync_all");
    assert!(!started, "a sideloaded shelf has nothing to sync");
    assert!(app.sync_events().is_empty());
}

/// A transport with nobody on the other end.
struct DeadHttp;

impl HttpClient for DeadHttp {
    fn get(&self, _request: HttpRequest) -> Result<HttpResponse, HttpError> {
        Err(HttpError::new("nobody home"))
    }
}

#[test]
fn the_driver_reports_an_unreachable_service_per_book() {
    let dir = TempDir::new("driver");
    let mut app = open_app(&dir);
    let record = app.import(&fixture("minimal.epub")).expect("import");

    // Move and save, so the position owes a push and the sync genuinely
    // has to reach the (dead) service.
    let mut session = session_of(app.open_book(record.id).expect("open"));
    settle(&mut session);
    session.apply(Action::NextPage);
    session.save_position();
    drop(session);

    let mut library = Library::open(app.dir()).expect("second connection");
    library
        .set_sync_targets(record.id, Some("http://127.0.0.1:1/progression"), None)
        .expect("targets");

    let engine = SyncEngine::new(
        Library::open(app.dir()).expect("engine connection"),
        Arc::new(DeadHttp),
        Device {
            id: "test-device".into(),
            name: "test".into(),
        },
    );
    let driver = SyncDriver::spawn(engine, Arc::new(MemoryCredentials::new()), Arc::new(|| {}));
    assert!(driver.request(SyncRequest::All));

    let first = driver.next_status().expect("a report");
    match first {
        SyncStatus::Book(report) => {
            assert_eq!(report.book, record.id);
            assert!(
                matches!(report.position, PositionReport::Failed(_)),
                "a dead transport fails the position half, got {:?}",
                report.position
            );
        }
        other => panic!("expected a book report, got {other:?}"),
    }
    match driver.next_status().expect("a finish") {
        SyncStatus::Finished { books } => assert_eq!(books, 1),
        other => panic!("expected the batch to finish, got {other:?}"),
    }
}

#[test]
fn removing_a_book_empties_the_shelf() {
    let dir = TempDir::new("remove");
    let mut app = open_app(&dir);
    let record = app.import(&fixture("minimal.epub")).expect("import");
    app.remove(record.id).expect("remove");
    assert!(app
        .shelf(&ShelfFilter::default())
        .expect("shelf")
        .is_empty());
    assert!(app.book(record.id).expect("lookup").is_none());
}

#[test]
fn the_state_line_matches_the_shelf() {
    let dir = TempDir::new("state");
    let mut app = open_app(&dir);
    let record = app.import(&fixture("minimal.epub")).expect("import");
    assert_eq!(chapbook_app::describe_state(&record), "unread");
    app.set_finished(record.id, true).expect("finish");
    let record = app.book(record.id).expect("lookup").expect("still there");
    assert_eq!(chapbook_app::describe_state(&record), "finished");
}

#[test]
fn a_flattened_toc_keeps_reading_order_and_depth() {
    let dir = TempDir::new("toc");
    let mut app = open_app(&dir);
    let record = app.import(&fixture("long.epub")).expect("import");
    let mut session = session_of(app.open_book(record.id).expect("open"));
    settle(&mut session);

    let flat = chapbook_app::flatten_toc(session.toc());
    assert!(!flat.is_empty(), "long.epub has contents");
    assert_eq!(flat[0].0, 0, "the first entry is top level");
    // Every entry the tree holds appears exactly once.
    fn count(entries: &[chapbook_app::chapbook_reader::chapbook_core::TocEntry]) -> usize {
        entries.iter().map(|e| 1 + count(&e.children)).sum()
    }
    assert_eq!(flat.len(), count(session.toc()));
}

#[test]
fn a_mark_describes_itself_for_a_list() {
    let dir = TempDir::new("describe-mark");
    let mut app = open_app(&dir);
    let record = app.import(&fixture("minimal.epub")).expect("import");
    let mut session = session_of(app.open_book(record.id).expect("open"));
    settle(&mut session);

    // A bookmark carries no text, so its line is where it sits.
    session.add_bookmark().expect("bookmarked");
    let marks = session.annotations();
    assert_eq!(marks.len(), 1);
    let line = chapbook_app::describe_annotation(&marks[0]);
    assert!(line.starts_with("bookmark"), "{line:?}");
    assert!(line.contains('%'), "a mark says where it sits: {line:?}");

    // A highlight quotes what it covers.
    session.select_range(0, 12);
    assert!(session.selected_range().is_some(), "an exact range selects");
    session.add_highlight().expect("highlighted");
    let marks = session.annotations();
    let highlight = marks
        .iter()
        .find(|m| {
            matches!(
                m.kind,
                chapbook_app::chapbook_library::AnnotationKind::Highlight
            )
        })
        .expect("the highlight is listed");
    let line = chapbook_app::describe_annotation(highlight);
    assert!(line.starts_with("highlight"), "{line:?}");
    assert!(line.contains('\u{201c}'), "a highlight quotes: {line:?}");
}

// ---- Custody ----

#[test]
fn an_adopted_book_keeps_no_copy_and_comes_back_as_its_grant() {
    let dir = TempDir::new("adopt");
    let mut app = open_app(&dir);
    // The platform owns the file; the app hands over a handle and the
    // token that will reach the file again — here a path, on a phone a
    // content URI or a bookmark. The token is opaque bytes to the layer.
    let file = std::fs::File::open(fixture("minimal.epub")).expect("fixture");
    let grant = b"content://provider/document/42";
    let id = app.adopt(Source::reader(file), grant).expect("adopt");

    let record = app.book(id).expect("lookup").expect("on the shelf");
    assert!(
        record.file_path.as_os_str().is_empty(),
        "the library keeps no copy of an adopted book"
    );
    assert_eq!(
        app.grant(&record.fingerprint).expect("grant"),
        Some(grant.to_vec())
    );

    match app.open_book(id).expect("open") {
        Opened::Adopted {
            fingerprint,
            grant: token,
        } => {
            assert_eq!(fingerprint, record.fingerprint);
            assert_eq!(token, grant.to_vec());
        }
        Opened::Session(_) => panic!("nothing to open by path"),
        Opened::Missing => panic!("the grant was remembered"),
    }

    // The same bytes adopted twice are one row, and importing the same
    // file resolves to it too.
    let again = std::fs::File::open(fixture("minimal.epub")).expect("fixture");
    assert_eq!(
        app.adopt(Source::reader(again), b"other")
            .expect("adopt again"),
        id
    );
    assert_eq!(app.shelf(&ShelfFilter::default()).expect("shelf").len(), 1);

    // A forgotten grant is a book out of reach, not a crash.
    app.forget_grant(&record.fingerprint).expect("forget");
    assert!(matches!(app.open_book(id).expect("open"), Opened::Missing));
}

#[test]
fn a_copy_that_left_the_disk_is_missing_not_an_error() {
    let dir = TempDir::new("missing");
    let mut app = open_app(&dir);
    let record = app.import(&fixture("minimal.epub")).expect("import");
    std::fs::remove_file(&record.file_path).expect("take the copy away");
    assert!(matches!(
        app.open_book(record.id).expect("open"),
        Opened::Missing
    ));
}

#[test]
fn junk_is_refused_and_the_shelf_stays_clean() {
    let dir = TempDir::new("junk");
    let mut app = open_app(&dir);
    let junk = dir.path().join("notes.txt");
    std::fs::write(&junk, "not a book").expect("write junk");
    assert!(app.import(&junk).is_err());
    assert!(app
        .adopt(Source::bytes(b"not a book".to_vec()), b"grant")
        .is_err());
    assert!(app
        .shelf(&ShelfFilter::default())
        .expect("shelf")
        .is_empty());
}

#[test]
fn the_session_config_is_the_apps_own() {
    let dir = TempDir::new("config");
    let app = open_app(&dir);
    let config = app.session_config();
    assert_eq!(config.library_dir.as_deref(), Some(dir.path()));
    assert!(
        config.transport.is_none(),
        "no bundled transport on a phone"
    );
}

// ---- Preferences ----

#[test]
fn the_progress_readout_is_a_preference_that_survives_a_relaunch() {
    let dir = TempDir::new("prefs");
    let mut app = open_app(&dir);
    assert_eq!(app.progress_label(), ProgressLabel::Percent);
    app.set_progress_label(ProgressLabel::PagesLeft)
        .expect("set");
    assert_eq!(app.progress_label(), ProgressLabel::PagesLeft);
    drop(app);
    let again = open_app(&dir);
    assert_eq!(again.progress_label(), ProgressLabel::PagesLeft);
}

// ---- Catalogs ----

#[test]
fn saved_catalogs_are_kept_in_order_renamed_and_removed() {
    let dir = TempDir::new("catalogs");
    let mut app = open_app(&dir);
    assert!(app.catalogs().expect("catalogs").is_empty());
    let a = app
        .add_catalog(" https://a.test/opds/ ", "")
        .expect("add a");
    let b = app.add_catalog("https://b.test/opds/", "B").expect("add b");
    let urls: Vec<String> = app
        .catalogs()
        .expect("catalogs")
        .into_iter()
        .map(|c| c.url)
        .collect();
    assert_eq!(urls, ["https://a.test/opds/", "https://b.test/opds/"]);
    assert!(app.rename_catalog(a.id, "A").expect("rename"));
    assert_eq!(app.catalog(a.id).expect("get").unwrap().title, "A");
    assert!(app.remove_catalog(b.id).expect("remove"));
    let ids: Vec<i64> = app
        .catalogs()
        .expect("catalogs")
        .into_iter()
        .map(|c| c.id)
        .collect();
    assert_eq!(ids, [a.id]);
    assert!(app.catalog(b.id).expect("get").is_none());
}

/// Only a build without the bundled transport can prove this; with it, a
/// missing platform transport falls through to `ureq`.
#[cfg(not(feature = "bundled-http"))]
#[test]
fn a_phone_with_no_transport_cannot_browse_and_says_so() {
    let dir = TempDir::new("no-transport");
    let app = open_app(&dir);
    let saved = chapbook_app::SavedCatalog {
        id: 1,
        title: String::new(),
        url: "https://x.test/".into(),
    };
    assert!(app.browse(&saved).is_err());
}

// ---- Downloads ----

#[test]
fn a_landed_download_is_shelved_with_its_services() {
    let dir = TempDir::new("land");
    let mut app = open_app(&dir);
    // The platform's transfer produced a file wherever it likes; the
    // landing imports it and records what the entry said.
    let landed = dir.path().join("download-1");
    std::fs::copy(fixture("minimal.epub"), &landed).expect("stage");
    let id = app
        .land_download(&landed, Some("https://x.test/progress/1"), None)
        .expect("land");
    assert!(landed.exists(), "the file is the platform's to remove");
    let targets = app.library().sync_targets(id).expect("targets");
    assert_eq!(
        targets.progression_url.as_deref(),
        Some("https://x.test/progress/1")
    );
    // Landing the same bytes again is the same row: a retried job needs
    // no bookkeeping.
    assert_eq!(app.land_download(&landed, None, None).expect("again"), id);
    assert_eq!(app.shelf(&ShelfFilter::default()).expect("shelf").len(), 1);
}

// ---- The reader's policy ----

#[test]
fn the_place_is_spine_weighted_and_moves_with_the_reader() {
    let dir = TempDir::new("place");
    let mut app = open_app(&dir);
    let record = app.import(&fixture("long.epub")).expect("import");
    let mut session = session_of(app.open_book(record.id).expect("open"));
    settle(&mut session);

    let start = Place::of(&mut session);
    assert_eq!(start.title, session.title());
    assert_eq!(start.book_fraction, 0.0);
    assert!(start.spine_len > 1 && start.page_count > 0);
    assert!(!start.can_go_back);

    assert!(session.apply(Action::NextUnit).needs_redraw());
    let moved = Place::of(&mut session);
    assert!((moved.book_fraction - 1.0 / start.spine_len as f64).abs() < 0.001);
    assert!(moved.book_fraction <= 1.0);
    assert_eq!(moved.pages_left(), moved.page_count - 1);
}

#[test]
fn a_memory_warning_halves_the_budget_to_a_floor() {
    let dir = TempDir::new("memory");
    let mut app = open_app(&dir);
    let record = app.import(&fixture("minimal.epub")).expect("import");
    let mut session = session_of(app.open_book(record.id).expect("open"));
    session.set_cache_budget(reader::cache_budget_for(256 << 20));
    assert_eq!(session.cache_budget(), 64 << 20);
    assert_eq!(reader::after_memory_warning(&mut session), 32 << 20);
    session.set_cache_budget(5 << 20);
    assert_eq!(
        reader::after_memory_warning(&mut session),
        reader::MIN_CACHE_BUDGET_UNDER_PRESSURE
    );
}

#[test]
fn a_search_walks_the_book_unit_by_unit_and_a_hit_can_be_shown() {
    let dir = TempDir::new("search");
    let mut app = open_app(&dir);
    let record = app.import(&fixture("long.epub")).expect("import");
    let mut session = session_of(app.open_book(record.id).expect("open"));
    settle(&mut session);

    // A word the page actually shows, so the hit is not a guess.
    let page = session.speakable_page().expect("a page of text");
    let word = page
        .text
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
        .find(|w| w.chars().count() >= 4)
        .expect("a word")
        .to_string();

    let mut walk = SearchWalk::new(&word).expect("a query");
    let mut steps = 0;
    while walk.step(&mut session) {
        steps += 1;
    }
    assert!(walk.is_done());
    assert_eq!(
        steps,
        session.spine_len() - 1,
        "one step per unit, the last reports done"
    );
    assert!(!walk.hits().is_empty(), "found {word}");
    let hit = walk.hits()[0].clone();
    assert!(hit.context.to_lowercase().contains(&word.to_lowercase()));

    reader::show_hit(&mut session, &hit);
    assert_eq!(
        session.selected_range(),
        Some((hit.locator.char_offset, hit.end))
    );
    assert_eq!(
        session
            .selected_text()
            .unwrap_or_default()
            .trim()
            .to_lowercase(),
        word.to_lowercase()
    );
}

#[test]
fn a_selection_becomes_a_highlight_and_the_selection_goes() {
    let dir = TempDir::new("highlight");
    let mut app = open_app(&dir);
    let record = app.import(&fixture("minimal.epub")).expect("import");
    let mut session = session_of(app.open_book(record.id).expect("open"));
    settle(&mut session);

    assert_eq!(
        reader::highlight_selection(&mut session),
        None,
        "nothing selected"
    );
    session.select_range(0, 12);
    let id = reader::highlight_selection(&mut session).expect("highlighted");
    assert_eq!(session.selected_range(), None);
    assert_eq!(session.annotations().len(), 1);
    assert_eq!(session.annotations()[0].id, id);

    session.select_range(0, 12);
    let note = reader::note_on_selection(&mut session, "a thought").expect("noted");
    assert_ne!(note, id);
    assert_eq!(session.selected_range(), None);
    assert_eq!(session.annotations().len(), 2);
}

#[test]
fn what_is_read_once_at_open_is_the_books_shape() {
    let dir = TempDir::new("reading");
    let mut app = open_app(&dir);
    let record = app.import(&fixture("long.epub")).expect("import");
    let session = session_of(app.open_book(record.id).expect("open"));
    let reading = reader::Reading::of(&session);
    assert_eq!(
        reading.kind,
        chapbook_app::chapbook_reader::chapbook_core::BookKind::Epub
    );
    assert!(!reading.contents.is_empty());
    assert!(reading.font_families.iter().any(|f| f == "Crimson Text"));
}

// ---- The desktop ----

#[cfg(feature = "desktop")]
#[test]
fn the_desktop_platform_is_the_old_defaults() {
    let dir = TempDir::new("desktop");
    let app = App::desktop(Some(dir.path())).expect("open desktop app");
    assert_eq!(app.dir(), dir.path());
    assert!(
        app.session_config().transport.is_none(),
        "a desktop session falls through to the bundled transport"
    );
    assert_eq!(app.platform().device_name, "chapbook-app");
}
