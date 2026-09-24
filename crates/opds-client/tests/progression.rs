//! OPDS Progression 1.0 driven over a scripted transport.
//!
//! Separate from `injected_transport.rs` on purpose: that file is the
//! regression test for the bring-your-own-HTTP promise and must keep
//! passing under `--no-default-features`, so the feature-gated flows live
//! here instead of teaching it a feature.
//!
//! The documents below are the draft's own examples, copied verbatim, so a
//! spec revision shows up as a test failure rather than as a shrug.

#![cfg(feature = "progression")]

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::{Arc, Mutex};

use opds_client::http::{
    header, Body, HttpClient, HttpError, HttpRequest, HttpResponse, Method, Response,
};
use opds_client::progression::{
    Device, Progression, ProgressionUpdate, RefusalReason, MEDIA_TYPE_PROGRESSION,
};
use opds_client::{OpdsClient, OpdsError};

const HOST: &str = "https://example.com";
const SERVICE: &str = "/019c0435-5361-7e59-89b7-4ee01a6d87b8/progression";

/// Draft example 3: progression in a reflowable EPUB.
const DOCUMENT: &str = r#"{
  "title": "Chapter 1 - A New Dawn",
  "modified": "2026-01-27T11:00:00Z",
  "device": {
    "id": "urn:uuid:019c0047-cc8d-7ec4-a3c3-938ccadc020a",
    "name": "Ebook Reader (Pixel 10 Pro)"
  },
  "progression": 0.0174920,
  "references": ["chapter1.html#:~:text=It%20was%20expected"]
}"#;

/// Draft example 10: the Problem Details object on a refusal.
const STALE_PROBLEM: &str = r#"{
  "type": "https://registry.opds.io/error#progression-date",
  "title": "A more recent progression point is already available."
}"#;

const AUTH_DOC: &str = r#"{
  "title": "Private Shelf",
  "authentication": [{"type": "http://opds-spec.org/auth/basic"}]
}"#;

/// An OPDS 2.0 publication document advertising a progression service —
/// draft example 1's link, in the document it would arrive in.
const PUBLICATION: &str = r#"{
  "metadata": {"title": "A Book"},
  "links": [
    {"href": "/019c0435-5361-7e59-89b7-4ee01a6d87b8/progression",
     "type": "application/opds-progression+json",
     "rel": "http://opds-spec.org/progression"}
  ]
}"#;

/// An Atom acquisition feed carrying the same link on an entry.
const FEED: &str = r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <id>urn:cat</id><title>Shelf</title>
  <entry><id>urn:b1</id><title>A Book</title>
    <link rel="http://opds-spec.org/acquisition" href="/dl/b1.epub" type="application/epub+zip"/>
    <link rel="http://opds-spec.org/progression"
          href="/019c0435-5361-7e59-89b7-4ee01a6d87b8/progression"
          type="application/opds-progression+json"/>
  </entry>
</feed>"#;

type Headers = Vec<(String, String)>;
/// What a route serves: status, content-type, body.
type CannedResponse = (u16, String, String);

/// One request as the transport saw it.
#[derive(Clone)]
struct Seen {
    method: String,
    url: String,
    headers: Headers,
    body: Vec<u8>,
}

/// A canned-response transport that records method, url, headers and body.
#[derive(Clone, Default)]
struct FakeHttp(Arc<Canned>);

#[derive(Default)]
struct Canned {
    get_routes: Mutex<HashMap<String, CannedResponse>>,
    put_routes: Mutex<HashMap<String, CannedResponse>>,
    /// Every request seen, in order.
    seen: Mutex<Vec<Seen>>,
}

impl FakeHttp {
    fn on_get(self, path: &str, status: u16, content_type: &str, body: &str) -> Self {
        self.0.get_routes.lock().unwrap().insert(
            path.to_string(),
            (status, content_type.to_string(), body.to_string()),
        );
        self
    }

    fn on_put(self, path: &str, status: u16, content_type: &str, body: &str) -> Self {
        self.0.put_routes.lock().unwrap().insert(
            path.to_string(),
            (status, content_type.to_string(), body.to_string()),
        );
        self
    }

    fn seen(&self) -> Vec<Seen> {
        self.0.seen.lock().unwrap().clone()
    }

    fn record(&self, request: &HttpRequest) {
        self.0.seen.lock().unwrap().push(Seen {
            method: request.method().to_string(),
            url: request.uri().to_string(),
            headers: request
                .headers()
                .iter()
                .filter_map(|(n, v)| Some((n.to_string(), v.to_str().ok()?.to_string())))
                .collect(),
            body: request.body().clone(),
        });
    }

    fn serve(routes: &Mutex<HashMap<String, CannedResponse>>, url: &str) -> HttpResponse {
        let path = url.trim_start_matches(HOST);
        match routes.lock().unwrap().get(path) {
            Some((status, content_type, body)) => Response::builder()
                .status(*status)
                .header(header::CONTENT_TYPE, content_type)
                .body(Box::new(Cursor::new(body.clone().into_bytes())) as Body)
                .unwrap(),
            None => Response::builder()
                .status(404)
                .body(Box::new(Cursor::new(Vec::new())) as Body)
                .unwrap(),
        }
    }
}

impl HttpClient for FakeHttp {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        self.record(&request);
        let url = request.uri().to_string();
        match *request.method() {
            Method::GET => Ok(Self::serve(&self.0.get_routes, &url)),
            Method::PUT => Ok(Self::serve(&self.0.put_routes, &url)),
            ref other => panic!("progression only ever GETs and PUTs, not {other}"),
        }
    }
}

fn header_of(headers: &Headers, name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.clone())
}

fn sample() -> Progression {
    Progression {
        title: Some("Chapter 1 - A New Dawn".into()),
        modified: "2026-01-27T11:00:00Z".into(),
        device: Device {
            id: "urn:uuid:019c0047-cc8d-7ec4-a3c3-938ccadc020a".into(),
            name: "Ebook Reader (Pixel 10 Pro)".into(),
        },
        progression: 0.017_492,
        references: vec!["chapter1.html#:~:text=It%20was%20expected".into()],
    }
}

fn url() -> String {
    format!("{HOST}{SERVICE}")
}

#[test]
fn fetch_parses_the_drafts_own_example() {
    let http = FakeHttp::default().on_get(SERVICE, 200, MEDIA_TYPE_PROGRESSION, DOCUMENT);
    let client = OpdsClient::new(http.clone());

    let progression = client.fetch_progression(&url()).unwrap().unwrap();

    assert_eq!(progression, sample());
    // One Accept media type, no q-values — the crate's rule everywhere.
    let accept = header_of(&http.seen()[0].headers, "Accept");
    assert_eq!(accept.as_deref(), Some(MEDIA_TYPE_PROGRESSION));
}

#[test]
fn empty_body_means_nothing_recorded_yet_not_a_parse_failure() {
    for body in ["", "\n", "   "] {
        let http = FakeHttp::default().on_get(SERVICE, 200, MEDIA_TYPE_PROGRESSION, body);
        let client = OpdsClient::new(http);
        assert_eq!(client.fetch_progression(&url()).unwrap(), None);
    }
}

#[test]
fn fetch_surfaces_the_authentication_document() {
    let http = FakeHttp::default().on_get(
        SERVICE,
        401,
        "application/opds-authentication+json",
        AUTH_DOC,
    );
    let client = OpdsClient::new(http);

    match client.fetch_progression(&url()) {
        Err(OpdsError::AuthRequired(Some(doc))) => assert_eq!(doc.title, "Private Shelf"),
        other => panic!("expected an authentication document, got {other:?}"),
    }
}

#[test]
fn put_sends_the_document_with_both_media_type_headers() {
    let http = FakeHttp::default().on_put(SERVICE, 200, MEDIA_TYPE_PROGRESSION, DOCUMENT);
    let mut client = OpdsClient::new(http.clone());
    client.set_basic_auth("reader", "pw");

    let outcome = client.put_progression(&url(), &sample()).unwrap();

    assert_eq!(outcome, ProgressionUpdate::Stored(Some(sample())));
    let request = http.seen().remove(0);
    assert_eq!(request.method, "PUT");
    assert_eq!(request.url, url());
    assert_eq!(
        header_of(&request.headers, "Content-Type").as_deref(),
        Some(MEDIA_TYPE_PROGRESSION)
    );
    assert_eq!(
        header_of(&request.headers, "Accept").as_deref(),
        Some(MEDIA_TYPE_PROGRESSION)
    );
    assert!(header_of(&request.headers, "Authorization").is_some());
    // The body is a Progression Document that round-trips.
    let sent: Progression = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(sent, sample());
}

#[test]
fn optional_fields_are_omitted_rather_than_sent_null() {
    let http = FakeHttp::default().on_put(SERVICE, 201, MEDIA_TYPE_PROGRESSION, "");
    let client = OpdsClient::new(http.clone());
    let minimal = Progression {
        title: None,
        references: Vec::new(),
        ..sample()
    };

    let outcome = client.put_progression(&url(), &minimal).unwrap();

    assert_eq!(outcome, ProgressionUpdate::Created(None));
    let body = String::from_utf8(http.seen().remove(0).body).unwrap();
    assert!(!body.contains("title"), "sent {body}");
    assert!(!body.contains("references"), "sent {body}");
}

#[test]
fn conflict_is_a_refusal_carrying_the_problem_details() {
    let http = FakeHttp::default().on_put(SERVICE, 409, "application/problem+json", STALE_PROBLEM);
    let client = OpdsClient::new(http);

    let refusal = match client.put_progression(&url(), &sample()).unwrap() {
        ProgressionUpdate::Refused(refusal) => refusal,
        other => panic!("expected a refusal, got {other:?}"),
    };

    assert_eq!(refusal.reason, RefusalReason::Stale);
    assert_eq!(refusal.status, 409);
    assert_eq!(
        refusal.title.as_deref(),
        Some("A more recent progression point is already available.")
    );
}

/// The two 403s share a status, so only the Problem Details `type` can tell
/// them apart — and a server that sends none leaves it genuinely unknown.
#[test]
fn the_two_forbidden_variants_are_separated_by_type_alone() {
    let cases = [
        (
            r#"{"type": "https://registry.opds.io/error#progression-locked", "title": "Locked"}"#,
            RefusalReason::Locked,
        ),
        (
            r#"{"type": "https://registry.opds.io/error#progression-incorrect-user", "title": "Nope"}"#,
            RefusalReason::IncorrectUser,
        ),
        ("", RefusalReason::Unknown),
    ];

    for (body, expected) in cases {
        let http = FakeHttp::default().on_put(SERVICE, 403, "application/problem+json", body);
        let client = OpdsClient::new(http);
        match client.put_progression(&url(), &sample()).unwrap() {
            ProgressionUpdate::Refused(refusal) => assert_eq!(refusal.reason, expected),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }
}

#[test]
fn bad_request_is_a_refusal_but_a_server_error_is_an_error() {
    let http = FakeHttp::default().on_put(SERVICE, 400, "application/problem+json", "");
    match OpdsClient::new(http).put_progression(&url(), &sample()) {
        Ok(ProgressionUpdate::Refused(r)) => assert_eq!(r.reason, RefusalReason::InvalidPayload),
        other => panic!("expected a refusal, got {other:?}"),
    }

    let http = FakeHttp::default().on_put(SERVICE, 503, "text/plain", "down");
    match OpdsClient::new(http).put_progression(&url(), &sample()) {
        Err(OpdsError::Http(503)) => {}
        other => panic!("expected an HTTP error, got {other:?}"),
    }
}

#[test]
fn an_out_of_range_progression_never_reaches_the_network() {
    for bad in [-0.1, 1.5, f64::NAN] {
        let http = FakeHttp::default().on_put(SERVICE, 200, MEDIA_TYPE_PROGRESSION, DOCUMENT);
        let client = OpdsClient::new(http.clone());
        let out_of_range = Progression {
            progression: bad,
            ..sample()
        };

        assert!(matches!(
            client.put_progression(&url(), &out_of_range),
            Err(OpdsError::Parse(_))
        ));
        assert!(http.seen().is_empty(), "{bad} was sent anyway");
    }
}

#[test]
fn a_transport_that_cannot_write_says_so_instead_of_dropping_the_write() {
    /// A read-only transport: it refuses every method but GET, by name,
    /// which is what the trait asks of a host that only browses.
    struct GetOnly;
    impl HttpClient for GetOnly {
        fn send(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
            if request.method() == Method::GET {
                unreachable!("the flow under test never gets this far");
            }
            Err(HttpError::new(format!(
                "read-only transport cannot {}",
                request.method()
            )))
        }
    }

    match OpdsClient::new(GetOnly).put_progression(&url(), &sample()) {
        Err(OpdsError::Network(message)) => assert!(message.contains("PUT"), "{message}"),
        other => panic!("expected a transport refusal, got {other:?}"),
    }
}

#[test]
fn the_service_is_discovered_from_either_dialect() {
    let http = FakeHttp::default()
        .on_get(
            "/pub",
            200,
            "application/opds-publication+json",
            PUBLICATION,
        )
        .on_get("/feed", 200, "application/atom+xml", FEED);
    let client = OpdsClient::new(http);
    let expected = url();

    // 2.0: on the publication document itself.
    let publication = client.fetch(&format!("{HOST}/pub")).unwrap();
    assert_eq!(publication.progression().unwrap().href, expected);

    // 1.2: on the entry, alongside the acquisition link.
    let feed = client.fetch(&format!("{HOST}/feed")).unwrap();
    let entry = &feed.entries[0];
    assert_eq!(entry.progression().unwrap().href, expected);
    // …and it is not mistaken for something to download.
    assert_eq!(entry.acquisitions().count(), 1);
}
