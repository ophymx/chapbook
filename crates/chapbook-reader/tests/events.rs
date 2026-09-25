//! What a shell reacts to that is not a repaint.
//!
//! `FrameIntent` already says what to redraw. These are the other half —
//! a page that will never arrive, the reader moving somewhere the shell
//! did not send them, the end of the book — and until `drain_events`
//! existed a shell could see none of them. The failure case is the one
//! that mattered: a comic page that failed to download was recorded
//! internally so it would not be retried, and stayed a placeholder
//! forever with nothing able to say why.

mod common;
use chapbook_core::{EdgeSizes, PageMetrics, Rotation, Size};
use chapbook_reader::{Session, SessionConfig, SessionEvent};
use common::*;

fn dir_for(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "chapbook-events-test-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn open(name: &str, source: &str) -> Session {
    let mut session = Session::open_with(
        source,
        SessionConfig::new(fixture_fonts()).with_library_dir(dir_for(name)),
    )
    .unwrap();
    session.set_metrics(PageMetrics {
        size: Size::new(400.0, 600.0),
        margins: EdgeSizes::uniform(0.0),
        dpi_scale: 1.0,
        rotation: Rotation::None,
    });
    session
}

/// Drive an image book's async load to completion.
fn settle(session: &mut Session) {
    for _ in 0..200 {
        session.render();
        if !session.has_pending_loads() {
            session.poll_loaded();
            session.render();
            return;
        }
        session.poll_loaded();
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    panic!("unit never finished loading");
}

/// Opening is not an event. A shell drains on its first tick and should
/// not be told the reader "moved" to where the book already was.
#[test]
fn an_untouched_session_has_nothing_to_report() {
    let mut session = open("quiet", &fixture("epub/minimal.epub"));
    session.render();
    assert_eq!(session.drain_events(), Vec::new());
}

/// The position is derived at drain time rather than recorded at each of
/// the eleven sites that move it, so this is really asking whether the
/// derivation notices an ordinary turn.
#[test]
fn turning_a_page_reports_where_the_reader_ended_up() {
    let mut session = open("moved", &fixture("epub/long.epub"));
    session.render();
    let _ = session.drain_events();

    assert!(session.next_page(), "the fixture should have a second page");
    let after = session.position();
    assert_eq!(
        session.drain_events(),
        vec![SessionEvent::PositionChanged {
            spine: after.spine,
            page: after.page,
        }]
    );
}

/// Draining twice does not report the same move twice.
#[test]
fn a_position_is_reported_once() {
    let mut session = open("once", &fixture("epub/long.epub"));
    session.render();
    let _ = session.drain_events();
    session.next_page();
    assert_eq!(session.drain_events().len(), 1);
    assert_eq!(session.drain_events(), Vec::new());
}

/// Ten turns between drains are one move, not ten. A shell asked where
/// the reader is, not for a transcript of how they got there — and a sync
/// client pushing each intermediate position would be worse than useless.
#[test]
fn many_turns_between_drains_coalesce() {
    let mut session = open("coalesce", &fixture("epub/long.epub"));
    session.render();
    let _ = session.drain_events();

    let mut turns = 0;
    for _ in 0..10 {
        if session.next_page() {
            turns += 1;
        }
    }
    assert!(turns > 1, "the fixture should paginate to several pages");

    let events = session.drain_events();
    let at = session.position();
    assert_eq!(
        events,
        vec![SessionEvent::PositionChanged {
            spine: at.spine,
            page: at.page,
        }],
        "{turns} turns should report one position, not {}",
        events.len()
    );
}

/// Reaching the end fires once, and re-arms if the reader leaves and
/// comes back — "mark as read" should not fire on every drain while the
/// reader sits on the last page.
#[test]
fn finishing_the_book_fires_on_the_transition() {
    let mut session = open("finish", &fixture("epub/minimal.epub"));
    session.render();
    let _ = session.drain_events();

    // Walk to the end.
    for _ in 0..500 {
        if !session.next_page() {
            break;
        }
    }
    session.render();
    let events = session.drain_events();
    assert!(
        events.contains(&SessionEvent::BookFinished),
        "reaching the last page of the last unit should report it: {events:?}"
    );

    // Sitting there is not finishing it again.
    assert_eq!(session.drain_events(), Vec::new());

    // Leaving and returning re-arms.
    assert!(
        session.prev_page(),
        "the fixture should have a page to go back to"
    );
    session.render();
    let _ = session.drain_events();
    assert!(session.next_page());
    session.render();
    assert!(session.drain_events().contains(&SessionEvent::BookFinished));
}

/// A comic page decoding on the loader thread is a discrete fact, so it
/// is queued when it happens rather than derived.
#[test]
fn a_loaded_unit_is_reported_with_its_spine() {
    let mut session = open("loaded", &fixture("cbz/minimal.cbz"));
    settle(&mut session);
    let events = session.drain_events();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, SessionEvent::UnitLoaded { spine: 0 })),
        "the first comic page should report itself: {events:?}"
    );
}

/// The gap this whole type was built for.
///
/// A page that cannot be fetched is recorded internally so a retry is not
/// queued every frame — and that was the end of it. The reader saw a
/// placeholder that never resolved and the shell had nothing to say.
///
/// A streamed comic whose pages 404 is the real shape of it: the catalog
/// opens, the book is three pages long, and none of them will ever
/// arrive. The message is for a person and is free to change; that it
/// arrives at all is the contract.
#[cfg(all(feature = "opds", feature = "cbz"))]
#[test]
fn a_failed_unit_reaches_the_shell_instead_of_only_the_log() {
    use chapbook_reader::chapbook_opds::http::{header, Response};
    use chapbook_reader::{Body, HttpClient, HttpError, HttpRequest, HttpResponse};
    use std::io::Cursor;
    use std::sync::Arc;

    const HOST: &str = "https://comics.example.com";

    /// Serves the catalog and refuses every page.
    struct PagesRefused;

    impl HttpClient for PagesRefused {
        fn send(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
            if request.uri().path().contains("/pages") {
                return Err(HttpError::new("the page shed is locked"));
            }
            let feed = format!(
                r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom" xmlns:pse="http://vaemendis.net/opds-pse/ns">
  <id>urn:cat:comics</id><title>Comics</title>
  <entry><id>urn:c1</id><title>Unreachable Comic</title>
    <link rel="http://vaemendis.net/opds-pse/stream"
          href="{HOST}/pages?page={{pageNumber}}&amp;width={{maxWidth}}"
          type="image/png" pse:count="3"/>
  </entry>
</feed>"#
            );
            Ok(Response::builder()
                .status(200)
                .header(header::CONTENT_TYPE, "application/atom+xml")
                .body(Box::new(Cursor::new(feed.into_bytes())) as Body)
                .expect("a well-formed canned response"))
        }
    }

    let mut session = Session::open_with(
        format!("{HOST}/opds/"),
        SessionConfig::new(fixture_fonts())
            .with_library_dir(dir_for("failed"))
            .with_transport(Arc::new(PagesRefused)),
    )
    .expect("the catalog itself is reachable");
    session.set_metrics(PageMetrics {
        size: Size::new(400.0, 600.0),
        margins: EdgeSizes::uniform(0.0),
        dpi_scale: 1.0,
        rotation: Rotation::None,
    });
    settle(&mut session);

    let events = session.drain_events();
    let failed: Vec<&SessionEvent> = events
        .iter()
        .filter(|e| matches!(e, SessionEvent::UnitFailed { .. }))
        .collect();
    assert!(
        !failed.is_empty(),
        "a page that will never arrive reported nothing: {events:?}"
    );
    let Some(SessionEvent::UnitFailed { message, .. }) = failed.first().copied() else {
        unreachable!()
    };
    assert!(!message.trim().is_empty(), "the failure said nothing");
}

/// A shell that installs no wakeup and never drains must not grow the
/// queue without limit while a comic prefetches its way through a book.
#[test]
fn the_queue_keeps_one_event_per_subject() {
    let mut session = open("bounded", &fixture("cbz/minimal.cbz"));
    settle(&mut session);
    // Walk the whole book without ever draining.
    for _ in 0..50 {
        if !session.next_page() {
            break;
        }
        settle(&mut session);
    }
    let events = session.drain_events();
    let loads: Vec<usize> = events
        .iter()
        .filter_map(|e| match e {
            SessionEvent::UnitLoaded { spine } => Some(*spine),
            _ => None,
        })
        .collect();
    let mut unique = loads.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(
        loads.len(),
        unique.len(),
        "the same unit was queued more than once: {loads:?}"
    );
    assert!(
        events.len() <= 2 * session.spine_len() + 2,
        "the queue outgrew its bound: {} events for {} spine entries",
        events.len(),
        session.spine_len()
    );
}

/// The event says the book was finished; the library is what remembers
/// it. A shell that never drains events still gets a shelf that knows.
#[test]
fn finishing_the_book_is_recorded_on_the_shelf() {
    // `dir_for` clears the directory, so it is called once and the path
    // kept: asking again after opening would delete the library.
    let dir = dir_for("finish-recorded");
    let mut session = Session::open_with(
        fixture("epub/minimal.epub").as_str(),
        SessionConfig::new(fixture_fonts()).with_library_dir(&dir),
    )
    .unwrap();
    session.set_metrics(PageMetrics {
        size: Size::new(400.0, 600.0),
        margins: EdgeSizes::uniform(0.0),
        dpi_scale: 1.0,
        rotation: Rotation::None,
    });
    session.render();
    let id = session.book_id().expect("a local book reaches the library");

    // Saved part-way through, and not finished: the mark has to come
    // from where the reader is, not from having saved at all.
    session.save_position();
    let library = chapbook_library::Library::open(&dir).unwrap();
    assert_eq!(
        library.book(id).unwrap().unwrap().state(),
        chapbook_library::ReadingState::Reading
    );
    drop(library);

    for _ in 0..500 {
        if !session.next_page() {
            break;
        }
    }
    session.render();
    session.save_position();

    let library = chapbook_library::Library::open(&dir).unwrap();
    assert_eq!(
        library.book(id).unwrap().unwrap().state(),
        chapbook_library::ReadingState::Finished
    );
}
