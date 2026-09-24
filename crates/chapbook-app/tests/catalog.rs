//! A catalog browsed the way a screen browses it, against a canned
//! server behind a login — the same feed the two mobile model tests used,
//! served through the platform seam instead of a URLProtocol or a
//! MockWebServer.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chapbook_app::chapbook_opds::{HttpClient, HttpError, HttpRequest, HttpResponse};
use chapbook_app::chapbook_reader::chapbook_core::{
    CredentialKey, CredentialLookup, FontSource, Freshness, MemoryCredentials,
};
use chapbook_app::{App, BrowseState, Platform};

const HOST: &str = "https://catalog.example.test";

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let dir =
            std::env::temp_dir().join(format!("chapbook-app-catalog-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
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
        .join("../../fixtures")
        .join(name)
}

fn feed() -> String {
    format!(
        r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom" xmlns:opds="http://opds-spec.org/2010/catalog">
  <id>urn:root</id><title>Test Shelf</title>
  <link rel="search" href="{HOST}/search?q={{searchTerms}}" type="application/atom+xml"/>
  <link rel="next" href="{HOST}/page2" type="application/atom+xml;profile=opds-catalog"/>
  <link rel="http://opds-spec.org/facet" href="{HOST}/en" title="English" opds:facetGroup="Language" opds:activeFacet="true"/>
  <link rel="http://opds-spec.org/facet" href="{HOST}/fr" title="French" opds:facetGroup="Language"/>
  <entry><id>urn:shelf</id><title>A Section</title>
    <link rel="subsection" href="{HOST}/section" type="application/atom+xml;profile=opds-catalog;kind=acquisition"/>
  </entry>
  <entry><id>urn:book:1</id><title>Minimal</title>
    <author><name>Nobody</name></author>
    <link rel="http://opds-spec.org/acquisition/open-access" href="{HOST}/books/minimal.epub" type="application/epub+zip"/>
    <link rel="http://opds-spec.org/progression" href="/progress/1" type="application/json"/>
  </entry>
</feed>"#
    )
}

fn page2() -> String {
    format!(
        r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <id>urn:page2</id><title>Test Shelf</title>
  <entry><id>urn:more</id><title>More</title>
    <link rel="subsection" href="{HOST}/more" type="application/atom+xml;profile=opds-catalog;kind=acquisition"/>
  </entry>
</feed>"#
    )
}

fn section() -> String {
    r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <id>urn:section</id><title>The Section</title>
</feed>"#
        .to_string()
}

fn auth_document() -> String {
    format!(
        r#"{{"id":"{HOST}/auth","title":"Test Shelf Login",
 "authentication":[{{"type":"http://opds-spec.org/auth/basic"}}]}}"#
    )
}

/// The canned server: refuses until an `Authorization` arrives, then
/// answers by path. Records what it saw.
struct Server {
    seen: Mutex<Vec<(String, Option<String>)>>,
}

impl Server {
    fn respond(status: u16, content_type: &str, body: Vec<u8>) -> HttpResponse {
        HttpResponse {
            status,
            content_type: Some(content_type.to_string()),
            headers: Vec::new(),
            body: Box::new(Cursor::new(body)),
        }
    }
}

impl HttpClient for Server {
    fn get(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        let authorization = request
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
            .map(|(_, value)| value.clone());
        self.seen
            .lock()
            .unwrap()
            .push((request.url.clone(), authorization.clone()));
        if request.url == "https://down.test/" {
            return Err(HttpError::new("nobody home"));
        }
        if authorization.is_none() {
            return Ok(Server::respond(
                401,
                "application/opds-authentication+json",
                auth_document().into_bytes(),
            ));
        }
        let path = request.url.strip_prefix(HOST).unwrap_or("");
        let atom = "application/atom+xml;profile=opds-catalog";
        Ok(match path {
            "/page2" => Server::respond(200, atom, page2().into_bytes()),
            "/section" | "/en" | "/fr" => Server::respond(200, atom, section().into_bytes()),
            "/books/minimal.epub" => Server::respond(
                200,
                "application/epub+zip",
                std::fs::read(fixture("epub/minimal.epub")).expect("fixture"),
            ),
            _ => Server::respond(200, atom, feed().into_bytes()),
        })
    }
}

fn app(dir: &TempDir, server: Arc<Server>) -> App {
    let platform = Platform::new(FontSource::embedded(fixture("fonts"), "Crimson Text"))
        .with_credentials(Arc::new(MemoryCredentials::new()))
        .with_transport(server);
    App::open(dir.path(), platform).expect("open app")
}

#[test]
fn a_refused_catalog_becomes_a_login_and_a_sign_in_reaches_the_feed() {
    let dir = TempDir::new("login");
    let server = Arc::new(Server {
        seen: Mutex::new(Vec::new()),
    });
    let mut app = app(&dir, server.clone());
    let saved = app.add_catalog(&format!("{HOST}/opds/"), "").expect("add");
    let mut catalog = app.browse(&saved).expect("browse");
    assert_eq!(*catalog.state(), BrowseState::Opening);

    // A 401 is an answer: the login draws from the authentication document.
    assert!(catalog.go(&saved.url).is_err());
    assert_eq!(
        *catalog.state(),
        BrowseState::Login {
            title: "Test Shelf Login".into(),
            offers_basic: true,
            retry: saved.url.clone(),
        }
    );
    assert!(catalog.entries().is_empty());

    // Signing in stores the credential by origin and fetches again.
    catalog.sign_in("reader", "secret").expect("signed in");
    assert_eq!(*catalog.state(), BrowseState::Feed);
    assert_eq!(catalog.title(), "Test Shelf");
    assert!(catalog.has_search());
    assert_eq!(
        catalog.next_page().as_deref(),
        Some(&*format!("{HOST}/page2"))
    );
    assert_eq!(catalog.entries().len(), 2);
    let facets = catalog.facets();
    assert_eq!(
        facets.iter().map(|f| f.label.as_str()).collect::<Vec<_>>(),
        ["English", "French"]
    );
    assert!(facets[0].active);
    let key = CredentialKey::http_origin(&saved.url).expect("an origin");
    let CredentialLookup::Found(stored) = app.platform().credentials.get(&key, Freshness::Cached)
    else {
        panic!("the sign-in was stored by origin");
    };
    assert_eq!(
        stored.authorization,
        chapbook_app::chapbook_reader::chapbook_core::basic_authorization("reader", "secret")
    );

    // The screen pages before the reader taps Get: the rows accumulate,
    // and the row from page one still describes its own download — with
    // the sync service resolved against the catalog, not left relative.
    assert!(catalog.load_more().expect("page two"));
    assert_eq!(catalog.entries().len(), 3);
    assert_eq!(catalog.next_page(), None, "page two is the last");
    assert!(!catalog.load_more().expect("nothing more"));
    assert_eq!(catalog.facets().len(), 2, "facets are the first page's");
    let download = catalog.download(1).expect("a book row");
    assert_eq!(download.url, format!("{HOST}/books/minimal.epub"));
    assert_eq!(download.entry_id, "urn:book:1");
    assert_eq!(download.title, "Minimal");
    assert_eq!(
        download.progression_url.as_deref(),
        Some(&*format!("{HOST}/progress/1"))
    );
    assert_eq!(download.annotation_container, None);
    assert!(
        download
            .headers
            .iter()
            .all(|(name, _)| !name.eq_ignore_ascii_case("authorization")),
        "no credential travels in a download"
    );
    assert!(
        catalog.download(0).is_none(),
        "a navigation row has nothing to fetch"
    );
    assert!(catalog.download(2).is_none());

    // A navigation row pushes a crumb; Back walks it before leaving.
    let section = catalog.entries()[0]
        .navigation()
        .map(|link| link.href.clone())
        .expect("a section link");
    catalog.go(&section).expect("section");
    assert_eq!(catalog.title(), "The Section");
    assert_eq!(catalog.base(), section);
    assert!(catalog.back());
    assert_eq!(catalog.base(), saved.url);
    assert_eq!(catalog.title(), "Test Shelf");
    assert!(!catalog.back(), "the root is the last crumb");

    // A facet is a fetch that pushes a crumb too.
    catalog.apply_facet(1).expect("french");
    assert_eq!(catalog.base(), format!("{HOST}/fr"));
    assert!(catalog.back());

    // Search replaces the feed and moves no crumb.
    catalog.search("minimal").expect("search");
    assert!(!catalog.back(), "still at the root");

    // Every request after the sign-in carried the credential, added by
    // the layer from the store — no front end interceptor involved.
    let seen = server.seen.lock().unwrap();
    assert!(seen.len() > 4);
    assert!(seen[0].1.is_none(), "the first fetch had nothing to send");
    assert!(seen[1..].iter().all(|(_, auth)| auth.is_some()), "{seen:?}");
}

#[test]
fn a_dead_host_is_a_failure_the_screen_can_name() {
    let dir = TempDir::new("dead");
    let server = Arc::new(Server {
        seen: Mutex::new(Vec::new()),
    });
    let app = app(&dir, server);
    let mut catalog = app.open_catalog().expect("catalog");
    assert!(catalog.go("https://down.test/").is_err());
    match catalog.state() {
        BrowseState::Failed { url, reason } => {
            assert_eq!(url, "https://down.test/");
            assert!(reason.contains("nobody home"), "{reason}");
        }
        other => panic!("expected a failure, got {other:?}"),
    }
    assert!(
        catalog.sign_in("a", "b").is_err(),
        "nothing asked for a login"
    );
    assert!(catalog.search("x").is_err(), "nothing held to search");
}

#[test]
fn the_whole_download_lands_the_book_with_its_services() {
    let dir = TempDir::new("download");
    let server = Arc::new(Server {
        seen: Mutex::new(Vec::new()),
    });
    let mut app = app(&dir, server);
    let saved = app
        .add_catalog(&format!("{HOST}/opds/"), "Shelf")
        .expect("add");
    let mut catalog = app.browse(&saved).expect("browse");
    assert_eq!(
        catalog.title(),
        "Shelf",
        "the saved title until the feed says"
    );
    let _ = catalog.go(&saved.url);
    catalog.sign_in("reader", "secret").expect("signed in");
    let id = catalog
        .download_to_library(1, app.dir())
        .expect("downloaded");
    let targets = app.library().sync_targets(id).expect("targets");
    assert_eq!(
        targets.progression_url.as_deref(),
        Some(&*format!("{HOST}/progress/1"))
    );
    assert_eq!(app.shelf(&Default::default()).expect("shelf").len(), 1);
}
