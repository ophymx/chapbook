//! OPDS Progression 1.0: fetch and update the last-known reading position
//! for one publication, behind the non-default `progression` feature.
//!
//! **Why this is off by default.** Progression 1.0 is an unreleased draft.
//! It lives in `opds-community/drafts` and is served from
//! `drafts.opds.io`; `specs.opds.io` lists only OPDS 2.0, 1.2, 1.1, 1.0 and
//! 0.9 as released, and Progression appears there not at all. The wire
//! format can still change under us, so the types below are not part of
//! what this crate promises to hold still — turning the feature on is a
//! caller saying it accepts that. Nothing here is reachable, and no draft
//! vocabulary appears in [`OpdsError`], unless the feature is on.
//!
//! The feature adds no dependencies. It is a surface gate, not a size one.
//!
//! # The shape of the protocol
//!
//! A progression service is **per publication**: the URL *is* the
//! publication's identity, discovered from a catalog link with
//! [`REL_PROGRESSION`] and media type [`MEDIA_TYPE_PROGRESSION`]. There is
//! no publication identifier in the document itself, so a caller that wants
//! to sync a book must persist the service URL it came from — a book that
//! arrived some other way (adopted bytes, a sideloaded file) has no service
//! to talk to until something maps it back to a catalog entry.
//!
//! `GET` reads the last-known point, `PUT` offers a new one and the server
//! decides. Both may answer 200 with an **empty body**, meaning "no
//! progression recorded yet" — which is why [`OpdsClient::fetch_progression`]
//! returns `Option` rather than treating empty as a parse failure.
//!
//! # Decisions worth not re-litigating
//!
//! - **`modified` stays a `String`.** The crate has no date type and parses
//!   dates loosely everywhere else (`Entry::published` is the same shape);
//!   adding one for a draft would be the tail wagging the dog.
//! - **Refusals are values, not errors.** 409 is a routine outcome of two
//!   devices reading the same book, not a failure, so
//!   [`OpdsClient::put_progression`] returns
//!   [`ProgressionUpdate::Refused`] and reserves `Err` for a request that
//!   did not complete. The four documented failure codes map to
//!   [`RefusalReason`]; anything else non-2xx is a real error.
//! - **The two 403s are only separable by Problem Details.** The spec gives
//!   `progression-incorrect-user` and `progression-locked` the same status,
//!   so a server that omits the RFC 7807 body gets
//!   [`RefusalReason::Unknown`]. That is the honest answer, not a gap to
//!   paper over with a guess.
//! - **The `authenticate` link hint is not modelled.** OPDS 2.0 lets a
//!   progression link carry `properties.authenticate` to save a 401
//!   round-trip. Reading it would mean adding a field to the ungated
//!   [`Link`], putting draft shape into the stable model to save one
//!   request; the 401 flow already works. Revisit if the draft is released.

use crate::http::{header, settle, HeaderValue, Method};
use crate::model::{AuthDocument, Entry, Feed, Link};
use crate::{OpdsClient, OpdsError};

/// The link relation a progression service is discovered by.
pub const REL_PROGRESSION: &str = "http://opds-spec.org/progression";

/// The media type of a Progression Document, sent as both `Accept` and
/// `Content-Type`.
pub const MEDIA_TYPE_PROGRESSION: &str = "application/opds-progression+json";

// The Problem Details `type` values the draft defines. Classification
// prefers these over the status code, because status alone cannot tell the
// two 403s apart.
const ERR_INVALID_PAYLOAD: &str = "https://registry.opds.io/error#progression-invalid-payload";
const ERR_INCORRECT_USER: &str = "https://registry.opds.io/error#progression-incorrect-user";
const ERR_LOCKED: &str = "https://registry.opds.io/error#progression-locked";
const ERR_DATE: &str = "https://registry.opds.io/error#progression-date";

/// A Progression Document: where a reader last was in one publication.
///
/// `progression` is the whole-publication fraction and the only thing every
/// client can act on; `references` refine it and are advisory. Per the
/// draft: prefer the most specific reference that resolves, and fall back
/// to `progression` when none do. The array carries no ordering guarantee,
/// so do not read position 0 as "best".
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Progression {
    /// Human context for the point — a chapter title. Display only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// ISO 8601 timestamp of the point. Compared, never parsed here.
    pub modified: String,
    /// Which device reported it, so a UI can say where the reader left off.
    pub device: Device,
    /// Whole-publication progress, 0.0 to 1.0 inclusive.
    pub progression: f64,
    /// URI references into the publication: a fragment (`#page=87`,
    /// `#t=849.250`), a path with one (`chapter1.html#par26`), a full URL,
    /// or a text fragment (`chapter1.html#:~:text=It%20was%20expected`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<String>,
}

/// The device that reported a progression.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Device {
    /// A stable URI naming this installation — a `urn:uuid:` or the
    /// reader's own URL. Not shown to the reader.
    ///
    /// This is host-owned identity, like a credential: the host mints it
    /// once and keeps it. This crate neither generates nor persists one.
    pub id: String,
    /// Display name, shown to the reader as "last read on …".
    pub name: String,
}

/// What the service did with a progression we offered it.
#[derive(Debug, Clone, PartialEq)]
pub enum ProgressionUpdate {
    /// 200 — stored, replacing what the service held. The body is the
    /// service's resulting document, which is authoritative over what we
    /// sent; empty when the service returns none.
    Stored(Option<Progression>),
    /// 201 — stored, and the first progression this service held for the
    /// publication.
    Created(Option<Progression>),
    /// The service declined. What it already held is unchanged.
    Refused(ProgressionRefusal),
}

/// A declined update, as the service explained it.
#[derive(Debug, Clone, PartialEq)]
pub struct ProgressionRefusal {
    /// The HTTP status, kept because [`RefusalReason::Unknown`] alone
    /// cannot be reported usefully.
    pub status: u16,
    pub reason: RefusalReason,
    /// The Problem Details `type` URI, when the service sent one.
    pub type_uri: Option<String>,
    /// The Problem Details `title` — human-readable, safe to surface.
    pub title: Option<String>,
}

/// Why a service refused an update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalReason {
    /// 409 — the service holds a more recent point. Fetch before retrying;
    /// re-sending the same point will be refused again.
    Stale,
    /// 403 — the progression belongs to a different user. A credential
    /// problem, not a position problem.
    IncorrectUser,
    /// 403 — this publication no longer accepts updates (a returned loan,
    /// an expired licence). Stop offering; retrying will not help.
    Locked,
    /// 400 — the service rejected the document itself.
    InvalidPayload,
    /// A documented failure status with no Problem Details to disambiguate
    /// it. For 403 this is the common case in the wild, and it means
    /// exactly "declined, cause unstated".
    Unknown,
}

impl Link {
    /// Whether this link points at a progression service.
    ///
    /// The relation alone decides. The media type is advisory here as
    /// everywhere else in OPDS, and a link that carries the rel without the
    /// type is still the service.
    pub fn is_progression(&self) -> bool {
        self.has_rel(REL_PROGRESSION)
    }
}

impl Entry {
    /// The progression service for this publication, when the catalog
    /// advertises one.
    pub fn progression(&self) -> Option<&Link> {
        self.links.iter().find(|l| l.is_progression())
    }
}

impl Feed {
    /// The progression service advertised at the document level — where a
    /// standalone OPDS 2.0 publication document puts it, rather than on an
    /// entry.
    pub fn progression(&self) -> Option<&Link> {
        self.links.iter().find(|l| l.is_progression())
    }
}

impl OpdsClient {
    /// Read the last-known progression for one publication.
    ///
    /// `Ok(None)` is a successful answer: the service has nothing recorded
    /// for this publication yet, which the draft encodes as 200 with an
    /// empty body. A 401 raises [`OpdsError::AuthRequired`] carrying the
    /// Authentication Document, exactly as catalog browsing does.
    pub fn fetch_progression(&self, url: &str) -> Result<Option<Progression>, OpdsError> {
        let (body, _) = self.get(url, MEDIA_TYPE_PROGRESSION)?;
        parse_optional(&body)
    }

    /// Offer a progression to the service, which decides whether to take it.
    ///
    /// `Err` means the exchange did not complete — transport failure, an
    /// undefined status, an unparseable body, or a 401 needing credentials.
    /// A service that answered and declined is
    /// [`ProgressionUpdate::Refused`], because two devices reading the same
    /// book race routinely and that is not an error.
    pub fn put_progression(
        &self,
        url: &str,
        progression: &Progression,
    ) -> Result<ProgressionUpdate, OpdsError> {
        // Checked before the request rather than after a 400: the range is
        // the one thing about the document we can be sure of, and a NaN
        // would serialize to `null` and fail deserialization on the far
        // side with a worse message. `contains` rejects NaN too.
        if !(0.0..=1.0).contains(&progression.progression) {
            return Err(OpdsError::Parse(format!(
                "progression must be between 0 and 1, got {}",
                progression.progression
            )));
        }
        let body = serde_json::to_vec(progression)
            .map_err(|e| OpdsError::Parse(format!("serialize progression: {e}")))?;
        let mut request = self.request(url, MEDIA_TYPE_PROGRESSION)?;
        *request.method_mut() = Method::PUT;
        request.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static(MEDIA_TYPE_PROGRESSION),
        );
        *request.body_mut() = body;

        let response = self
            .transport()
            .send(request)
            .map_err(|e| OpdsError::Network(e.to_string()))?;
        let settled =
            settle(response).map_err(|e| OpdsError::Network(format!("read body: {e}")))?;
        let (status, body) = (settled.status, &settled.body);

        if status == 401 {
            let auth_doc = settled
                .content_type()
                .filter(|t| t.contains("opds-authentication"))
                .and_then(|_| serde_json::from_slice::<AuthDocument>(body).ok());
            return Err(OpdsError::AuthRequired(auth_doc.map(Box::new)));
        }
        match status {
            201 => Ok(ProgressionUpdate::Created(parse_optional(body)?)),
            // Any other 2xx is read as "stored": the draft names 200 and
            // 201, and a service answering 204 has still accepted it.
            200..=299 => Ok(ProgressionUpdate::Stored(parse_optional(body)?)),
            400 | 403 | 409 => Ok(ProgressionUpdate::Refused(refusal(status, body))),
            // Everything else — 404 at a URL with no service, a 5xx — is a
            // failure rather than a decision about the position.
            _ => Err(OpdsError::Http(status)),
        }
    }
}

/// Parse a body that the draft allows to be empty.
///
/// Whitespace-only counts as empty: a service that answers 200 with a
/// newline means the same thing as one that sends zero bytes, and failing
/// there would turn "nothing recorded yet" into an error.
fn parse_optional(body: &[u8]) -> Result<Option<Progression>, OpdsError> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    serde_json::from_slice(body)
        .map(Some)
        .map_err(|e| OpdsError::Parse(format!("progression document: {e}")))
}

/// Classify a declined update, preferring the Problem Details `type` over
/// the status — 400/403/409 cannot separate the two 403s on their own.
fn refusal(status: u16, body: &[u8]) -> ProgressionRefusal {
    #[derive(serde::Deserialize)]
    struct ProblemDetails {
        #[serde(rename = "type")]
        type_uri: Option<String>,
        title: Option<String>,
    }

    let problem = serde_json::from_slice::<ProblemDetails>(body).ok();
    let type_uri = problem.as_ref().and_then(|p| p.type_uri.clone());
    let title = problem.as_ref().and_then(|p| p.title.clone());
    let reason = match type_uri.as_deref() {
        Some(ERR_DATE) => RefusalReason::Stale,
        Some(ERR_INCORRECT_USER) => RefusalReason::IncorrectUser,
        Some(ERR_LOCKED) => RefusalReason::Locked,
        Some(ERR_INVALID_PAYLOAD) => RefusalReason::InvalidPayload,
        // No Problem Details, or a `type` this draft does not define: fall
        // back to what the status alone can prove. 403 proves nothing.
        _ => match status {
            409 => RefusalReason::Stale,
            400 => RefusalReason::InvalidPayload,
            _ => RefusalReason::Unknown,
        },
    };
    ProgressionRefusal {
        status,
        reason,
        type_uri,
        title,
    }
}
