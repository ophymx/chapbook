# OPDS client interop requirements

What this client must handle to work against real catalogs. Every behavior
in §§0–5 is observed in servers in the wild (self-hosted catalog servers,
comic servers, and library-lending stacks, surveyed Aug 2026), and each one
is asserted by the crate's tests: `tests/fixtures.rs` parses the wire-format
corpus (`fixtures/opds/` at the workspace root, one fixture per quirk),
`tests/injected_transport.rs` drives the flows over a scripted `HttpClient`,
and `tests/live_server.rs` is the opt-in smoke against a real catalog.

§6 is the exception and says so: it is written from an unreleased draft
rather than from a survey, which is why it is behind a feature and why its
tests (`tests/progression.rs`) quote the draft's examples verbatim.

## 0. Crate decision: atom_syndication cannot carry OPDS

**Confirmed, not speculation:** `atom_syndication`'s `Link` struct has only
the six fixed Atom fields. Foreign-namespace attributes on `<link>` —
`opds:facetGroup`, `opds:activeFacet`, `thr:count`, `pse:count`,
`pse:lastRead`, `pse:lastReadDate` — are **silently dropped on parse** and
cannot be produced on write. Those attributes are where facets and page
streaming live. `feed-rs` is worse (lossy normalized model).

So feeds are parsed at the XML level with `quick-xml` (namespace-aware) —
fully, not just for `<link>` elements; the Atom subset OPDS uses is small.
`atom_syndication` appears nowhere, and the facet-attribute assertions in
`tests/fixtures.rs` are the canary that keeps a lossy feed crate from
creeping back in.

## 1. Version strategy

Speak **OPDS 1.2 Atom as the canonical dialect**. OPDS 2.0 (JSON) is a
secondary parser kept honest by fixtures. Reasons:

- Page streaming (PSE) exists only in 1.x output on every known server.
- The facet `active` flag exists only in 1.x on reference servers.
- 1.2 is the universal floor: every OPDS server speaks it; several major ones
  speak nothing else.

Request one media type at a time: `Accept: application/atom+xml` (or
`application/opds+json` when explicitly probing 2.0). **Do not send compound
Accept headers with q-values** — real servers negotiate by naive substring
matching and ignore q entirely; `application/atom+xml,
application/opds+json;q=0.1` can return JSON. Some servers also honor
`?version=1.2|2.0` / `?f=atom|json` query overrides.

The two encodings of the same catalog are **not informationally equivalent**
(2.0 gains series/`belongsTo`, image dimensions; 1.2 gains PSE, facet-active,
distinct summary-vs-HTML-content). Never assume a field survives a version
switch.

## 2. Parsing hard requirements

- **Resolve every href against the request URL.** Self links, pagination,
  search templates, acquisition links, image links — all may be relative,
  including on servers that "should" emit absolute URIs in 2.0.
- **Never strict-URI-parse link hrefs.** PSE templates contain literal
  `{pageNumber}`/`{maxWidth}` braces.
- **Never schema-validate received feeds as an acceptance gate.** PSE stream
  links and lending extensions are schema-invalid *by design* on reference
  servers. Schemas (in `fixtures/opds/schema/`) are for testing our own 2.0
  parser, nothing else.
- Media types come with no space after `;`:
  `application/atom+xml;profile=opds-catalog;kind=acquisition`. Compare
  parsed essence + parameters, never string equality.
- Pagination: the back-rel is **`previous`** (not `prev`); `first`/`last` are
  frequently absent. Totals: 1.2 uses OpenSearch elements
  (`totalResults`/`itemsPerPage`/`startIndex`); 2.0 uses
  `numberOfItems`/`itemsPerPage`/`currentPage`.
- Dates: `dcterms:issued` may be date-only (`YYYY-MM-DD`) while 2.0
  `published` is full RFC 3339 — parse both shapes everywhere a date appears.
  Feed `updated` may change on every request (servers default it to now):
  worthless as a cache key.
- 2.0 polymorphism: contributor and subject are string-or-object; `rel` is
  string-or-array; `description` may contain raw HTML.
- 2.0 images carry **no rel** — treat the first image as cover.
- 1.2 group stand-in: entries may carry `rel="collection"` links where 2.0
  would use `groups[]`.
- Entry ids and hrefs are opaque strings: comic-server chapter ids contain
  slashes and dots; path-normalizing or splitting them breaks routing.
  `Content-Disposition` filenames on downloads can be garbage — sanitize.
- **Fill every OpenSearch template parameter, not just `{searchTerms}`.**
  Real description documents carry refinement parameters —
  `?q={searchTerms}&author={atom:author?}&title={atom:title?}` is what
  Calibre-Web, COPS and Kavita emit. Per OpenSearch 1.1 §4.2 an optional
  parameter (trailing `?`) the client does not supply is replaced with the
  **empty string**, and a required one it cannot supply means the template
  is unusable. Leaving `{atom:author?}` in the URL is not a cosmetic
  failure: the server filters on an author literally named
  `{atom:author?}` and answers **200 with an empty feed**, which is
  indistinguishable from a search that found nothing. Match parameters on
  their local name (`{os:searchTerms}` is `{searchTerms}`);
  `startIndex`/`startPage`/`language`/`inputEncoding`/`outputEncoding` have
  spec defaults, `count` does not.

## 3. Page streaming (OPDS-PSE)

- Namespace `http://vaemendis.net/opds-pse/ns`; stream link rel
  `http://vaemendis.net/opds-pse/stream`.
- `pse:count` is required by convention (render nothing without it).
  `{pageNumber}` substitutes **0-based**; `pse:lastRead` is **1-based**;
  `{maxWidth}` may be ignored server-side — request it, don't rely on it.
- **Lazy PSE pattern:** feed entries may omit the stream link entirely; it
  appears only on the complete entry behind `rel="alternate"` +
  `type=application/atom+xml;type=entry;profile=opds-catalog`. The browse
  loop is feed → follow alternate → stream.
- The link's `type` (page image media type) is advisory; trust the HTTP
  `Content-Type` of each page response. Expect JPEG/PNG/WebP, occasionally
  AVIF.
- Comic servers commonly write reading progress server-side as pages are
  fetched, and some populate `pse:lastRead(-Date)` for resume — offer
  "resume at page N" when present.
- First fetch of a cold chapter can be slow (upstream servers rate-limit);
  use generous timeouts and shallow prefetch (1–2 pages ahead).

## 4. Auth

Two tiers, both required:

1. **HTTP Basic**, including mid-flow: any request — feed, search,
   acquisition, page image — may return 401. Re-prompt/retry with
   credentials; persist per-catalog. (This is all several popular reader
   clients support, and all many servers offer.) The client holds an
   opaque `Authorization` header value rather than a username and
   password, so a bearer token or a per-user API key is the same field and
   needs no protocol work here; persistence is the host's, behind
   `chapbook_core::CredentialStore`. Anything that cannot be one constant
   header — per-request signing, cookie sessions — is the injected
   `HttpClient`'s job instead.
2. **OPDS Authentication Document** (`application/opds-authentication+json`):
   well-behaved servers return this JSON alongside 401. Parse it
   (fixture: `authentication.opds-auth.json`): `title`, `description`,
   `authentication[]` flows (support `http://opds-spec.org/auth/basic`;
   recognize-and-decline others gracefully), `links` (logo, help, register)
   — and render a proper native login dialog instead of a raw failure.
   Desktop-class readers (Thorium, Cantook) do this; matching them is the
   bar.

Some servers put per-user API keys in the catalog URL path instead of using
auth headers — treat the catalog URL as an opaque secret-bearing string
(don't log it, don't normalize it).

## 5. Caching and downloads

- **No conditional requests to count on:** many servers emit no
  `ETag`/`Last-Modified`/`Cache-Control` on feeds or files. The client owns
  freshness policy (short TTL per catalog; manual refresh).
- **No Range support to count on:** downloads may be fully buffered
  server-side. An interrupted download is a restart, not a resume — download
  to a temp file, atomically rename on completion.
- Follow redirects on acquisition links, including cross-host (covers and
  files may live on a CDN or object store).

## 6. Position sync (OPDS Progression 1.0) — behind a feature

Everything above is observed behavior of shipping servers. This section is
not: **Progression 1.0 is an unreleased draft**, published at
`drafts.opds.io` and absent from `specs.opds.io`, which lists only OPDS
2.0, 1.2, 1.1, 1.0 and 0.9 as released. So the implementation sits behind
the non-default `progression` feature, and the gate is about *surface*, not
size — the feature adds no dependencies. Turning it on is a caller
accepting that these types may move when the draft does. The tests copy the
draft's own examples verbatim, so a revision shows up as a failure rather
than as drift.

For the same reason, no server survey backs this section: it is written
from the spec, and the first real service to speak it may well move things
here. What the code commits to:

- **The service URL is the publication's identity.** A progression service
  is per publication, discovered from a link with
  `rel="http://opds-spec.org/progression"` and type
  `application/opds-progression+json` — in either dialect, on an Atom entry
  or on an OPDS 2.0 publication document. Nothing in the document names the
  publication, so a client that wants to sync must persist the URL it found
  alongside the book. A book that arrived any other way — sideloaded,
  adopted from bytes — has no service to talk to.
- **Empty is an answer.** A successful `GET` may return 200 with an empty
  body, meaning "nothing recorded yet". Treating that as a parse failure
  would turn the ordinary first-sync case into an error, so
  `fetch_progression` returns `Option`, and a whitespace-only body counts
  as empty.
- **`PUT` offers, the server decides.** 200/201 return the service's
  resulting document, which is authoritative over what was sent. The four
  documented failure codes (400, two 403s, 409) are *refusals*, not
  errors: two devices reading one book race routinely, so they come back as
  a value the caller must handle. Anything else non-2xx — a 404 at a dead
  URL, a 5xx — is an error.
- **The two 403s are only separable by RFC 7807 Problem Details.** The
  draft gives `progression-incorrect-user` and `progression-locked` the
  same status and distinguishes them by the `type` URI. A server that sends
  no problem document is therefore classified `Unknown`, which is the
  truthful answer; do not guess between "wrong user" and "stop asking".
- **Conflict resolution is not specified.** A 409 says the service holds
  something more recent and says nothing about who should win. That policy
  belongs to whatever binds this to a library, not here.
- **401 behaves like everywhere else** in this crate: the Authentication
  Document surfaces through `OpdsError::AuthRequired`, so §4's flow covers
  progression with no special case. The draft's `properties.authenticate`
  link hint, which would save a round-trip, is deliberately not modelled —
  it would mean putting draft-shaped fields on the ungated `Link`.
- **`PUT` is the crate's only write,** and it goes through the same
  `HttpClient::send` as every GET, with the method on the request. A
  transport that only reads must refuse it rather than silently drop the
  body — a write that vanishes looks to a reader exactly like a position
  that syncs and never persists.
