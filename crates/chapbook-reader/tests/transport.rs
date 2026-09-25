//! The session's transport seam, and the credential retry that rides on it.
//!
//! This is the test `SessionConfig`'s credential half could not have: until
//! the session took a transport, the OPDS path built its own `ureq` client
//! and the only way to reach `Session::open_with`'s 401 handling was over a
//! real network. Now the whole flow — cached credential rejected, store
//! asked again with `Freshness::Renewed`, one retry, success — runs in
//! process with no socket open.
#![cfg(all(feature = "opds", feature = "cbz"))]

use std::io::Cursor;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chapbook_core::{
    Credential, CredentialKey, CredentialLookup, CredentialStore, Freshness, NoCredentials,
};
use chapbook_reader::chapbook_opds::http::{header, Response};
use chapbook_reader::{
    Body, HttpClient, HttpError, HttpRequest, HttpResponse, Session, SessionConfig,
};

const HOST: &str = "https://comics.example.com";
const STALE: &str = "Bearer stale-token";
const FRESH: &str = "Bearer fresh-token";

/// A feed whose entry carries the PSE stream directly, so opening is one
/// fetch and the test is about auth rather than about lazy resolution.
fn feed() -> String {
    format!(
        r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom" xmlns:pse="http://vaemendis.net/opds-pse/ns">
  <id>urn:cat:comics</id><title>Comics</title>
  <entry><id>urn:c1</id><title>Retry Comic</title>
    <author><name>Ada Fixture</name></author>
    <link rel="http://vaemendis.net/opds-pse/stream"
          href="{HOST}/pages?page={{pageNumber}}&amp;width={{maxWidth}}"
          type="image/png" pse:count="3"/>
  </entry>
</feed>"#
    )
}

const AUTH_DOC: &str = r#"{
  "title": "Comics",
  "authentication": [{"type": "http://opds-spec.org/auth/basic"}]
}"#;

/// A transport that accepts exactly one credential, and records every
/// `Authorization` value it was offered.
#[derive(Clone, Default)]
struct PickyHttp(Arc<Offered>);

#[derive(Default)]
struct Offered {
    seen: Mutex<Vec<Option<String>>>,
}

impl PickyHttp {
    fn offered(&self) -> Vec<Option<String>> {
        self.0.seen.lock().unwrap().clone()
    }
}

fn respond(status: u16, content_type: &str, body: Vec<u8>) -> HttpResponse {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .body(Box::new(Cursor::new(body)) as Body)
        .expect("a well-formed canned response")
}

impl HttpClient for PickyHttp {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        let offered = request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        self.0.seen.lock().unwrap().push(offered.clone());

        if offered.as_deref() != Some(FRESH) {
            return Ok(respond(
                401,
                "application/opds-authentication+json",
                AUTH_DOC.as_bytes().to_vec(),
            ));
        }
        Ok(respond(200, "application/atom+xml", feed().into_bytes()))
    }
}

/// A store whose secret expires: the cached value is the one the server
/// stopped accepting, and only a `Renewed` lookup produces the live one.
///
/// This is the shape `Freshness` exists for — an OAuth store refreshing
/// against its provider — reduced to the part the engine can see.
#[derive(Default)]
struct RefreshingStore {
    renewals: AtomicUsize,
}

impl CredentialStore for RefreshingStore {
    fn get(&self, _key: &CredentialKey, freshness: Freshness) -> CredentialLookup {
        match freshness {
            Freshness::Cached => CredentialLookup::Found(Credential::new(STALE)),
            Freshness::Renewed => {
                self.renewals.fetch_add(1, Ordering::SeqCst);
                CredentialLookup::Found(Credential::new(FRESH))
            }
        }
    }
}

/// A store that has the credential but cannot read it right now.
struct LockedStore;

impl CredentialStore for LockedStore {
    fn get(&self, _key: &CredentialKey, _freshness: Freshness) -> CredentialLookup {
        CredentialLookup::Locked
    }
}

fn isolated_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "chapbook-transport-test-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn fixture_fonts() -> chapbook_core::FontSource {
    chapbook_core::FontSource::embedded(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/fonts"),
        "Crimson Text",
    )
}

#[test]
fn a_rejected_credential_is_renewed_and_the_open_retries() {
    let dir = isolated_dir("renew");

    let http = PickyHttp::default();
    let store = Arc::new(RefreshingStore::default());
    let config = SessionConfig::new(fixture_fonts())
        .with_library_dir(&dir)
        .with_credentials(store.clone())
        .with_transport(Arc::new(http.clone()));

    let session = Session::open_with(format!("{HOST}/opds/"), config).unwrap();
    assert_eq!(session.spine_len(), 3, "the PSE stream's three pages");

    // The store was asked twice: once cheaply, once because the server
    // said no. One renewal, not a loop.
    assert_eq!(store.renewals.load(Ordering::SeqCst), 1);

    // And the transport was offered the stale value first, then the fresh
    // one — the header verbatim both times, with nothing in the session
    // inspecting the scheme.
    let offered = http.offered();
    assert_eq!(offered.first().unwrap().as_deref(), Some(STALE));
    assert!(
        offered.iter().any(|o| o.as_deref() == Some(FRESH)),
        "the retry carried the renewed credential: {offered:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_store_with_nothing_to_offer_surfaces_the_authentication_document() {
    let dir = isolated_dir("none");

    let http = PickyHttp::default();
    let config = SessionConfig::new(fixture_fonts())
        .with_library_dir(&dir)
        .with_credentials(Arc::new(NoCredentials))
        .with_transport(Arc::new(http.clone()));

    let Err(err) = Session::open_with(format!("{HOST}/opds/"), config) else {
        panic!("a session with no credentials must not open a private catalog");
    };

    // The shell needs the server's document to build a login dialog from,
    // so the error must carry it rather than collapsing to "failed".
    let message = err.to_string();
    assert!(message.contains("authentication"), "{message}");

    // One attempt, not a retry: there was nothing new to try with.
    assert_eq!(http.offered().len(), 1, "{:?}", http.offered());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_locked_store_does_not_spend_a_retry() {
    let dir = isolated_dir("locked");

    let http = PickyHttp::default();
    let config = SessionConfig::new(fixture_fonts())
        .with_library_dir(&dir)
        .with_credentials(Arc::new(LockedStore))
        .with_transport(Arc::new(http.clone()));

    assert!(
        Session::open_with(format!("{HOST}/opds/"), config).is_err(),
        "a locked store is not a credential"
    );

    // Locked is not Missing, but it is not a credential either: the
    // request goes out unauthenticated once and stops. Re-prompting the
    // user is the shell's call, and it needs to know the difference.
    let offered = http.offered();
    assert_eq!(offered.len(), 1, "{offered:?}");
    assert_eq!(offered[0], None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_local_book_needs_no_transport_at_all() {
    let dir = isolated_dir("local");

    // No `with_transport`, and nothing constructs one: the bundled ureq
    // default is lazy, so a shell that only opens local books never pays
    // for a transport it does not use.
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/epub/minimal.epub")
        .to_string_lossy()
        .into_owned();
    let session = Session::open_with(
        path,
        SessionConfig::new(fixture_fonts()).with_library_dir(&dir),
    )
    .unwrap();
    assert!(session.spine_len() > 0);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Only meaningful in the device shape: `opds` on, `ureq` off. A shell
/// that drops the bundled transport and then forgets to supply one gets a
/// sentence saying exactly that, rather than a compile error it cannot act
/// on or a panic at the first fetch.
#[test]
#[cfg(not(feature = "ureq"))]
fn a_build_with_no_bundled_transport_says_so() {
    let dir = isolated_dir("no-transport");

    let config = SessionConfig::new(fixture_fonts()).with_library_dir(&dir);
    let Err(err) = Session::open_with(format!("{HOST}/opds/"), config) else {
        panic!("there is no transport in this build to have opened that with");
    };

    let message = err.to_string();
    assert!(message.contains("with_transport"), "{message}");
    let _ = std::fs::remove_dir_all(&dir);
}
