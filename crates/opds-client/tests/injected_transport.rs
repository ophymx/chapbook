//! The whole crate driven through a transport that opens no sockets.
//!
//! This is the regression test for the seam: it uses no `ureq`, no TLS and
//! no network, so it is also the test that still runs under
//! `--no-default-features`. If any of it starts needing a feature, the
//! bring-your-own-HTTP promise has quietly stopped being true.

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::{Arc, Mutex};

use opds_client::http::{HttpClient, HttpError, HttpRequest, HttpResponse};
use opds_client::{OpdsClient, OpdsError};

const HOST: &str = "https://cat.example.com";

const FEED: &str = r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <id>urn:cat:private</id><title>Private Shelf</title>
  <entry><id>urn:b1</id><title>Book One</title>
    <link rel="http://opds-spec.org/acquisition/open-access" href="/dl/b1.epub" type="application/epub+zip"/>
  </entry>
</feed>"#;

const AUTH_DOC: &str = r#"{
  "title": "Private Shelf",
  "authentication": [{"type": "http://opds-spec.org/auth/basic",
                      "labels": {"login": "User", "password": "Pass"}}]
}"#;

/// A canned-response transport that records what it was asked for.
///
/// Cloning shares one recorder, so a test can keep a handle after moving a
/// copy into the client.
#[derive(Clone, Default)]
struct FakeHttp(Arc<Canned>);

/// Header name/value pairs, as `HttpRequest` carries them.
type Headers = Vec<(String, String)>;
/// What a route serves: status, content-type, body.
type CannedResponse = (u16, String, Vec<u8>);

#[derive(Default)]
struct Canned {
    routes: Mutex<HashMap<String, CannedResponse>>,
    /// Every request seen, in order, as (url, headers).
    seen: Mutex<Vec<(String, Headers)>>,
    /// Serve 401 + the Authentication Document until credentials show up.
    require_auth: Mutex<bool>,
}

impl FakeHttp {
    fn route(self, path: &str, status: u16, content_type: &str, body: &[u8]) -> Self {
        self.0.routes.lock().unwrap().insert(
            path.to_string(),
            (status, content_type.to_string(), body.to_vec()),
        );
        self
    }

    fn requiring_auth(self) -> Self {
        *self.0.require_auth.lock().unwrap() = true;
        self
    }

    fn requests(&self) -> Vec<(String, Headers)> {
        self.0.seen.lock().unwrap().clone()
    }
}

fn header_of(headers: &Headers, name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.clone())
}

fn respond(status: u16, content_type: Option<&str>, body: Vec<u8>) -> HttpResponse {
    HttpResponse {
        status,
        content_type: content_type.map(str::to_string),
        headers: Vec::new(),
        body: Box::new(Cursor::new(body)),
    }
}

impl HttpClient for FakeHttp {
    fn get(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        let authorized = header_of(&request.headers, "Authorization").is_some();
        self.0
            .seen
            .lock()
            .unwrap()
            .push((request.url.clone(), request.headers.clone()));

        if *self.0.require_auth.lock().unwrap() && !authorized {
            return Ok(respond(
                401,
                Some("application/opds-authentication+json"),
                AUTH_DOC.as_bytes().to_vec(),
            ));
        }
        let path = request.url.trim_start_matches(HOST);
        match self.0.routes.lock().unwrap().get(path) {
            Some((status, content_type, body)) => {
                Ok(respond(*status, Some(content_type), body.clone()))
            }
            None => Ok(respond(404, None, Vec::new())),
        }
    }
}

fn catalog() -> FakeHttp {
    FakeHttp::default()
        .route(
            "/opds/",
            200,
            "application/atom+xml;profile=opds-catalog",
            FEED.as_bytes(),
        )
        .route(
            "/dl/b1.epub",
            200,
            "application/epub+zip",
            b"PK\x03\x04fake",
        )
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("opds-client-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_feed_parses_with_no_networking_crate_involved() {
    let client = OpdsClient::new(catalog());
    let feed = client.fetch(&format!("{HOST}/opds/")).unwrap();

    assert_eq!(feed.title, "Private Shelf");
    assert_eq!(feed.entries.len(), 1);
    // Hrefs resolve against the request URL even through a fake transport.
    assert_eq!(feed.entries[0].links[0].href, format!("{HOST}/dl/b1.epub"));
}

/// Interop doc §1: real servers negotiate by naive substring match, so the
/// client must ask for exactly one media type and never send a q-value.
/// The transport is the only place that is observable.
#[test]
fn accept_carries_one_media_type_and_no_q_value() {
    let http = catalog();
    let client = OpdsClient::new(http.clone());
    client.fetch(&format!("{HOST}/opds/")).unwrap();

    let sent = http.requests();
    assert_eq!(sent.len(), 1);
    let accept = header_of(&sent[0].1, "Accept").expect("Accept header");
    assert_eq!(accept, "application/atom+xml");
    assert!(!accept.contains(','), "compound Accept: {accept}");
    assert!(!accept.contains(";q="), "q-value in Accept: {accept}");
}

#[test]
fn a_401_surfaces_the_authentication_document_then_credentials_retry() {
    let http = catalog().requiring_auth();
    let mut client = OpdsClient::new(http.clone());

    let err = client.fetch(&format!("{HOST}/opds/")).unwrap_err();
    let OpdsError::AuthRequired(Some(doc)) = err else {
        panic!("expected AuthRequired carrying a document, got {err:?}");
    };
    assert_eq!(doc.title, "Private Shelf");
    assert!(doc.basic_flow().is_some());

    client.set_basic_auth("user", "pw");
    let feed = client.fetch(&format!("{HOST}/opds/")).unwrap();
    assert_eq!(feed.title, "Private Shelf");

    // The retry carried credentials; the first attempt did not.
    let sent = http.requests();
    assert!(header_of(&sent[0].1, "Authorization").is_none());
    assert_eq!(
        header_of(&sent[1].1, "Authorization").as_deref(),
        Some("Basic dXNlcjpwdw==")
    );
}

#[test]
fn a_scheme_this_crate_has_never_heard_of_rides_through_verbatim() {
    // The credential is an opaque header value, so a bearer token — or
    // anything else a host's store produces — needs no code here. This is
    // the test that keeps it that way: if the crate ever starts parsing
    // the scheme, this is what breaks.
    let http = catalog().requiring_auth();
    let mut client = OpdsClient::new(http.clone());

    client.set_authorization("Bearer eyJhbGciOiJub25lIn0.e30.");
    let feed = client.fetch(&format!("{HOST}/opds/")).unwrap();
    assert_eq!(feed.title, "Private Shelf");

    let sent = http.requests();
    assert_eq!(
        header_of(&sent[0].1, "Authorization").as_deref(),
        Some("Bearer eyJhbGciOiJub25lIn0.e30."),
        "sent unaltered, scheme word and all"
    );

    // And clearing it puts the flow back where it started.
    client.clear_authorization();
    assert!(matches!(
        client.fetch(&format!("{HOST}/opds/")),
        Err(OpdsError::AuthRequired(_))
    ));
}

#[test]
fn one_transport_serves_several_clients() {
    // What a host with a single background URLSession needs: two clients
    // (chapbook builds a fresh one per auth attempt) over one transport,
    // not two transports.
    let fake = catalog().requiring_auth();
    let http: Arc<dyn HttpClient> = Arc::new(fake.clone());

    let anonymous = OpdsClient::new(http.clone());
    assert!(matches!(
        anonymous.fetch(&format!("{HOST}/opds/")),
        Err(OpdsError::AuthRequired(_))
    ));

    let mut authorized = OpdsClient::new(http.clone());
    authorized.set_basic_auth("user", "pw");
    assert_eq!(
        authorized.fetch(&format!("{HOST}/opds/")).unwrap().title,
        "Private Shelf"
    );

    // One recorder saw both requests, so both clients shared one transport.
    let sent = fake.requests();
    assert_eq!(sent.len(), 2);
    assert!(header_of(&sent[0].1, "Authorization").is_none());
    assert!(header_of(&sent[1].1, "Authorization").is_some());
}

#[test]
fn the_default_download_lands_complete_and_leaves_no_temp_file() {
    let dir = scratch("inject");
    let dest = dir.join("b1.epub");

    let client = OpdsClient::new(catalog());
    client
        .download(&format!("{HOST}/dl/b1.epub"), &dest)
        .unwrap();

    assert_eq!(std::fs::read(&dest).unwrap(), b"PK\x03\x04fake");
    assert!(
        !dest.with_extension("part").exists(),
        "temp file must be renamed away"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_non_success_download_status_writes_nothing() {
    let dir = scratch("404");
    let dest = dir.join("missing.epub");

    let client = OpdsClient::new(catalog());
    let err = client
        .download(&format!("{HOST}/dl/nope.epub"), &dest)
        .unwrap_err();

    assert!(matches!(err, OpdsError::Http(404)), "got {err:?}");
    assert!(!dest.exists(), "nothing should have been written");
    assert!(!dest.with_extension("part").exists());
    std::fs::remove_dir_all(&dir).ok();
}
