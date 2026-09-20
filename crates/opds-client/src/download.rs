//! Handing a download to the platform instead of performing it.
//!
//! [`OpdsClient::download`](crate::OpdsClient::download) fetches an
//! acquisition itself, through the injected transport, and returns when the
//! file is on disk. That is the right shape for a desktop process and the
//! wrong shape for a phone: a transfer that has to survive the app being
//! suspended is a *job*, not a call. It has an identity, it reports
//! progress, and it finishes by waking the app rather than by returning —
//! none of which a blocking function can express.
//!
//! The transport does not rescue it, which is why there is no hook there
//! to reach for. [`HttpClient`](crate::HttpClient) fetches bytes and
//! nothing else: every method on it blocks until its request settles,
//! so an implementation that "owned downloading" would hold a thread for
//! the whole transfer — precisely what a transfer outliving its process
//! does not do.
//!
//! So the other door hands the whole transfer over. [`DownloadRequest`]
//! describes what to fetch; the host fetches it with `WorkManager`,
//! a background `URLSession`, a browser's own download manager, `curl`;
//! and the finished file comes back to the library afterwards. Nothing in
//! this crate is involved in between, which is the point — a job system
//! cannot call back into a client that no longer exists.
//!
//! # The descriptor is advice, not instruction
//!
//! Every field here is a suggestion the host may override, and a host that
//! rewrites all of them is behaving correctly. It will add its own
//! `Authorization` and `User-Agent`, it may route through a proxy, and it
//! will very often rename the file — `URLSession` lands bytes in a temp
//! file of its own choosing, and `DownloadManager` answers with a
//! `content://` URI that has no name at all.
//!
//! What the crate therefore must never assume, and does not:
//!
//! - **the path**, so completion is told where the file actually is;
//! - **the name or extension**, which is safe because format is decided by
//!   sniffing bytes;
//! - **the headers**, since the crate never sees the request that was sent;
//! - **the response** — no status, no `Content-Type`, no `ETag` comes back.
//!   The host decided the transfer succeeded; the only later check is
//!   whether the bytes open as a publication.
//!
//! # Credentials do not travel
//!
//! [`headers`](DownloadRequest::headers) never carries an `Authorization`,
//! and there is no field naming which credential to use either. The host
//! opened this catalog, so it already knows which credential the catalog
//! takes; repeating it here would add a field and no information.
//!
//! Two things follow. A secret never reaches the places a job descriptor
//! gets persisted — `WorkManager`'s input `Data`, a `URLSessionTask`'s
//! description — both of which are on-disk and enumerable. And a token
//! rotated between enqueuing the job and running it is simply fresh, since
//! the host reads its own store when the transfer starts rather than
//! carrying a copy taken minutes earlier.
//!
//! # Getting the book back onto the shelf
//!
//! One thing genuinely cannot be recovered from the downloaded file: the
//! sync services the catalog entry advertised. They live in the entry and
//! nowhere else, and by the time a background transfer lands, the feed is
//! usually gone. A host that means to sync should read
//! them off the entry when it builds the request and persist them beside
//! the job. That is the only state the completion step needs.

use crate::model::{Entry, MediaType};

/// What the host needs in order to fetch one acquisition itself.
///
/// Build it with [`Entry::download_request`]. Note what that signature does
/// *not* take: no client, no transport, no base URL. Producing this is pure
/// inspection of an entry already in hand, so a host can enqueue a download
/// long after the catalog session that produced the feed has been dropped.
///
/// Every field is advisory except [`url`](Self::url) — see the module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadRequest {
    /// The acquisition to fetch, absolute: hrefs are resolved against the
    /// request URL at parse time.
    ///
    /// The one field that is not really advice. A host may route it through
    /// a proxy or follow it across hosts, but substituting a different
    /// resource means downloading a different book.
    pub url: String,

    /// Headers to send, in order — `Accept: */*` today.
    ///
    /// Send them as given, the way [`HttpRequest`](crate::HttpRequest)
    /// asks: catalog servers negotiate by naive substring match, so
    /// rewriting or merging `Accept` breaks servers in the wild. Adding
    /// headers of the host's own — credentials above all — is expected.
    pub headers: Vec<(String, String)>,

    /// A filename for the user's benefit, derived from the title and safe
    /// to use as one path component: no separators, no control characters,
    /// nothing a shell or a filesystem reads as structure.
    ///
    /// Advisory in the strongest sense. It is a courtesy for a downloads
    /// folder or a notification, it is not unique, and it carries no
    /// meaning for the engine — format is decided by sniffing bytes, so the
    /// extension is decoration. Hosts should still validate it against
    /// their own filesystem's rules before using it.
    pub suggested_filename: String,

    /// The acquisition link's advertised type, verbatim, when it had one.
    ///
    /// A hint for the host's own UI. Do not trust it to decide how to read
    /// the file: catalog servers mislabel acquisitions often enough that
    /// the interop doc has a section about it.
    pub media_type: Option<String>,

    /// The entry's title, so a progress notification can name the book
    /// after the feed it came from is gone.
    pub title: String,

    /// The entry's OPDS id, opaque — comic-server ids contain slashes and
    /// dots. A stable key for the host's own record of the job; never
    /// normalize it, and never build a path out of it.
    pub entry_id: String,
}

impl Entry {
    /// Describe this entry's acquisition for a host that will fetch it
    /// itself, or `None` when there is nothing to fetch.
    ///
    /// The acquisition chosen is the first one, matching
    /// [`acquisitions`](Entry::acquisitions) and the `can_download` flag
    /// the bindings expose, so the two doors never disagree about which
    /// link a row means.
    ///
    /// **A borrow or buy link is not a book.** An acquisition carrying an
    /// [`indirect`](crate::Link::indirect) chain points at an intermediate
    /// step — a loan endpoint, a fulfilment document — and fetching it
    /// yields that document rather than a publication. This returns it
    /// anyway, because refusing here would disagree with `can_download`,
    /// and because servers in the wild do put `indirectAcquisition` on
    /// links that really are the file. A host offering "Borrow" separately
    /// from "Get" should consult `indirect` itself.
    pub fn download_request(&self) -> Option<DownloadRequest> {
        let link = self.acquisitions().next()?;
        Some(DownloadRequest {
            url: link.href.clone(),
            headers: vec![("Accept".to_string(), "*/*".to_string())],
            suggested_filename: suggested_filename(&self.title, link.media_type.as_ref()),
            media_type: link.media_type.as_ref().map(|t| t.raw.clone()),
            title: self.title.clone(),
            entry_id: self.id.clone(),
        })
    }
}

/// Bytes, not characters: filesystems count bytes, and a title in a script
/// that costs three bytes a glyph must not blow past the limit. Well under
/// the usual 255 so a host has room to disambiguate.
const MAX_STEM: usize = 100;

/// What a stem collapses to when the title contributes nothing usable — an
/// empty title, or one made entirely of characters a path cannot hold.
const FALLBACK_STEM: &str = "book";

/// A title turned into one safe path component, with an extension guessed
/// from the media type.
///
/// Safety here means structural only: the result is a single component that
/// no filesystem or shell reads as anything but a name. It is deliberately
/// not sanitized down to ASCII — a Japanese title should still arrive
/// readable — so a host with stricter rules than "no separators, no control
/// characters" has to apply them itself.
fn suggested_filename(title: &str, media_type: Option<&MediaType>) -> String {
    let mut stem = String::new();
    let mut pending_separator = false;
    for ch in title.chars() {
        // `:` and friends are here for Windows and for the `content://`
        // URIs Android hands back; `/` and `\` and control characters are
        // what would make this more than one path component.
        let safe =
            !matches!(ch, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') && !ch.is_control();
        if !safe || ch.is_whitespace() {
            // One run of unusable characters becomes one separator, and a
            // run at either end becomes none: `_` is written lazily.
            pending_separator = !stem.is_empty();
            continue;
        }
        if stem.len() + ch.len_utf8() > MAX_STEM {
            break;
        }
        if pending_separator {
            stem.push('_');
            pending_separator = false;
        }
        stem.push(ch);
    }

    // A leading dot hides the file on Unix; a trailing dot is silently
    // dropped by Windows, which turns `Vol. 1.` and `Vol. 1` into the same
    // name. Separators go with them, since a stem that opens or closes on
    // one is just the shape a stripped-out path left behind.
    let stem = stem.trim_matches(|ch| ch == '.' || ch == '_');
    let stem = if stem.is_empty() { FALLBACK_STEM } else { stem };

    match media_type.and_then(|t| extension_for(&t.essence)) {
        Some(extension) => format!("{stem}.{extension}"),
        None => stem.to_string(),
    }
}

/// The file extension conventionally used for an acquisition media type.
///
/// Cosmetic: a book is its bytes, not its name, and nothing in the engine
/// dispatches on this. It exists so a downloads folder is legible. An
/// unrecognized type gets no extension rather than a guessed one, since a
/// wrong extension is worse than none — it is what tempts a host into
/// trusting the name.
fn extension_for(essence: &str) -> Option<&'static str> {
    Some(match essence {
        "application/epub+zip" => "epub",
        "application/pdf" => "pdf",
        "application/vnd.comicbook+zip" | "application/x-cbz" => "cbz",
        "application/vnd.comicbook-rar" | "application/x-cbr" => "cbr",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Link;

    fn typed(essence: &str) -> Option<MediaType> {
        Some(MediaType::parse(essence))
    }

    fn entry_with(link: Link) -> Entry {
        Entry {
            id: "urn:uuid:1".to_string(),
            title: "Dune".to_string(),
            links: vec![link],
            ..entry()
        }
    }

    fn entry() -> Entry {
        Entry {
            id: String::new(),
            title: String::new(),
            authors: Vec::new(),
            language: None,
            published: None,
            publisher: None,
            identifier: None,
            summary: None,
            content_html: None,
            series: None,
            links: Vec::new(),
        }
    }

    fn acquisition(href: &str, essence: Option<&str>) -> Link {
        Link {
            href: href.to_string(),
            rel: vec![crate::REL_ACQ_PREFIX.to_string()],
            media_type: essence.and_then(typed),
            ..Link::default()
        }
    }

    #[test]
    fn an_entry_without_an_acquisition_has_nothing_to_download() {
        let mut entry = entry();
        entry.links.push(Link {
            href: "https://example.com/cover.png".to_string(),
            rel: vec![crate::REL_IMAGE.to_string()],
            ..Link::default()
        });
        assert!(entry.download_request().is_none());
    }

    #[test]
    fn the_request_describes_the_first_acquisition() {
        let entry = entry_with(acquisition(
            "https://example.com/dune.epub",
            Some("application/epub+zip"),
        ));
        let request = entry.download_request().expect("acquisition");
        assert_eq!(request.url, "https://example.com/dune.epub");
        assert_eq!(request.title, "Dune");
        assert_eq!(request.entry_id, "urn:uuid:1");
        assert_eq!(request.suggested_filename, "Dune.epub");
        assert_eq!(request.media_type.as_deref(), Some("application/epub+zip"));
        assert_eq!(request.headers, [("Accept".to_string(), "*/*".to_string())]);
    }

    /// The invariant the module doc rests on: a credential never rides in
    /// the descriptor, so it never reaches the host's on-disk job record.
    #[test]
    fn the_request_never_carries_a_credential() {
        let entry = entry_with(acquisition("https://example.com/dune.epub", None));
        let request = entry.download_request().expect("acquisition");
        assert!(!request
            .headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("authorization")));
    }

    #[test]
    fn a_title_never_becomes_more_than_one_path_component() {
        assert_eq!(suggested_filename("../../etc/passwd", None), "etc_passwd");
        assert_eq!(
            suggested_filename("C:\\Windows\\System32", None),
            "C_Windows_System32"
        );
        assert_eq!(suggested_filename("tab\there", None), "tab_here");
    }

    #[test]
    fn runs_of_unusable_characters_collapse_and_never_edge_the_name() {
        assert_eq!(
            suggested_filename("  Dune   Messiah  ", None),
            "Dune_Messiah"
        );
        assert_eq!(
            suggested_filename("Dune: ??? Messiah", None),
            "Dune_Messiah"
        );
    }

    /// A leading dot hides the file; Windows drops a trailing one, which
    /// would silently merge two different books' names.
    #[test]
    fn a_stem_never_starts_or_ends_with_a_dot_or_separator() {
        assert_eq!(suggested_filename(".hidden", None), "hidden");
        assert_eq!(suggested_filename("Vol. 1.", None), "Vol._1");
        assert_eq!(suggested_filename("[2003] Dune", None), "[2003]_Dune");
    }

    #[test]
    fn a_title_that_contributes_nothing_falls_back() {
        assert_eq!(
            suggested_filename("", Some(&MediaType::parse("application/pdf"))),
            "book.pdf"
        );
        assert_eq!(suggested_filename("///", None), "book");
        assert_eq!(suggested_filename("...", None), "book");
    }

    /// Truncation counts bytes, because filesystems do — and it must never
    /// split a character, which would make the name invalid UTF-8's worth
    /// of trouble for a host marshalling it back out.
    #[test]
    fn a_long_title_is_cut_on_a_character_boundary() {
        let name = suggested_filename(&"さ".repeat(200), None);
        assert!(name.len() <= MAX_STEM, "{} bytes", name.len());
        assert_eq!(name.chars().count(), MAX_STEM / "さ".len());
    }

    #[test]
    fn the_extension_follows_the_media_type_or_is_absent() {
        let cases = [
            ("application/epub+zip", "Dune.epub"),
            ("application/pdf", "Dune.pdf"),
            ("application/vnd.comicbook+zip", "Dune.cbz"),
            ("application/x-cbz", "Dune.cbz"),
            ("application/x-cbr", "Dune.cbr"),
            ("application/octet-stream", "Dune"),
        ];
        for (essence, expected) in cases {
            let media_type = MediaType::parse(essence);
            assert_eq!(suggested_filename("Dune", Some(&media_type)), expected);
        }
    }

    /// Essence comparison, not string equality — the media type arrives
    /// with parameters and arbitrary case from real servers.
    #[test]
    fn the_extension_survives_parameters_and_case() {
        let media_type = MediaType::parse("Application/EPUB+Zip; charset=utf-8");
        assert_eq!(suggested_filename("Dune", Some(&media_type)), "Dune.epub");
    }
}
