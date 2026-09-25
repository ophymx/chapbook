//! The container protocol against a scripted transport: what each status
//! means, and that the entity tag actually rides along.

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::{Arc, Mutex};

use chapbook_annotations::container::AnnotationContainer;
use chapbook_annotations::http::{
    header, Body, HttpClient, HttpError, HttpRequest, HttpResponse, Method, Response,
};
use chapbook_annotations::model::Annotation;
use chapbook_annotations::ContainerError;
use serde_json::json;

const HOST: &str = "https://library.example.com";

/// A scripted response: status, headers, body.
type CannedResponse = (u16, Vec<(String, String)>, String);

/// One request as the fake saw it: method, path, headers, body.
type SeenRequest = (String, String, Vec<(String, String)>, String);

#[derive(Clone, Default)]
struct FakeHttp(Arc<Inner>);

#[derive(Default)]
struct Inner {
    routes: Mutex<HashMap<String, CannedResponse>>,
    seen: Mutex<Vec<SeenRequest>>,
}

impl FakeHttp {
    fn on(
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

    fn seen(&self) -> Vec<SeenRequest> {
        self.0.seen.lock().unwrap().clone()
    }

    fn serve(&self, request: &HttpRequest) -> HttpResponse {
        let method = request.method().to_string();
        let path = request
            .uri()
            .to_string()
            .trim_start_matches(HOST)
            .to_string();
        self.0.seen.lock().unwrap().push((
            method.clone(),
            path.clone(),
            request
                .headers()
                .iter()
                .filter_map(|(n, v)| Some((n.to_string(), v.to_str().ok()?.to_string())))
                .collect(),
            String::from_utf8_lossy(request.body()).into_owned(),
        ));
        match self
            .0
            .routes
            .lock()
            .unwrap()
            .get(&format!("{method} {path}"))
        {
            Some((status, headers, body)) => {
                let mut response = Response::builder()
                    .status(*status)
                    .header(header::CONTENT_TYPE, "application/ld+json");
                for (name, value) in headers {
                    response = response.header(name.as_str(), value.as_str());
                }
                response
                    .body(Box::new(Cursor::new(body.clone().into_bytes())) as Body)
                    .unwrap()
            }
            None => Response::builder()
                .status(404)
                .body(Box::new(Cursor::new(Vec::new())) as Body)
                .unwrap(),
        }
    }
}

impl HttpClient for FakeHttp {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        Ok(self.serve(&request))
    }
}

fn annotation(id: Option<&str>) -> serde_json::Value {
    let mut value = json!({
        "@context": "http://www.w3.org/ns/anno.jsonld",
        "type": "Annotation",
        "motivation": "highlighting",
        "target": {"type": "SpecificResource", "source": "urn:book",
                   "selector": [{"type": "TextQuoteSelector", "exact": "hi"}]}
    });
    if let Some(id) = id {
        value["id"] = json!(id);
    }
    value
}

fn header_of(headers: &[(String, String)], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.clone())
}

/// Creation is the one flow that learns an identity, and `Location` is
/// where a container states it.
#[test]
fn create_takes_the_iri_from_location_and_keeps_the_etag() {
    let http = FakeHttp::default().on(
        "POST",
        "/annotations/",
        201,
        &[
            ("Location", "https://library.example.com/annotations/abc"),
            ("ETag", "\"v1\""),
        ],
        &annotation(Some("https://library.example.com/annotations/abc")).to_string(),
    );
    let container = AnnotationContainer::new(http.clone());
    let document: Annotation = serde_json::from_value(annotation(None)).unwrap();

    let stored = container
        .create(&format!("{HOST}/annotations/"), &document)
        .unwrap();
    assert_eq!(stored.iri, "https://library.example.com/annotations/abc");
    assert_eq!(stored.etag.as_deref(), Some("\"v1\""));

    let (method, _, headers, body) = http.seen().remove(0);
    assert_eq!(method, "POST");
    assert!(
        header_of(&headers, "Content-Type")
            .unwrap()
            .contains("application/ld+json"),
        "{headers:?}"
    );
    assert!(body.contains("TextQuoteSelector"), "{body}");
}

/// A container that returns the document but no `Location` still says
/// where it put it — in the document's own `id`.
#[test]
fn create_falls_back_to_the_documents_own_id() {
    let http = FakeHttp::default().on(
        "POST",
        "/annotations/",
        201,
        &[],
        &annotation(Some("https://library.example.com/annotations/xyz")).to_string(),
    );
    let stored = AnnotationContainer::new(http)
        .create(
            &format!("{HOST}/annotations/"),
            &serde_json::from_value(annotation(None)).unwrap(),
        )
        .unwrap();
    assert_eq!(stored.iri, "https://library.example.com/annotations/xyz");
}

/// A relative `Location` resolves against the container it was created in.
#[test]
fn a_relative_location_resolves_against_the_container() {
    let http = FakeHttp::default().on(
        "POST",
        "/annotations/",
        201,
        &[("Location", "abc")],
        &annotation(None).to_string(),
    );
    let stored = AnnotationContainer::new(http)
        .create(
            &format!("{HOST}/annotations/"),
            &serde_json::from_value(annotation(None)).unwrap(),
        )
        .unwrap();
    assert_eq!(stored.iri, "https://library.example.com/annotations/abc");
}

/// The whole point of the entity tag: it has to leave as `If-Match`.
#[test]
fn an_update_sends_the_tag_it_was_read_with() {
    let http = FakeHttp::default().on(
        "PUT",
        "/annotations/abc",
        200,
        &[("ETag", "\"v2\"")],
        &annotation(Some("https://library.example.com/annotations/abc")).to_string(),
    );
    let container = AnnotationContainer::new(http.clone());
    let updated = container
        .update(
            &format!("{HOST}/annotations/abc"),
            &serde_json::from_value(annotation(None)).unwrap(),
            Some("\"v1\""),
        )
        .unwrap();
    assert_eq!(updated.etag.as_deref(), Some("\"v2\""));

    let (_, _, headers, _) = http.seen().remove(0);
    assert_eq!(header_of(&headers, "If-Match").as_deref(), Some("\"v1\""));
}

/// Two devices editing one highlight is the ordinary case, not a failure.
/// A container that sends its copy along lets the caller merge.
#[test]
fn a_stale_tag_is_a_conflict_carrying_the_servers_copy() {
    let http = FakeHttp::default().on(
        "PUT",
        "/annotations/abc",
        412,
        &[],
        &annotation(Some("https://library.example.com/annotations/abc")).to_string(),
    );
    match AnnotationContainer::new(http).update(
        &format!("{HOST}/annotations/abc"),
        &serde_json::from_value(annotation(None)).unwrap(),
        Some("\"stale\""),
    ) {
        Err(ContainerError::Conflict { current: Some(_) }) => {}
        other => panic!("expected a conflict with the server's copy, got {other:?}"),
    }
}

/// And one that sends nothing is still a conflict — the caller re-reads.
#[test]
fn a_conflict_without_a_body_is_still_a_conflict() {
    let http = FakeHttp::default().on("PUT", "/annotations/abc", 412, &[], "");
    match AnnotationContainer::new(http).update(
        &format!("{HOST}/annotations/abc"),
        &serde_json::from_value(annotation(None)).unwrap(),
        Some("\"stale\""),
    ) {
        Err(ContainerError::Conflict { current: None }) => {}
        other => panic!("expected a bare conflict, got {other:?}"),
    }
}

/// Deleting something already gone is the outcome the caller wanted.
#[test]
fn deleting_an_already_deleted_annotation_is_not_a_failure() {
    let http = FakeHttp::default().on("DELETE", "/annotations/gone", 404, &[], "");
    AnnotationContainer::new(http)
        .delete(&format!("{HOST}/annotations/gone"), None)
        .expect("a 404 on delete means it is not there, which is the point");
}

/// A tombstoned IRI is permanent: the local record must stop pointing at
/// it rather than retrying forever.
#[test]
fn a_tombstoned_iri_reads_as_gone() {
    let http = FakeHttp::default().on("GET", "/annotations/gone", 410, &[], "");
    match AnnotationContainer::new(http).get(&format!("{HOST}/annotations/gone")) {
        Err(ContainerError::Gone) => {}
        other => panic!("expected gone, got {other:?}"),
    }
}

#[test]
fn a_challenge_is_reported_as_one() {
    let http = FakeHttp::default().on("GET", "/annotations/abc", 401, &[], "");
    match AnnotationContainer::new(http).get(&format!("{HOST}/annotations/abc")) {
        Err(ContainerError::Unauthorized) => {}
        other => panic!("expected unauthorized, got {other:?}"),
    }
}

#[test]
fn the_authorization_is_sent_and_is_never_inspected() {
    let http = FakeHttp::default().on(
        "GET",
        "/annotations/abc",
        200,
        &[],
        &annotation(Some("urn:a")).to_string(),
    );
    let mut container = AnnotationContainer::new(http.clone());
    container.set_authorization("Bearer opaque-token-value");
    container.get(&format!("{HOST}/annotations/abc")).unwrap();
    let (_, _, headers, _) = http.seen().remove(0);
    assert_eq!(
        header_of(&headers, "Authorization").as_deref(),
        Some("Bearer opaque-token-value")
    );
}

/// A container states where its items are; walking it should not make the
/// caller know whether they were embedded or a hop away.
#[test]
fn a_collection_is_followed_to_its_first_page_and_then_paged() {
    let http = FakeHttp::default()
        .on(
            "GET",
            "/annotations/",
            200,
            &[],
            &json!({"type": ["BasicContainer", "AnnotationCollection"],
                    "total": 2,
                    "first": "https://library.example.com/annotations/?page=0"})
            .to_string(),
        )
        .on(
            "GET",
            "/annotations/?page=0",
            200,
            &[],
            &json!({"type": "AnnotationPage",
                    "items": [annotation(Some("urn:a"))],
                    "next": "https://library.example.com/annotations/?page=1"})
            .to_string(),
        )
        .on(
            "GET",
            "/annotations/?page=1",
            200,
            &[],
            &json!({"type": "AnnotationPage", "items": [annotation(Some("urn:b"))]}).to_string(),
        );

    let listing = AnnotationContainer::new(http)
        .all(&format!("{HOST}/annotations/"), None)
        .unwrap();
    assert_eq!(
        listing
            .items
            .iter()
            .map(|s| s.iri.as_str())
            .collect::<Vec<_>>(),
        vec!["urn:a", "urn:b"]
    );
    assert!(listing.complete, "the chain ended, so the walk was whole");
}

/// A container is somebody else's, and a `next` that points at itself must
/// not be an infinite loop.
#[test]
fn a_page_that_points_at_itself_terminates() {
    let http = FakeHttp::default().on(
        "GET",
        "/annotations/",
        200,
        &[],
        &json!({"type": "AnnotationPage",
                "items": [annotation(Some("urn:a"))],
                "next": "https://library.example.com/annotations/"})
        .to_string(),
    );
    let listing = AnnotationContainer::new(http)
        .all(&format!("{HOST}/annotations/"), None)
        .unwrap();
    assert_eq!(listing.items.len(), 1);
    assert!(
        !listing.complete,
        "a page pointing at itself is a chain that was never followed to an end"
    );
}

/// And a long chain is capped when the caller asked for a cap.
#[test]
fn the_page_limit_is_honoured() {
    let http = FakeHttp::default()
        .on(
            "GET",
            "/annotations/",
            200,
            &[],
            &json!({"items": [annotation(Some("urn:a"))],
                    "next": "https://library.example.com/annotations/?page=1"})
            .to_string(),
        )
        .on(
            "GET",
            "/annotations/?page=1",
            200,
            &[],
            &json!({"items": [annotation(Some("urn:b"))],
                    "next": "https://library.example.com/annotations/?page=2"})
            .to_string(),
        );
    let listing = AnnotationContainer::new(http)
        .all(&format!("{HOST}/annotations/"), Some(1))
        .unwrap();
    assert_eq!(listing.items.len(), 1, "the cap was not honoured");
    assert!(
        !listing.complete,
        "a capped walk must say it saw only a prefix — a caller that reads \
         absence as deletion is trusting exactly this flag"
    );
}

/// A transport that cannot write says so rather than reporting success.
/// There is one method on the trait, so "cannot write" is a transport
/// refusing by verb — which is what the trait asks of a read-only host.
#[test]
fn a_read_only_transport_refuses_the_write_instead_of_dropping_it() {
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
    match AnnotationContainer::new(GetOnly).create(
        &format!("{HOST}/annotations/"),
        &serde_json::from_value(annotation(None)).unwrap(),
    ) {
        Err(ContainerError::Network(message)) => assert!(message.contains("POST"), "{message}"),
        other => panic!("expected a transport refusal, got {other:?}"),
    }
}

/// A container may answer with bare IRIs rather than annotation bodies.
/// Silently reading that as an empty container is the failure shape that
/// looks like success, so the IRIs are followed.
#[test]
fn a_container_that_serves_iris_is_followed_rather_than_read_as_empty() {
    let http = FakeHttp::default()
        .on(
            "GET",
            "/annotations/",
            200,
            &[],
            &json!({"type": "AnnotationPage",
                    "items": ["https://library.example.com/annotations/a",
                              "https://library.example.com/annotations/b"]})
            .to_string(),
        )
        .on(
            "GET",
            "/annotations/a",
            200,
            &[],
            &annotation(Some("https://library.example.com/annotations/a")).to_string(),
        )
        .on(
            "GET",
            "/annotations/b",
            200,
            &[],
            &annotation(Some("https://library.example.com/annotations/b")).to_string(),
        );

    let listing = AnnotationContainer::new(http)
        .all(&format!("{HOST}/annotations/"), None)
        .unwrap();
    assert_eq!(listing.items.len(), 2, "an IRI listing was read as empty");
    assert_eq!(listing.items[0].annotation.target.source, "urn:book");
}

/// And the request says which it wants, so following should rarely be
/// needed.
#[test]
fn a_container_read_asks_for_the_annotations_themselves() {
    let http = FakeHttp::default().on(
        "GET",
        "/annotations/",
        200,
        &[],
        &json!({"type": "AnnotationPage", "items": []}).to_string(),
    );
    AnnotationContainer::new(http.clone())
        .all(&format!("{HOST}/annotations/"), None)
        .unwrap();
    let (_, _, headers, _) = http.seen().remove(0);
    let prefer = header_of(&headers, "Prefer").unwrap_or_default();
    assert!(
        prefer.contains("PreferContainedDescriptions"),
        "the read did not say what it can use: {prefer:?}"
    );
}
