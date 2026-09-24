//! Browsing a catalog: the held feed, the crumb trail, the login.
//!
//! `opds-client` fetches and parses; this is what an app *does* with a
//! feed. It was the largest thing written twice: a navigation row is a
//! fetch that pushes a crumb, Back walks the crumbs before it leaves the
//! screen, a facet replaces the feed and a page appends to it, a 401 is a
//! login drawn from the authentication document rather than a failure,
//! and signing in stores the credential by origin and fetches again. All
//! of that is here now, over the same held-feed accessors the C ABI and
//! the JNI binding already read.
//!
//! **Every call that fetches blocks.** A [`Catalog`] belongs on whatever
//! thread a front end keeps its blocking catalog work on, one at a time;
//! it spawns nothing.

use std::path::Path;
use std::sync::Arc;

use chapbook_core::{
    Credential, CredentialKey, CredentialLookup, CredentialStore, Freshness, Result,
};
use chapbook_library::{BookId, Library, OpdsSource};
use chapbook_opds::{resolve_url, AuthDocument, Entry, Feed, OpdsClient, OpdsError};

/// A catalog the reader added: where it is and what it called itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedCatalog {
    pub id: i64,
    /// Blank until the reader names it or its feed does.
    pub title: String,
    /// Opaque and possibly secret-bearing: never log it, and never key a
    /// credential by it.
    pub url: String,
}

impl From<OpdsSource> for SavedCatalog {
    fn from(source: OpdsSource) -> SavedCatalog {
        SavedCatalog {
            id: source.id,
            title: source.title.unwrap_or_default(),
            url: source.url,
        }
    }
}

/// What the catalog screen shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowseState {
    /// Nothing fetched yet.
    Opening,
    /// A feed is held; read it through the accessors.
    Feed,
    /// A 401 with a login to draw. `retry` is the URL that was refused,
    /// which [`Catalog::sign_in`] fetches again.
    Login {
        title: String,
        offers_basic: bool,
        retry: String,
    },
    /// The fetch of `url` failed and there is nothing to show. `reason`
    /// is for a log; the front end has its own sentence.
    Failed { url: String, reason: String },
}

/// One way to narrow the held feed, as the catalog offers it. Facets in
/// a group are alternatives; a front end draws one control per group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facet {
    pub group_index: usize,
    pub group: String,
    pub label: String,
    /// Absolute. Hand it to [`Catalog::apply_facet`] or [`Catalog::go`].
    pub href: String,
    pub active: bool,
    pub count: Option<u64>,
}

/// Everything a platform's transfer needs to fetch one entry itself, and
/// everything the landing needs afterwards — captured when the row was
/// read, so it travels with the row and stays right after the feed has
/// paged on.
///
/// No credential travels in here, deliberately: the app opened this
/// catalog, so it knows which one the origin takes, and adding it when
/// the transfer starts keeps the secret out of whatever the job system
/// persists and makes a token rotated in between simply fresh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Download {
    /// Absolute. The one field that is not advice.
    pub url: String,
    /// Send as given — `Accept: */*` today — plus the app's own.
    pub headers: Vec<(String, String)>,
    /// One safe path component, for a notification or a downloads folder.
    pub suggested_filename: String,
    pub media_type: Option<String>,
    /// So a notification can name the book after the feed is gone.
    pub title: String,
    /// Opaque; the key for one job per entry.
    pub entry_id: String,
    /// The two sync services, absolute, for the landing. They live in the
    /// entry and nowhere else.
    pub progression_url: Option<String>,
    pub annotation_container: Option<String>,
}

/// One catalog being browsed: the client, the feed it holds, the crumbs
/// that led there, and the login it was last refused with.
pub struct Catalog {
    client: OpdsClient,
    credentials: Arc<dyn CredentialStore>,
    /// The saved row's title, shown until a feed announces its own.
    fallback_title: String,
    /// The URL the held feed came from — what relative hrefs resolve
    /// against, and the last crumb.
    base: String,
    feed: Option<Feed>,
    /// The authentication document from the last refusal, held so a
    /// login can be drawn from it.
    auth: Option<AuthDocument>,
    crumbs: Vec<String>,
    state: BrowseState,
}

impl Catalog {
    pub(crate) fn new(
        client: OpdsClient,
        credentials: Arc<dyn CredentialStore>,
        fallback_title: String,
    ) -> Catalog {
        Catalog {
            client,
            credentials,
            fallback_title,
            base: String::new(),
            feed: None,
            auth: None,
            crumbs: Vec::new(),
            state: BrowseState::Opening,
        }
    }

    // ---- The held feed ----

    /// The URL the held feed came from.
    pub fn base(&self) -> &str {
        &self.base
    }

    pub fn feed(&self) -> Option<&Feed> {
        self.feed.as_ref()
    }

    /// The authentication document from the last refusal, if any.
    pub fn auth(&self) -> Option<&AuthDocument> {
        self.auth.as_ref()
    }

    /// The client, for a front end that manages a credential itself
    /// rather than through the store — `set_authorization` is the door.
    pub fn client(&mut self) -> &mut OpdsClient {
        &mut self.client
    }

    pub fn state(&self) -> &BrowseState {
        &self.state
    }

    /// What a browse screen puts at the top: the held feed's title, or
    /// the saved catalog's until there is one.
    pub fn title(&self) -> String {
        match &self.feed {
            Some(feed) if !feed.title.trim().is_empty() => feed.title.clone(),
            _ => self.fallback_title.clone(),
        }
    }

    /// Every row the screen shows: the held feed's entries, which
    /// [`load_more`](Catalog::load_more) appends to.
    pub fn entries(&self) -> &[Entry] {
        self.feed
            .as_ref()
            .map(|f| f.entries.as_slice())
            .unwrap_or(&[])
    }

    /// The download an entry describes, or `None` for a row with nothing
    /// to fetch — a navigation row, a purchase-only entry.
    pub fn download(&self, index: usize) -> Option<Download> {
        let entry = self.entries().get(index)?;
        let request = entry.download_request()?;
        let (progression, container) = chapbook_sync::targets_of(entry);
        Some(Download {
            url: resolve_url(&self.base, &request.url),
            headers: request.headers,
            suggested_filename: request.suggested_filename,
            media_type: request.media_type,
            title: request.title,
            entry_id: request.entry_id,
            // Resolved again even though the parser already did: a no-op
            // on an absolute href, and what stops a root-relative service
            // path reaching the library as a path.
            progression_url: progression.map(|href| resolve_url(&self.base, &href)),
            annotation_container: container.map(|href| resolve_url(&self.base, &href)),
        })
    }

    /// The held feed's facets, flattened with their group.
    pub fn facets(&self) -> Vec<Facet> {
        let Some(feed) = &self.feed else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (group_index, (group, links)) in feed.facet_groups().into_iter().enumerate() {
            for link in links {
                out.push(Facet {
                    group_index,
                    group: group.clone(),
                    label: link.title.clone().unwrap_or_default(),
                    href: resolve_url(&self.base, &link.href),
                    active: link.active_facet,
                    count: link.count,
                });
            }
        }
        out
    }

    /// Whether this catalog offers a search — what decides if a search
    /// box is drawn at all.
    pub fn has_search(&self) -> bool {
        self.feed.as_ref().is_some_and(|f| f.search().is_some())
    }

    /// The next page's URL, or `None` at the end of a paged feed.
    pub fn next_page(&self) -> Option<String> {
        self.feed
            .as_ref()
            .and_then(|f| f.next())
            .map(|l| resolve_url(&self.base, &l.href))
    }

    pub fn previous_page(&self) -> Option<String> {
        self.feed
            .as_ref()
            .and_then(|f| f.previous())
            .map(|l| resolve_url(&self.base, &l.href))
    }

    /// The refused catalog's own name for itself, for a login sheet.
    pub fn auth_title(&self) -> Option<&str> {
        self.auth.as_ref().map(|doc| doc.title.as_str())
    }

    /// Whether the refused catalog offers username-and-password — the
    /// only flow a reader completes without a browser. False means it
    /// wants something else, and a front end says so rather than drawing
    /// a login that cannot work.
    pub fn auth_offers_basic(&self) -> bool {
        self.auth
            .as_ref()
            .is_some_and(|doc| doc.basic_flow().is_some())
    }

    // ---- Fetching ----

    /// Fetch what is at `url` and hold it, replacing whatever was held.
    /// The state follows: a feed, a login, or a failure. Blocking.
    ///
    /// The primitive under [`go`](Catalog::go); it moves no crumb, which
    /// is what a facet, a search result and a sign-in retry want.
    pub fn fetch(&mut self, url: &str) -> std::result::Result<(), OpdsError> {
        self.authorize(url);
        match self.client.fetch(url) {
            Ok(feed) => {
                self.base = url.to_string();
                self.feed = Some(feed);
                self.auth = None;
                self.state = BrowseState::Feed;
                Ok(())
            }
            Err(e) => Err(self.refused(url, e)),
        }
    }

    /// Open a feed the reader chose — the root, a navigation row, a facet
    /// — pushing a crumb Back can return to.
    pub fn go(&mut self, url: &str) -> std::result::Result<(), OpdsError> {
        self.crumbs.push(url.to_string());
        self.fetch(url)
    }

    /// Back, inside the catalog: fetch the previous crumb again. `false`
    /// at the root, which is the front end's cue to leave the screen.
    pub fn back(&mut self) -> bool {
        if self.crumbs.len() <= 1 {
            return false;
        }
        self.crumbs.pop();
        let previous = self.crumbs.last().cloned().expect("a crumb remains");
        let _ = self.fetch(&previous);
        true
    }

    /// Narrow by a facet of the held feed, by index into
    /// [`facets`](Catalog::facets).
    pub fn apply_facet(&mut self, index: usize) -> std::result::Result<(), OpdsError> {
        let Some(facet) = self.facets().into_iter().nth(index) else {
            return Err(OpdsError::Parse(format!("no facet at {index}")));
        };
        self.go(&facet.href)
    }

    /// Search the held catalog; the results replace it, so browsing and
    /// searching are one screen. `Err(Parse)` when the catalog offers no
    /// search, which [`has_search`](Catalog::has_search) predicts.
    pub fn search(&mut self, query: &str) -> std::result::Result<(), OpdsError> {
        if self.feed.is_none() {
            return Err(OpdsError::Parse("nothing has been fetched yet".into()));
        }
        if !self.has_search() {
            return Err(OpdsError::Parse("this catalog offers no search".into()));
        }
        let base = self.base.clone();
        self.authorize(&base);
        let feed = self.feed.as_ref().expect("checked above");
        match self.client.search(feed, &base, query) {
            Ok(results) => {
                self.feed = Some(results);
                self.auth = None;
                self.state = BrowseState::Feed;
                Ok(())
            }
            Err(e) => Err(self.refused(&base, e)),
        }
    }

    /// Fetch the next page and append its rows to the held feed, for the
    /// infinite scroll a phone browses with. `Ok(false)` when there is no
    /// next page. On failure the held feed is untouched and the state
    /// stays [`BrowseState::Feed`]: a page that did not arrive is a row
    /// that did not appear, not a screen that failed.
    pub fn load_more(&mut self) -> std::result::Result<bool, OpdsError> {
        let Some(next) = self.next_page() else {
            return Ok(false);
        };
        self.authorize(&next);
        let page = self.client.fetch(&next)?;
        let held = self.feed.as_mut().expect("a next page implies a held feed");
        held.entries.extend(page.entries);
        // The pagination links move on; everything else — facets, the
        // search link — is the first page's, since a later page carries
        // the same or nothing.
        let paging = |link: &chapbook_opds::Link| {
            link.has_rel("next") || link.has_rel("previous") || link.has_rel("prev")
        };
        held.links.retain(|link| !paging(link));
        held.links.extend(page.links.into_iter().filter(paging));
        Ok(true)
    }

    /// Sign in with Basic to the catalog that refused: store the
    /// credential by the refused URL's origin — never by the URL, whose
    /// path may be a secret — and fetch it again without moving a crumb.
    /// A store that cannot hold it (read-only, locked) still signs this
    /// session in; the reader is asked again next launch.
    pub fn sign_in(
        &mut self,
        username: &str,
        password: &str,
    ) -> std::result::Result<(), OpdsError> {
        let BrowseState::Login { retry, .. } = &self.state else {
            return Err(OpdsError::Parse("nothing has asked for a login".into()));
        };
        let retry = retry.clone();
        let credential = Credential::basic(username, password);
        if let Some(key) = CredentialKey::http_origin(&retry) {
            if let Err(e) = self.credentials.store(&key, &credential) {
                log::warn!("the credential store would not keep the sign-in: {e}");
            }
        }
        self.client.set_authorization(credential.authorization);
        self.fetch(&retry)
    }

    /// The whole job on one thread: fetch the entry's acquisition, import
    /// it into the library at `library_dir`, record the sync services it
    /// advertises. Right for a tap the reader is watching on a desk;
    /// wrong for a transfer that has to outlive a phone's screen, which
    /// takes [`download`](Catalog::download) apart and lands with
    /// [`App::land_download`](crate::App::land_download). The staging
    /// file is removed whatever happens.
    pub fn download_to_library(&mut self, index: usize, library_dir: &Path) -> Result<BookId> {
        let request = self.download(index).ok_or_else(|| {
            chapbook_core::ChapbookError::Opds("this entry has nothing to download".into())
        })?;
        self.authorize(&request.url);
        let staging = std::env::temp_dir().join(format!("chapbook-acquire-{}", std::process::id()));
        std::fs::create_dir_all(&staging)?;
        let file = staging.join(format!("entry-{index}"));
        if let Err(e) = self.client.download(&request.url, &file) {
            let _ = std::fs::remove_file(&file);
            return Err(chapbook_opds::to_chapbook_error(e));
        }
        let imported = (|| {
            let publication = chapbook_reader::open_publication(&file)?;
            let mut library = Library::open(library_dir)?;
            let id = library.import(&file, publication.as_ref())?;
            library.set_sync_targets(
                id,
                request.progression_url.as_deref(),
                request.annotation_container.as_deref(),
            )?;
            Ok(id)
        })();
        let _ = std::fs::remove_file(&file);
        imported
    }

    /// Give the client the store's credential for `url`'s origin, when
    /// there is one. A missing credential leaves whatever the front end
    /// set through [`client`](Catalog::client) in place, so a host that
    /// manages its own is not undone by an empty store.
    fn authorize(&mut self, url: &str) {
        let Some(key) = CredentialKey::http_origin(url) else {
            return;
        };
        if let CredentialLookup::Found(credential) = self.credentials.get(&key, Freshness::Cached) {
            self.client.set_authorization(credential.authorization);
        }
    }

    /// Turn a failure into the state the screen shows, keeping the
    /// authentication document where a login can be drawn from it, and
    /// hand the error back for a caller that maps it to a code.
    fn refused(&mut self, url: &str, error: OpdsError) -> OpdsError {
        match error {
            OpdsError::AuthRequired(document) => {
                self.auth = document.map(|boxed| *boxed);
                self.state = BrowseState::Login {
                    title: self
                        .auth_title()
                        .map(str::to_string)
                        .unwrap_or_else(|| self.fallback_title.clone()),
                    offers_basic: self.auth_offers_basic(),
                    retry: url.to_string(),
                };
                OpdsError::AuthRequired(self.auth.clone().map(Box::new))
            }
            other => {
                self.state = BrowseState::Failed {
                    url: url.to_string(),
                    reason: other.to_string(),
                };
                other
            }
        }
    }
}
