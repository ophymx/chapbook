//! The reconcile rules, against a scripted transport and a real library.
//!
//! What is worth pinning here is not that a PUT goes out — the mapping
//! crates test that — but who wins when two devices disagree, and what
//! happens to a mark the container refuses.

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::{Arc, Mutex};

use chapbook_core::{LayeredLocator, Quote, LOCATOR_VERSION};
use chapbook_library::{AnnotationKind, BookId, Library};
use chapbook_opds::http::{
    header, Body, HttpClient, HttpError, HttpRequest, HttpResponse, Response,
};
use chapbook_opds::progression::Device;
use chapbook_sync::{PositionReport, SyncEngine, SyncError};
use serde_json::json;

const HOST: &str = "https://library.example.com";
const PROGRESSION: &str = "https://library.example.com/opds/progression/book";
const CONTAINER: &str = "https://library.example.com/annotations/";

// ---- a scripted server ----

type Canned = (u16, Vec<(String, String)>, String);

#[derive(Clone, Default)]
struct FakeHttp(Arc<Inner>);

#[derive(Default)]
struct Inner {
    routes: Mutex<HashMap<String, Canned>>,
    /// Responses consumed one per request, ahead of any standing route.
    /// A merge attempts the same write twice, so the two attempts have to
    /// be able to answer differently.
    queued: Mutex<HashMap<String, Vec<Canned>>>,
    seen: Mutex<Vec<(String, String, String)>>,
}

impl FakeHttp {
    fn on(self, method: &str, path: &str, status: u16, body: &str) -> Self {
        self.route(method, path, status, &[], body)
    }

    /// Queue one response, taken before any standing route for the same
    /// request and only once. Calls stack in order.
    fn once(
        self,
        method: &str,
        path: &str,
        status: u16,
        headers: &[(&str, &str)],
        body: &str,
    ) -> Self {
        self.0
            .queued
            .lock()
            .unwrap()
            .entry(format!("{method} {path}"))
            .or_default()
            .push((
                status,
                headers
                    .iter()
                    .map(|(n, v)| (n.to_string(), v.to_string()))
                    .collect(),
                body.to_string(),
            ));
        self
    }

    fn route(
        self,
        method: &str,
        path: &str,
        status: u16,
        headers: &[(&str, &str)],
        body: &str,
    ) -> Self {
        self.0.routes.lock().unwrap().insert(
            format!("{method} {path}"),
            (
                status,
                headers
                    .iter()
                    .map(|(n, v)| (n.to_string(), v.to_string()))
                    .collect(),
                body.to_string(),
            ),
        );
        self
    }

    fn seen(&self) -> Vec<(String, String, String)> {
        self.0.seen.lock().unwrap().clone()
    }

    fn sent(&self, method: &str) -> Vec<String> {
        self.seen()
            .into_iter()
            .filter(|(m, _, _)| m == method)
            .map(|(_, _, body)| body)
            .collect()
    }

    fn serve(&self, request: &HttpRequest) -> HttpResponse {
        let method = request.method().as_str();
        let url = request.uri().to_string();
        let path = url.trim_start_matches(HOST).to_string();
        self.0.seen.lock().unwrap().push((
            method.to_string(),
            path.clone(),
            String::from_utf8_lossy(request.body()).into_owned(),
        ));
        let key = format!("{method} {path}");
        if let Some(queue) = self.0.queued.lock().unwrap().get_mut(&key) {
            if !queue.is_empty() {
                let (status, headers, body) = queue.remove(0);
                return canned(status, &headers, body);
            }
        }
        match self.0.routes.lock().unwrap().get(&key) {
            Some((status, headers, body)) => canned(*status, headers, body.clone()),
            None => Response::builder()
                .status(404)
                .body(Box::new(Cursor::new(Vec::new())) as Body)
                .unwrap(),
        }
    }
}

/// A scripted response as the transport hands it back: JSON, plus
/// whatever headers the script named.
fn canned(status: u16, headers: &[(String, String)], body: String) -> HttpResponse {
    let mut response = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json");
    for (name, value) in headers {
        response = response.header(name.as_str(), value.as_str());
    }
    response
        .body(Box::new(Cursor::new(body.into_bytes())) as Body)
        .unwrap()
}

impl HttpClient for FakeHttp {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        Ok(self.serve(&request))
    }
}

// ---- a library with one syncable book ----

struct FakeBook(chapbook_core::BookMetadata);

impl chapbook_core::Publication for FakeBook {
    fn kind(&self) -> chapbook_core::BookKind {
        chapbook_core::BookKind::Epub
    }
    fn metadata(&self) -> &chapbook_core::BookMetadata {
        &self.0
    }
    fn spine(&self) -> &[chapbook_core::SpineItem] {
        &[]
    }
    fn toc(&self) -> &[chapbook_core::TocEntry] {
        &[]
    }
    fn unit_bytes(&self, _: usize) -> chapbook_core::Result<Vec<u8>> {
        Ok(Vec::new())
    }
}

fn scratch() -> std::path::PathBuf {
    use std::hash::{BuildHasher, Hasher};
    let suffix = std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish();
    let dir = std::env::temp_dir().join(format!("chapbook-sync-{}-{suffix}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn library_with_book(dir: &std::path::Path) -> (Library, BookId) {
    let mut library = Library::open(dir).unwrap();
    let path = dir.join("book.epub");
    std::fs::write(&path, b"a book").unwrap();
    let metadata = chapbook_core::BookMetadata {
        title: Some("Moby-Dick".into()),
        identifier: Some("urn:isbn:9780000000000".into()),
        ..Default::default()
    };
    let id = library.import(&path, &FakeBook(metadata)).unwrap();
    library
        .set_sync_targets(id, Some(PROGRESSION), Some(CONTAINER))
        .unwrap();
    (library, id)
}

fn engine(library: Library, http: FakeHttp) -> SyncEngine {
    SyncEngine::new(
        library,
        Arc::new(http),
        Device {
            id: "urn:uuid:this-device".into(),
            name: "chapbook".into(),
        },
    )
}

fn locator(offset: u32, progression: f64) -> LayeredLocator {
    LayeredLocator {
        spine_href: "OEBPS/ch4.xhtml".into(),
        spine_index: 3,
        char_offset: offset,
        locator_version: LOCATOR_VERSION,
        quote: Quote {
            prefix: "the harbour was ".into(),
            exact: String::new(),
            suffix: "quiet that morning".into(),
        },
        spine_fraction: progression,
        book_progression: progression,
    }
}

fn remote_progression(modified: &str, progression: f64) -> String {
    json!({
        "modified": modified,
        "device": {"id": "urn:uuid:other-device", "name": "Phone"},
        "progression": progression,
        "references": ["OEBPS/ch9.xhtml#:~:text=and%20then%20the%20whale"]
    })
    .to_string()
}

fn empty_container() -> String {
    json!({"type": "AnnotationPage", "items": []}).to_string()
}

// ---- position ----

#[test]
fn a_dirty_position_is_offered_to_the_service() {
    let dir = scratch();
    let (mut library, book) = library_with_book(&dir);
    library.set_position(book, &locator(1200, 0.42)).unwrap();

    let http = FakeHttp::default()
        .on("PUT", "/opds/progression/book", 200, "")
        .on("GET", "/annotations/", 200, &empty_container());
    let mut engine = engine(library, http.clone());

    let report = engine.sync_book(book).unwrap();
    assert_eq!(report.position, PositionReport::Pushed);

    let body = http.sent("PUT").remove(0);
    assert!(body.contains("\"progression\":0.42"), "{body}");
    assert!(
        body.contains("text=the%20harbour%20was%20-,quiet%20that%20morning"),
        "the quote layer did not travel: {body}"
    );
    assert!(
        !body.contains("1200"),
        "char_offset must not cross the wire: {body}"
    );

    // Clean now, so a second sync offers nothing.
    assert!(!engine.library().position_needs_push(book).unwrap());
    std::fs::remove_dir_all(&dir).ok();
}

/// Two devices race routinely. A service that says its copy is newer is
/// not a failure, and must not leave the local position looking synced.
#[test]
fn a_refused_push_leaves_the_position_owing_a_write() {
    let dir = scratch();
    let (mut library, book) = library_with_book(&dir);
    library.set_position(book, &locator(1200, 0.42)).unwrap();

    let http = FakeHttp::default()
        .on(
            "PUT",
            "/opds/progression/book",
            409,
            &json!({"type": "https://registry.opds.io/error#progression-date",
                    "title": "A more recent progression point is already available."})
            .to_string(),
        )
        .on("GET", "/annotations/", 200, &empty_container());
    let mut engine = engine(library, http);

    let report = engine.sync_book(book).unwrap();
    assert!(
        matches!(report.position, PositionReport::Refused(_)),
        "{:?}",
        report.position
    );
    assert!(
        engine.library().position_needs_push(book).unwrap(),
        "a refused push must not mark the position clean"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// The service moved and this device did not: adopt it. The adopted
/// locator must not claim an offset it never took.
#[test]
fn a_clean_local_position_adopts_the_services() {
    let dir = scratch();
    let (mut library, book) = library_with_book(&dir);
    library.set_position(book, &locator(1200, 0.42)).unwrap();
    // Pretend this position already reached the service.
    let revision = library.positions_needing_push().unwrap()[0].revision;
    library
        .mark_position_synced(book, revision, "2026-08-01T00:00:00Z")
        .unwrap();

    let http = FakeHttp::default()
        .on(
            "GET",
            "/opds/progression/book",
            200,
            &remote_progression("2026-08-30T12:00:00Z", 0.77),
        )
        .on("GET", "/annotations/", 200, &empty_container());
    let mut engine = engine(library, http.clone());

    let report = engine.sync_book(book).unwrap();
    assert_eq!(report.position, PositionReport::Pulled);
    assert!(http.sent("PUT").is_empty(), "a clean position was pushed");

    let stored = engine.library().position(book).unwrap().unwrap();
    assert_eq!(stored.locator.book_progression, 0.77);
    assert_eq!(stored.locator.spine_href, "OEBPS/ch9.xhtml");
    assert_eq!(
        stored.locator.quote.exact, "and then the whale",
        "the peer's quote is what re-anchors it"
    );
    assert_eq!(
        stored.locator.locator_version, 0,
        "an adopted position must not claim an offset this build can trust"
    );
    assert_eq!(stored.locator.char_offset, 0);

    // Adopting is agreement, not a change to push back.
    assert!(!engine.library().position_needs_push(book).unwrap());
    std::fs::remove_dir_all(&dir).ok();
}

/// The service's copy has not moved since we last looked, so there is
/// nothing to adopt and nothing to say.
#[test]
fn an_unchanged_service_copy_is_left_alone() {
    let dir = scratch();
    let (mut library, book) = library_with_book(&dir);
    library.set_position(book, &locator(1200, 0.42)).unwrap();
    let revision = library.positions_needing_push().unwrap()[0].revision;
    library
        .mark_position_synced(book, revision, "2026-08-30T12:00:00Z")
        .unwrap();

    let http = FakeHttp::default()
        .on(
            "GET",
            "/opds/progression/book",
            200,
            &remote_progression("2026-08-30T12:00:00Z", 0.77),
        )
        .on("GET", "/annotations/", 200, &empty_container());
    let mut engine = engine(library, http);

    assert_eq!(
        engine.sync_book(book).unwrap().position,
        PositionReport::Idle
    );
    assert_eq!(
        engine
            .library()
            .position(book)
            .unwrap()
            .unwrap()
            .locator
            .book_progression,
        0.42,
        "an unchanged remote overwrote the local position"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// A service with nothing recorded answers 200 with an empty body. That is
/// not a position at 0.0, and must not be read as one.
#[test]
fn an_empty_service_answer_is_not_a_position_at_zero() {
    let dir = scratch();
    let (mut library, book) = library_with_book(&dir);
    library.set_position(book, &locator(1200, 0.42)).unwrap();
    let revision = library.positions_needing_push().unwrap()[0].revision;
    library
        .mark_position_synced(book, revision, "2026-08-30T12:00:00Z")
        .unwrap();

    let http = FakeHttp::default()
        .on("GET", "/opds/progression/book", 200, "")
        .on("GET", "/annotations/", 200, &empty_container());
    let mut engine = engine(library, http);

    assert_eq!(
        engine.sync_book(book).unwrap().position,
        PositionReport::Idle
    );
    assert_eq!(
        engine
            .library()
            .position(book)
            .unwrap()
            .unwrap()
            .locator
            .book_progression,
        0.42
    );
    std::fs::remove_dir_all(&dir).ok();
}

// ---- annotations ----

#[test]
fn a_new_mark_is_created_and_remembered() {
    let dir = scratch();
    let (mut library, book) = library_with_book(&dir);
    let annotation = library
        .add_annotation(
            book,
            AnnotationKind::Highlight,
            &locator(10, 0.1),
            Some(&locator(30, 0.2)),
            Some("a note"),
            Some("#ffcc00"),
        )
        .unwrap();

    let http = FakeHttp::default()
        .route(
            "POST",
            "/annotations/",
            201,
            &[
                ("Location", "https://library.example.com/annotations/abc"),
                ("ETag", "\"v1\""),
            ],
            "",
        )
        .on("GET", "/annotations/", 200, &empty_container());
    let mut engine = engine(library, http.clone());

    let report = engine.sync_book(book).unwrap();
    assert_eq!(report.annotations.created, 1);

    let body = http.sent("POST").remove(0);
    assert!(body.contains("TextQuoteSelector"), "{body}");
    assert!(
        body.contains("urn:isbn:9780000000000"),
        "the mark should anchor to the publication: {body}"
    );

    assert!(engine
        .library()
        .annotations_needing_push(book)
        .unwrap()
        .is_empty());
    assert_eq!(
        engine
            .library()
            .annotation_by_remote_iri("https://library.example.com/annotations/abc")
            .unwrap(),
        Some(annotation)
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// The container's copy moved and says something else. Neither edit is
/// recoverable once dropped, so both survive: theirs becomes a mark of its
/// own here, ours keeps the IRI there.
#[test]
fn a_refused_edit_keeps_both_sides() {
    let dir = scratch();
    let (mut library, book) = library_with_book(&dir);
    let annotation = library
        .add_annotation(
            book,
            AnnotationKind::Highlight,
            &locator(10, 0.1),
            None,
            None,
            None,
        )
        .unwrap();
    let revision = library.annotations_needing_push(book).unwrap()[0].revision;
    library
        .mark_annotation_synced(
            annotation,
            revision,
            "https://library.example.com/annotations/abc",
            Some("\"v1\""),
        )
        .unwrap();
    library
        .set_annotation_color(annotation, Some("#00ccff"))
        .unwrap();

    let theirs = json!({
        "@context": "http://www.w3.org/ns/anno.jsonld",
        "id": "https://library.example.com/annotations/abc",
        "type": "Annotation",
        "motivation": "commenting",
        "bodyValue": "typed on the phone",
        "target": {"source": "urn:isbn:9780000000000", "selector": [
            {"type": "TextQuoteSelector", "exact": "Call me Ishmael"},
            {"type": "ProgressSelector", "value": 0.1}
        ]}
    })
    .to_string();

    let http = FakeHttp::default()
        // The stale tag is refused; the re-read hands back their copy and a
        // tag that works, and the second write carries it.
        .once("PUT", "/annotations/abc", 412, &[], "")
        .route(
            "GET",
            "/annotations/abc",
            200,
            &[("ETag", "\"v2\"")],
            &theirs,
        )
        .on("PUT", "/annotations/abc", 200, &theirs)
        .on("GET", "/annotations/", 200, &empty_container());
    let mut engine = engine(library, http.clone());

    let report = engine.sync_book(book).unwrap();
    assert_eq!(report.annotations.merged, 1);
    assert_eq!(report.annotations.updated, 1);
    assert_eq!(report.annotations.conflicts, 0);

    // Their words are here now, as a second mark rather than instead of
    // ours.
    let marks = engine.library().annotations(book).unwrap();
    assert_eq!(marks.len(), 2, "{marks:?}");
    assert!(
        marks.iter().any(|m| m.color.as_deref() == Some("#00ccff")),
        "our edit survived: {marks:?}"
    );
    assert!(
        marks
            .iter()
            .any(|m| m.text.as_deref() == Some("typed on the phone")),
        "their edit survived: {marks:?}"
    );

    // Ours is settled with the container; theirs is the one still owed a
    // write, which is what carries it back to the device that made it.
    let owed = engine.library().annotations_needing_push(book).unwrap();
    assert_eq!(owed.len(), 1, "{owed:?}");
    assert!(owed[0].remote_iri.is_none());

    // The retry carried the tag the re-read produced, not the stale one.
    assert_eq!(http.sent("PUT").len(), 2);
}

/// Both devices made the same edit. There is nothing to choose, so the row
/// simply agrees with the container — and nothing is written to say so.
#[test]
fn a_refused_edit_that_already_agrees_writes_nothing() {
    let dir = scratch();
    let (mut library, book) = library_with_book(&dir);
    let annotation = library
        .add_annotation(
            book,
            AnnotationKind::Highlight,
            &locator(10, 0.1),
            None,
            None,
            Some("#00ccff"),
        )
        .unwrap();
    let revision = library.annotations_needing_push(book).unwrap()[0].revision;
    library
        .mark_annotation_synced(
            annotation,
            revision,
            "https://library.example.com/annotations/abc",
            Some("\"v1\""),
        )
        .unwrap();
    library
        .set_annotation_color(annotation, Some("#ffcc00"))
        .unwrap();

    // What the container holds is exactly what this device was about to
    // write — the whole document, locator extensions included, because a
    // mark that differs in its anchor is a different mark. Only `created`
    // and `modified` are allowed to differ, and they do.
    let same = json!({
        "@context": "http://www.w3.org/ns/anno.jsonld",
        "id": "https://library.example.com/annotations/abc",
        "type": "Annotation",
        "motivation": "highlighting",
        "chapbook:color": "#ffcc00",
        "created": "2020-01-01T00:00:00Z",
        "modified": "2020-01-01T00:00:00Z",
        "target": {
            "type": "SpecificResource",
            "source": "urn:isbn:9780000000000",
            "chapbook:spineHref": "OEBPS/ch4.xhtml",
            "chapbook:spineIndex": 3,
            "chapbook:spineFraction": 0.1,
            "selector": [
                {"type": "TextQuoteSelector", "exact": "", "prefix": "the harbour was ",
                 "suffix": "quiet that morning"},
                {"type": "TextPositionSelector", "start": 10, "end": 10,
                 "chapbook:locatorVersion": 2},
                {"type": "ProgressSelector", "value": 0.1}
            ]
        }
    })
    .to_string();

    let http = FakeHttp::default()
        .once("PUT", "/annotations/abc", 412, &[], "")
        .route("GET", "/annotations/abc", 200, &[("ETag", "\"v2\"")], &same)
        .on("GET", "/annotations/", 200, &empty_container());
    let mut engine = engine(library, http.clone());

    let report = engine.sync_book(book).unwrap();
    assert_eq!(report.annotations.merged, 1);
    assert_eq!(
        report.annotations.updated, 0,
        "nothing went over the wire, so nothing was written"
    );
    assert_eq!(report.annotations.conflicts, 0);
    assert_eq!(
        engine.library().annotations(book).unwrap().len(),
        1,
        "an agreement is not a second mark"
    );
    assert!(engine
        .library()
        .annotations_needing_push(book)
        .unwrap()
        .is_empty());
    // Only the refused first attempt; the merge found nothing to say.
    assert_eq!(http.sent("PUT").len(), 1);
}

/// A third write landing between the re-read and the retry is a race, not
/// a merge. One retry settles a conflict; looping on it is a fight.
#[test]
fn an_edit_refused_twice_is_left_owing_a_write() {
    let dir = scratch();
    let (mut library, book) = library_with_book(&dir);
    let annotation = library
        .add_annotation(
            book,
            AnnotationKind::Highlight,
            &locator(10, 0.1),
            None,
            None,
            None,
        )
        .unwrap();
    let revision = library.annotations_needing_push(book).unwrap()[0].revision;
    library
        .mark_annotation_synced(
            annotation,
            revision,
            "https://library.example.com/annotations/abc",
            Some("\"v1\""),
        )
        .unwrap();
    library
        .set_annotation_color(annotation, Some("#00ccff"))
        .unwrap();

    let theirs = json!({
        "@context": "http://www.w3.org/ns/anno.jsonld",
        "id": "https://library.example.com/annotations/abc",
        "type": "Annotation",
        "motivation": "commenting",
        "bodyValue": "typed on the phone",
        "target": {"source": "urn:isbn:9780000000000", "selector": [
            {"type": "TextQuoteSelector", "exact": "Call me Ishmael"},
            {"type": "ProgressSelector", "value": 0.1}
        ]}
    })
    .to_string();

    let http = FakeHttp::default()
        .route(
            "GET",
            "/annotations/abc",
            200,
            &[("ETag", "\"v2\"")],
            &theirs,
        )
        .on("PUT", "/annotations/abc", 412, "")
        .on("GET", "/annotations/", 200, &empty_container());
    let mut engine = engine(library, http);

    let report = engine.sync_book(book).unwrap();
    assert_eq!(report.annotations.conflicts, 1);
    assert_eq!(report.annotations.merged, 0);
    assert_eq!(report.annotations.updated, 0);
    assert_eq!(
        engine
            .library()
            .annotations_needing_push(book)
            .unwrap()
            .len(),
        2,
        "ours still owes a write, and so does the copy of theirs"
    );
}

/// A delete the container refuses is still a delete. The reader said to
/// remove the mark; a tag that moved is not a reason to keep it.
#[test]
fn a_refused_delete_is_retried_with_a_fresh_tag() {
    let dir = scratch();
    let (mut library, book) = library_with_book(&dir);
    let annotation = library
        .add_annotation(
            book,
            AnnotationKind::Highlight,
            &locator(10, 0.1),
            None,
            None,
            None,
        )
        .unwrap();
    let revision = library.annotations_needing_push(book).unwrap()[0].revision;
    library
        .mark_annotation_synced(
            annotation,
            revision,
            "https://library.example.com/annotations/abc",
            Some("\"v1\""),
        )
        .unwrap();
    library.delete_annotation(annotation).unwrap();

    let theirs = json!({
        "@context": "http://www.w3.org/ns/anno.jsonld",
        "id": "https://library.example.com/annotations/abc",
        "type": "Annotation",
        "motivation": "highlighting",
        "target": {"source": "urn:isbn:9780000000000", "selector": [
            {"type": "ProgressSelector", "value": 0.1}
        ]}
    })
    .to_string();

    let http = FakeHttp::default()
        .once("DELETE", "/annotations/abc", 412, &[], "")
        .route(
            "GET",
            "/annotations/abc",
            200,
            &[("ETag", "\"v2\"")],
            &theirs,
        )
        .on("DELETE", "/annotations/abc", 204, "")
        .on("GET", "/annotations/", 200, &empty_container());
    let mut engine = engine(library, http.clone());

    let report = engine.sync_book(book).unwrap();
    assert_eq!(report.annotations.deleted, 1);
    assert_eq!(report.annotations.merged, 1);
    assert_eq!(report.annotations.conflicts, 0);
    assert!(
        engine.library().annotations(book).unwrap().is_empty(),
        "the row goes once the container has been told"
    );
    assert_eq!(http.sent("DELETE").len(), 2);
}

/// The reader deleted it here; the container has to be told, and only then
/// may the row go.
#[test]
fn a_deleted_mark_is_removed_there_before_it_is_forgotten_here() {
    let dir = scratch();
    let (mut library, book) = library_with_book(&dir);
    let annotation = library
        .add_annotation(
            book,
            AnnotationKind::Bookmark,
            &locator(10, 0.1),
            None,
            None,
            None,
        )
        .unwrap();
    let revision = library.annotations_needing_push(book).unwrap()[0].revision;
    library
        .mark_annotation_synced(
            annotation,
            revision,
            "https://library.example.com/annotations/abc",
            None,
        )
        .unwrap();
    library.delete_annotation(annotation).unwrap();

    let http = FakeHttp::default()
        .on("DELETE", "/annotations/abc", 204, "")
        .on("GET", "/annotations/", 200, &empty_container());
    let mut engine = engine(library, http.clone());

    let report = engine.sync_book(book).unwrap();
    assert_eq!(report.annotations.deleted, 1);
    assert_eq!(http.sent("DELETE").len(), 1);
    assert!(engine
        .library()
        .annotations_needing_push(book)
        .unwrap()
        .is_empty());
    assert_eq!(
        engine
            .library()
            .annotation_by_remote_iri("https://library.example.com/annotations/abc")
            .unwrap(),
        None,
        "the row should be gone once the container has been told"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// A mark from another device arrives with its own IRI and is adopted
/// once — a second sync must not duplicate it.
#[test]
fn a_mark_from_another_device_is_adopted_exactly_once() {
    let dir = scratch();
    let (library, book) = library_with_book(&dir);
    let remote = json!({
        "type": "AnnotationPage",
        "items": [{
            "@context": "http://www.w3.org/ns/anno.jsonld",
            "id": "https://library.example.com/annotations/theirs",
            "type": "Annotation",
            "motivation": "highlighting",
            "bodyValue": "from the phone",
            "target": {"source": "urn:isbn:9780000000000", "selector": [
                {"type": "TextQuoteSelector", "exact": "Call me Ishmael",
                 "prefix": "Loomings. "},
                {"type": "ProgressSelector", "value": 0.03}
            ]}
        }]
    })
    .to_string();

    let http = FakeHttp::default()
        .on("GET", "/opds/progression/book", 200, "")
        .on("GET", "/annotations/", 200, &remote);
    let mut engine = engine(library, http);

    let report = engine.sync_book(book).unwrap();
    assert_eq!(report.annotations.adopted, 1);
    let marks = engine.library().annotations(book).unwrap();
    assert_eq!(marks.len(), 1);
    assert_eq!(marks[0].text.as_deref(), Some("from the phone"));
    assert_eq!(marks[0].start.quote.exact, "Call me Ishmael");
    assert_eq!(
        marks[0].start.locator_version, 0,
        "an unstamped peer offset must not be trusted"
    );

    // Adopting is agreement: it must not immediately owe a write back,
    // and a second pass must not create a second copy.
    assert!(engine
        .library()
        .annotations_needing_push(book)
        .unwrap()
        .is_empty());
    let report = engine.sync_book(book).unwrap();
    assert_eq!(report.annotations.adopted, 0);
    assert_eq!(engine.library().annotations(book).unwrap().len(), 1);
    std::fs::remove_dir_all(&dir).ok();
}

/// A book nobody gave a service to is not an error worth a network call.
#[test]
fn a_book_with_no_service_is_refused_before_any_request() {
    let dir = scratch();
    let (mut library, book) = library_with_book(&dir);
    library.set_sync_targets(book, None, None).unwrap();

    let http = FakeHttp::default();
    let mut engine = engine(library, http.clone());
    assert!(engine.sync_book(book).is_err());
    assert!(
        http.seen().is_empty(),
        "a bookless sync went to the network"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// The position service and the annotation container are separate
/// services, possibly on separate hosts. One being down must not stop the
/// other: a reader whose catalog lost its progression endpoint should
/// still have their highlights reach the container.
#[test]
fn a_dead_progression_service_does_not_stop_the_marks() {
    let dir = scratch();
    let (mut library, book) = library_with_book(&dir);
    library.set_position(book, &locator(1200, 0.42)).unwrap();
    library
        .add_annotation(
            book,
            AnnotationKind::Highlight,
            &locator(10, 0.1),
            None,
            None,
            None,
        )
        .unwrap();

    let http = FakeHttp::default()
        .on("PUT", "/opds/progression/book", 503, "")
        .route(
            "POST",
            "/annotations/",
            201,
            &[("Location", "https://library.example.com/annotations/abc")],
            "",
        )
        .on("GET", "/annotations/", 200, &empty_container());
    let mut engine = engine(library, http);

    let report = engine.sync_book(book).unwrap();
    assert!(
        matches!(report.position, PositionReport::Failed(_)),
        "{:?}",
        report.position
    );
    assert_eq!(report.annotations.created, 1, "the mark did not get out");
    assert_eq!(report.annotations.failed, None);
    // And the position still owes a write, for the next attempt.
    assert!(engine.library().position_needs_push(book).unwrap());
    std::fs::remove_dir_all(&dir).ok();
}

/// And the other way round.
#[test]
fn a_dead_container_does_not_stop_the_position() {
    let dir = scratch();
    let (mut library, book) = library_with_book(&dir);
    library.set_position(book, &locator(1200, 0.42)).unwrap();
    library
        .add_annotation(
            book,
            AnnotationKind::Highlight,
            &locator(10, 0.1),
            None,
            None,
            None,
        )
        .unwrap();

    let http = FakeHttp::default()
        .on("PUT", "/opds/progression/book", 200, "")
        .on("POST", "/annotations/", 503, "");
    let mut engine = engine(library, http);

    let report = engine.sync_book(book).unwrap();
    assert_eq!(report.position, PositionReport::Pushed);
    assert!(report.annotations.failed.is_some());
    assert!(!engine.library().position_needs_push(book).unwrap());
    assert_eq!(
        engine
            .library()
            .annotations_needing_push(book)
            .unwrap()
            .len(),
        1,
        "the unsent mark must still owe a write"
    );
    std::fs::remove_dir_all(&dir).ok();
}

// ---- the worker ----

/// The worker is the supported way in, and dropping it must mean nothing
/// is still writing to the library behind your back.
#[test]
fn the_worker_reports_each_book_and_joins_on_drop() {
    use chapbook_sync::{SyncCommand, SyncEvent, SyncWorker};
    use std::sync::atomic::{AtomicUsize, Ordering};

    let dir = scratch();
    let (mut library, book) = library_with_book(&dir);
    library.set_position(book, &locator(1200, 0.42)).unwrap();

    let http = FakeHttp::default()
        .on("PUT", "/opds/progression/book", 200, "")
        .on("GET", "/annotations/", 200, &empty_container());
    let engine = engine(library, http);

    let wakes = Arc::new(AtomicUsize::new(0));
    let counter = wakes.clone();
    let worker = SyncWorker::spawn(
        engine,
        Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }),
    );

    assert!(worker.request(SyncCommand::All));
    // One event per book, then the batch's own.
    let mut reports = 0;
    let mut finished = None;
    while finished.is_none() {
        match worker.next_event() {
            Some(SyncEvent::Book(report)) => {
                assert_eq!(report.position, PositionReport::Pushed);
                reports += 1;
            }
            Some(SyncEvent::Failed { reason, .. }) => panic!("{reason}"),
            Some(SyncEvent::Finished { books }) => finished = Some(books),
            None => panic!("the worker ended without finishing"),
        }
    }
    assert_eq!(reports, 1);
    assert_eq!(finished, Some(1));
    assert!(
        wakes.load(Ordering::SeqCst) >= 2,
        "the shell should be nudged per book, not only per batch"
    );

    drop(worker);
    std::fs::remove_dir_all(&dir).ok();
}

/// A book that cannot sync is reported and the batch goes on — one dead
/// host should not leave the rest of a shelf unsynced.
#[test]
fn one_unsyncable_book_does_not_end_the_batch() {
    use chapbook_sync::{SyncCommand, SyncEvent, SyncWorker};

    let dir = scratch();
    let (mut library, first) = library_with_book(&dir);

    // A second book pointing at a service that answers nothing at all.
    let path = dir.join("second.epub");
    std::fs::write(&path, b"another book").unwrap();
    let second = library
        .import(
            &path,
            &FakeBook(chapbook_core::BookMetadata {
                title: Some("Second".into()),
                ..Default::default()
            }),
        )
        .unwrap();
    library
        .set_sync_targets(
            second,
            Some("https://library.example.com/opds/progression/missing"),
            None,
        )
        .unwrap();
    library.set_position(first, &locator(1200, 0.42)).unwrap();
    library.set_position(second, &locator(5, 0.05)).unwrap();

    let http = FakeHttp::default()
        .on("PUT", "/opds/progression/book", 200, "")
        .on("GET", "/annotations/", 200, &empty_container());
    let worker = SyncWorker::spawn(engine(library, http), Arc::new(|| {}));
    assert!(worker.request(SyncCommand::All));

    let mut seen = Vec::new();
    loop {
        match worker.next_event() {
            Some(SyncEvent::Book(report)) => seen.push(report.book),
            Some(SyncEvent::Failed { book, .. }) => seen.push(book),
            Some(SyncEvent::Finished { books }) => {
                assert_eq!(books, 2, "the batch stopped early");
                break;
            }
            None => panic!("the worker ended without finishing"),
        }
    }
    assert!(seen.contains(&first) && seen.contains(&second));
    drop(worker);
    std::fs::remove_dir_all(&dir).ok();
}

/// The bug a one-book test cannot see.
///
/// A Web Annotation container holds every annotation a reader has, for
/// every book: the protocol defines no way to ask one for a single
/// publication's, and a `?target=` on the advertised link is decoration
/// that a server is free to ignore — the reference implementation does.
/// So a pull has to filter on the target itself, or syncing one book files
/// another book's highlights against it and the next push sends them back
/// anchored to the wrong publication.
#[test]
fn another_books_marks_are_not_adopted_from_a_shared_container() {
    let dir = scratch();
    let (library, book) = library_with_book(&dir);

    let shared = json!({
        "type": "AnnotationPage",
        "items": [
            {
                "id": "https://library.example.com/annotations/ours",
                "type": "Annotation",
                "motivation": "highlighting",
                "bodyValue": "in this book",
                "target": {"source": "urn:isbn:9780000000000", "selector": [
                    {"type": "TextQuoteSelector", "exact": "Call me Ishmael"}
                ]}
            },
            {
                "id": "https://library.example.com/annotations/theirs",
                "type": "Annotation",
                "motivation": "highlighting",
                "bodyValue": "in a different book entirely",
                "target": {"source": "urn:isbn:9781111111111", "selector": [
                    {"type": "TextQuoteSelector", "exact": "It is a truth universally"}
                ]}
            }
        ]
    })
    .to_string();

    let http = FakeHttp::default()
        .on("GET", "/opds/progression/book", 200, "")
        .on("GET", "/annotations/", 200, &shared);
    let mut engine = engine(library, http);

    let report = engine.sync_book(book).unwrap();
    assert_eq!(report.annotations.adopted, 1, "the wrong count came back");

    let marks = engine.library().annotations(book).unwrap();
    assert_eq!(marks.len(), 1, "a mark from another book was adopted");
    assert_eq!(marks[0].text.as_deref(), Some("in this book"));

    // And the one that was skipped must not have been claimed, so the
    // other book can adopt it when its own turn comes.
    assert_eq!(
        engine
            .library()
            .annotation_by_remote_iri("https://library.example.com/annotations/theirs")
            .unwrap(),
        None
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// A book off the shelf does not sync, and both doors agree about it.
///
/// `sync_all` filters removed books in SQL and always has; `sync_book`
/// named one directly and went ahead, which mattered because the two dirty
/// predicates disagree about removed rows — `positions_needing_push`
/// filters them and `position_needs_push` does not. A removed book with a
/// dirty position took the pull path and came back `Conflict`, reporting
/// "both sides moved" about a book that had simply been taken off the
/// shelf. Worse, a removed book with a *clean* position adopted the
/// service's, writing a position onto a row the reader had removed.
#[test]
fn a_removed_book_does_not_sync_from_either_door() {
    let dir = scratch();
    let (mut library, book) = library_with_book(&dir);
    library.set_position(book, &locator(1200, 0.42)).unwrap();
    library.delete_book(book).unwrap();

    // Nothing is routed: reaching the network at all is the failure this
    // is watching for.
    let http = FakeHttp::default();
    let mut engine = engine(library, http.clone());

    assert!(
        engine
            .library()
            .books_with_sync_targets()
            .unwrap()
            .is_empty(),
        "sync_all must not offer a removed book"
    );
    assert!(
        matches!(engine.sync_book(book), Err(SyncError::NotSyncable(_))),
        "sync_book must refuse a removed book"
    );
    assert!(
        http.sent("PUT").is_empty() && http.sent("GET").is_empty(),
        "a removed book must not reach the network"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// Removing a book freezes what it owed a service rather than sending or
/// discarding it, and importing the same file again thaws it.
///
/// This is the half of `delete_book` that is a decision rather than an
/// oversight: the marks already pushed stay in their container, because
/// tidying one shelf is not a statement about the reader's other devices.
#[test]
fn what_a_removed_book_owed_is_frozen_and_comes_back_with_it() {
    let dir = scratch();
    let (mut library, book) = library_with_book(&dir);
    library.set_position(book, &locator(1200, 0.42)).unwrap();
    assert!(library.position_needs_push(book).unwrap());

    library.delete_book(book).unwrap();
    // Owed, not sent and not dropped: the row still says so, and only the
    // queries that feed a sync decline to offer it.
    assert!(
        library.position_needs_push(book).unwrap(),
        "the debt survives the removal"
    );
    assert!(library.positions_needing_push().unwrap().is_empty());

    // The same file back is the same book — `record` clears `deleted` — so
    // the targets and the debt are both still there.
    let path = dir.join("book.epub");
    let metadata = chapbook_core::BookMetadata {
        title: Some("Moby-Dick".into()),
        identifier: Some("urn:isbn:9780000000000".into()),
        ..Default::default()
    };
    let again = library.import(&path, &FakeBook(metadata)).unwrap();
    assert_eq!(again, book, "the same bytes are the same book");
    assert_eq!(
        library.positions_needing_push().unwrap().len(),
        1,
        "what it owed is offered again once it is back on the shelf"
    );
    assert!(library
        .sync_targets(book)
        .unwrap()
        .progression_url
        .is_some());
    std::fs::remove_dir_all(&dir).ok();
}

/// The same mark, anchored identically, with a note another device typed.
/// Only the words differ, so a refresh is exactly what should happen.
fn their_edit_of_abc(text: Option<&str>, color: Option<&str>) -> String {
    let mut doc = json!({
        "@context": "http://www.w3.org/ns/anno.jsonld",
        "id": "https://library.example.com/annotations/abc",
        "type": "Annotation",
        "motivation": "highlighting",
        "created": "2020-01-01T00:00:00Z",
        "modified": "2020-01-02T00:00:00Z",
        "target": {
            "type": "SpecificResource",
            "source": "urn:isbn:9780000000000",
            "chapbook:spineHref": "OEBPS/ch4.xhtml",
            "chapbook:spineIndex": 3,
            "chapbook:spineFraction": 0.1,
            "selector": [
                {"type": "TextQuoteSelector", "exact": "", "prefix": "the harbour was ",
                 "suffix": "quiet that morning"},
                {"type": "TextPositionSelector", "start": 10, "end": 10,
                 "chapbook:locatorVersion": 2},
                {"type": "ProgressSelector", "value": 0.1}
            ]
        }
    });
    if let Some(text) = text {
        doc["bodyValue"] = json!(text);
    }
    if let Some(color) = color {
        doc["chapbook:color"] = json!(color);
    }
    doc.to_string()
}

fn container_with(item: &str) -> String {
    format!("{{\"type\":\"AnnotationPage\",\"items\":[{item}]}}")
}

/// A mark this device already has, changed elsewhere. Until the pull
/// looked at known IRIs at all, this arrived only when this device
/// happened to write into the same mark and be refused.
#[test]
fn a_change_another_device_made_is_pulled() {
    let dir = scratch();
    let (mut library, book) = library_with_book(&dir);
    let annotation = library
        .add_annotation(
            book,
            AnnotationKind::Highlight,
            &locator(10, 0.1),
            None,
            None,
            None,
        )
        .unwrap();
    let revision = library.annotations_needing_push(book).unwrap()[0].revision;
    library
        .mark_annotation_synced(
            annotation,
            revision,
            "https://library.example.com/annotations/abc",
            Some("\"v1\""),
        )
        .unwrap();

    let http = FakeHttp::default().on(
        "GET",
        "/annotations/",
        200,
        &container_with(&their_edit_of_abc(
            Some("typed on the phone"),
            Some("#ffcc00"),
        )),
    );
    let mut engine = engine(library, http.clone());

    let report = engine.sync_book(book).unwrap();
    assert_eq!(report.annotations.refreshed, 1);
    assert_eq!(
        report.annotations.adopted, 0,
        "an edit to a known mark is not a new mark"
    );

    let marks = engine.library().annotations(book).unwrap();
    assert_eq!(marks.len(), 1, "refreshing must not duplicate: {marks:?}");
    assert_eq!(
        marks[0].id, annotation,
        "the same row, saying something else"
    );
    assert_eq!(marks[0].text.as_deref(), Some("typed on the phone"));
    assert_eq!(marks[0].color.as_deref(), Some("#ffcc00"));

    // Adopted from the container, so it owes the container nothing.
    assert!(engine
        .library()
        .annotations_needing_push(book)
        .unwrap()
        .is_empty());
    assert!(http.sent("PUT").is_empty(), "a pull must not write");
}

/// The container's copy says what this device already holds. Nothing
/// changed, so nothing is reported as having changed.
#[test]
fn a_known_mark_the_container_agrees_about_is_left_alone() {
    let dir = scratch();
    let (mut library, book) = library_with_book(&dir);
    let annotation = library
        .add_annotation(
            book,
            AnnotationKind::Highlight,
            &locator(10, 0.1),
            None,
            None,
            None,
        )
        .unwrap();
    let revision = library.annotations_needing_push(book).unwrap()[0].revision;
    library
        .mark_annotation_synced(
            annotation,
            revision,
            "https://library.example.com/annotations/abc",
            Some("\"v1\""),
        )
        .unwrap();

    let http = FakeHttp::default().on(
        "GET",
        "/annotations/",
        200,
        &container_with(&their_edit_of_abc(None, None)),
    );
    let mut engine = engine(library, http);

    let report = engine.sync_book(book).unwrap();
    assert_eq!(report.annotations.refreshed, 0);
    assert_eq!(report.annotations.adopted, 0);
    assert_eq!(engine.library().annotations(book).unwrap().len(), 1);
}

/// A mark deleted here is not put back by a pull, including while the
/// container has not been told yet. Resurrecting one would be the sync
/// undoing a reader's decision.
#[test]
fn a_pull_does_not_resurrect_a_mark_deleted_here() {
    let dir = scratch();
    let (mut library, book) = library_with_book(&dir);
    let annotation = library
        .add_annotation(
            book,
            AnnotationKind::Highlight,
            &locator(10, 0.1),
            None,
            None,
            None,
        )
        .unwrap();
    let revision = library.annotations_needing_push(book).unwrap()[0].revision;
    library
        .mark_annotation_synced(
            annotation,
            revision,
            "https://library.example.com/annotations/abc",
            Some("\"v1\""),
        )
        .unwrap();
    library.delete_annotation(annotation).unwrap();

    // The delete is refused twice, so the row stays deleted here and the
    // container keeps listing it — exactly the window a pull could undo.
    let http = FakeHttp::default()
        .on("DELETE", "/annotations/abc", 412, "")
        .route(
            "GET",
            "/annotations/abc",
            200,
            &[("ETag", "\"v2\"")],
            &their_edit_of_abc(None, None),
        )
        .on(
            "GET",
            "/annotations/",
            200,
            &container_with(&their_edit_of_abc(Some("still here"), None)),
        );
    let mut engine = engine(library, http);

    let report = engine.sync_book(book).unwrap();
    assert_eq!(report.annotations.conflicts, 1);
    assert_eq!(report.annotations.adopted, 0);
    assert_eq!(report.annotations.refreshed, 0);
    assert!(
        engine.library().annotations(book).unwrap().is_empty(),
        "the mark stays deleted"
    );
}

/// Both sides moved and the push could not settle it. The pull must not
/// then quietly adopt over the local edit — nor report the same
/// disagreement a second time.
#[test]
fn a_pull_does_not_overwrite_an_edit_that_still_owes_a_write() {
    let dir = scratch();
    let (mut library, book) = library_with_book(&dir);
    let annotation = library
        .add_annotation(
            book,
            AnnotationKind::Highlight,
            &locator(10, 0.1),
            None,
            None,
            None,
        )
        .unwrap();
    let revision = library.annotations_needing_push(book).unwrap()[0].revision;
    library
        .mark_annotation_synced(
            annotation,
            revision,
            "https://library.example.com/annotations/abc",
            Some("\"v1\""),
        )
        .unwrap();
    library
        .set_annotation_color(annotation, Some("#00ccff"))
        .unwrap();

    let http = FakeHttp::default()
        // Refused, and refused again on the retry: still owed.
        .on("PUT", "/annotations/abc", 412, "")
        .route(
            "GET",
            "/annotations/abc",
            200,
            &[("ETag", "\"v2\"")],
            &their_edit_of_abc(Some("typed on the phone"), None),
        )
        .on(
            "GET",
            "/annotations/",
            200,
            &container_with(&their_edit_of_abc(Some("typed on the phone"), None)),
        );
    let mut engine = engine(library, http);

    let report = engine.sync_book(book).unwrap();
    assert_eq!(
        report.annotations.conflicts, 1,
        "one disagreement, reported once"
    );
    assert_eq!(report.annotations.refreshed, 0);

    let ours = engine
        .library()
        .annotations(book)
        .unwrap()
        .into_iter()
        .find(|m| m.id == annotation)
        .expect("our mark is still here");
    assert_eq!(
        ours.color.as_deref(),
        Some("#00ccff"),
        "the local edit survived the pull"
    );
}

/// A mark another device deleted goes from this shelf too. Until the pull
/// could tell a whole listing from a partial one, it could not safely
/// conclude anything from absence, so a deletion never travelled.
#[test]
fn a_mark_deleted_elsewhere_is_taken_off_this_shelf() {
    let dir = scratch();
    let (mut library, book) = library_with_book(&dir);
    let annotation = library
        .add_annotation(
            book,
            AnnotationKind::Highlight,
            &locator(10, 0.1),
            None,
            None,
            None,
        )
        .unwrap();
    let revision = library.annotations_needing_push(book).unwrap()[0].revision;
    library
        .mark_annotation_synced(
            annotation,
            revision,
            "https://library.example.com/annotations/abc",
            Some("\"v1\""),
        )
        .unwrap();

    // A complete listing that does not mention it: it is gone there.
    let http = FakeHttp::default().on("GET", "/annotations/", 200, &empty_container());
    let mut engine = engine(library, http.clone());

    let report = engine.sync_book(book).unwrap();
    assert_eq!(report.annotations.withdrawn, 1);
    assert!(!report.annotations.truncated);
    assert!(
        engine.library().annotations(book).unwrap().is_empty(),
        "the mark is gone here too"
    );
    // Nothing is owed to a container that has already dropped it.
    assert!(engine
        .library()
        .annotations_needing_push(book)
        .unwrap()
        .is_empty());
    assert!(
        http.sent("DELETE").is_empty(),
        "adopting a deletion is not performing one"
    );
}

/// A listing that stopped early proves nothing about what it did not
/// reach. Reading that absence as deletion would take a reader's
/// highlights away for no better reason than a container being long.
#[test]
fn a_listing_that_stopped_early_withdraws_nothing() {
    let dir = scratch();
    let (mut library, book) = library_with_book(&dir);
    let annotation = library
        .add_annotation(
            book,
            AnnotationKind::Highlight,
            &locator(10, 0.1),
            None,
            None,
            None,
        )
        .unwrap();
    let revision = library.annotations_needing_push(book).unwrap()[0].revision;
    library
        .mark_annotation_synced(
            annotation,
            revision,
            "https://library.example.com/annotations/abc",
            Some("\"v1\""),
        )
        .unwrap();

    // A page whose `next` is itself: the walk stops without reaching an
    // end, which is exactly the shape a cap produces.
    let http = FakeHttp::default().on(
        "GET",
        "/annotations/",
        200,
        &json!({"type": "AnnotationPage", "items": [],
                "next": "https://library.example.com/annotations/"})
        .to_string(),
    );
    let mut engine = engine(library, http);

    let report = engine.sync_book(book).unwrap();
    assert!(report.annotations.truncated, "the short walk must say so");
    assert_eq!(
        report.annotations.withdrawn, 0,
        "absence from a prefix is not absence"
    );
    assert_eq!(
        engine.library().annotations(book).unwrap().len(),
        1,
        "the mark stays"
    );
}

/// A mark made moments ago is not evidence of anything. A container that
/// has not listed it yet has not deleted it.
#[test]
fn a_mark_created_this_pass_is_never_withdrawn() {
    let dir = scratch();
    let (mut library, book) = library_with_book(&dir);
    library
        .add_annotation(
            book,
            AnnotationKind::Highlight,
            &locator(10, 0.1),
            None,
            Some("just written"),
            None,
        )
        .unwrap();

    // The container takes the create and then lists nothing — a listing
    // that has not caught up, which is a real thing containers do.
    let http = FakeHttp::default()
        .route(
            "POST",
            "/annotations/",
            201,
            &[("Location", "https://library.example.com/annotations/new")],
            "",
        )
        .on("GET", "/annotations/", 200, &empty_container());
    let mut engine = engine(library, http);

    let report = engine.sync_book(book).unwrap();
    assert_eq!(report.annotations.created, 1);
    assert_eq!(
        report.annotations.withdrawn, 0,
        "a mark this pass created must survive a listing that has not caught up"
    );
    assert_eq!(engine.library().annotations(book).unwrap().len(), 1);
}
