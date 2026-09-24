# chapbook architecture

Core principle: **no webview, no full HTML5 browser.** Chapbook implements the
EPUB 3 standards — XHTML content documents and the EPUB 3 CSS profile (CSS 2.1
plus selected CSS3 modules) — with a purpose-built, pagination-first pipeline.

## Pipeline

```
.epub ──chapbook-epub──▶ XHTML bytes + CSS + resources
      ──chapbook-layout▶ arena Document (html5ever parse)           [::dom]
                       ▶ ComputedValues per element (stylo cascade) [::cascade]
                       ▶ ChapterLayout { pages, anchors, char_map } (cosmic-text)
      ──chapbook-paint─▶ DisplayList per page
      ──render backend─▶ pixels (tiny-skia CPU today; e-ink/GPU later)
```

Everything above the render backend works in CSS px and has no GPU or vsync
assumptions; hidpi scaling happens only inside a backend.

## Format extensibility (EPUB-first, not EPUB-only)

Chapbook's focus is the reflowable-EPUB pipeline above, but the seams are
placed so an image-per-page format joins without a redesign — and three
have:

- **Book model in core.** `BookMetadata`/`SpineItem`/`TocEntry`/`Resource`
  and the `Publication` trait live in `chapbook-core`. The library, reader
  session, and CLI program against that surface — and import it from core,
  never via an EPUB re-export, so `grep chapbook_epub` stays an honest
  coupling map. Four producers exist: `chapbook-epub` (rbook), `chapbook-cbz`
  (each archive image = one spine item, natural-sorted, media type guessed
  from extension — no manifest, but a `ComicInfo.xml` sidecar, when the
  archive carries one, supplies metadata and `Page/@Bookmark` toc entries),
  `chapbook_opds::StreamedComic` (OPDS-PSE
  page streaming, one HTTP fetch per page through a 0-based `{pageNumber}`
  template, disk-cached), and `chapbook-pdf` (pages rasterized at 2×
  via hayro — pure Rust, CPU-only; encrypted PDFs rejected; MSRV floor is
  hayro's 1.92 — with the catalog's `/Outlines` walked into a nested toc,
  plus a text layer: a recording `Device` captures per-glyph
  Unicode and geometry during interpretation, reading order reconstructed
  by baseline clustering, surfacing as `HiddenText` fragments so selection
  works on PDF pages exactly as on reflowed text). The streamed producer is what the
  `Publication::unit_bytes` blocking contract was written for: it may take
  seconds and fail with `ChapbookError::Network`, and UI code must call it
  off the UI thread.
- **Page model in paint.** `chapbook-paint` owns `Page`/`Fragment`/
  `DisplayList`; `chapbook-layout` *produces* into it. A comic page becomes a
  single image-fragment `Page` (`chapbook_paint::image_page`) — no DOM, no
  stylo, no cosmic-text shaping — and the render backends and viewers never
  know the difference. Fragments reference their source via an opaque `u64`
  tag, never a DOM type.
- **Per-format progression units.** Locators count a format-defined unit:
  chars of locator text for reflowable EPUB; pages for image formats
  (`char_offset = 0`, empty quote layer — the resolve chain already degrades
  past it to the fraction layers). See `chapbook_core::locator` docs.
- **OPDS is already format-neutral** — acquisition links carry media types
  (`application/vnd.comicbook+zip` works the same as EPUB's).

Everything else — dom, style, layout — is deliberately text-specific and
stays that way.

## Crate boundaries

- **chapbook-core** — geometry, `PageMetrics`, `ReadingSettings`,
  `FontSource` (where faces come from, what the five CSS generics mean, and
  what to try when a glyph is missing — three axes that `FontSystem::new()`
  decides from `cfg` and silently, which is why they are said out loud
  here; `chapbook-layout` realizes it, and no other crate touches
  `fontdb::Database`), the
  format-neutral book model (`Publication`, `BookMetadata`, `SpineItem`,
  `TocEntry`), and `Locator { spine_index, char_offset }` plus the layered,
  versioned persistence record (`LayeredLocator`: quote context, spine
  fraction, whole-book progression) and its resolve chain — spec in
  `chapbook_core::locator`'s module docs, rationale in `docs/LOCATORS.md`.
  `char_offset` indexes the *raw locator text*
  (`chapbook_layout::dom::locator_text`, versioned by `LOCATOR_VERSION`),
  not the
  collapsed display text. It also re-exports the three panel types the page
  model itself speaks — `UpdateClass`, `PanelRect`, `PixelFormat` — from
  mezzotint, so those still come from one place. No heavy deps.
- **chapbook-epub** — wraps `rbook` for OCF/OPF/spine/TOC; adds relative
  resource resolution, fixed-layout detection (rejected), font
  de-obfuscation (M5). The wrapper boundary means rbook gaps can be patched
  with `zip` + `quick-xml` per-field without touching consumers.
- **chapbook-layout** — the differentiator, and the whole stylo-facing half
  of the engine. One crate rather than three, because it moves as one: the
  DOM binding, the cascade driver, and layout are all shaped by the pinned
  stylo set, and an upgrade rewrites them together. Two modules sit under
  layout proper:
  - `dom` — slotmap arena `Document`; the copyable `DomNode<'a>` handle
    carries all stylo trait impls (`TNode`/`TDocument`/`TElement`/
    `selectors::Element`), structured after blitz-dom's proven binding. The
    handle **must stay pointer-sized** (stylo's style sharing cache
    statically asserts it), so `Document` heap-boxes a `DocumentInner` with
    a stable address, every node carries a sealed back-pointer + self-id,
    and `DomNode<'a>` is a `&'a Node` newtype. Documents are static after
    parse: no incremental restyle, no snapshots, no shadow DOM, no
    animations, no scripting — which deletes most of stylo's invalidation
    surface. Parsing is lenient html5ever by default (real EPUBs contain
    HTML-isms); a `strict-xml` feature runs xml5ever on the same tree
    builder.
  - `cascade` — owns the `Stylist` + media `Device` ("screen"), embeds the
    UA stylesheet (`ua.css` — the profile boundary: what is not in the EPUB
    3 CSS profile gets no UA support), registers author sheets from the
    chapter and user-origin override sheets (reader settings, themes), runs
    the restyle traversal, and exposes `@font-face` rules for fontdb
    registration (`crate::webfonts` does the registering). Its
    `FontMetricsProvider` returns no metrics, so `ex`/`ch` resolve through
    stylo's own approximations — an accepted gap, not a placeholder.
    Themes (`chapbook_core::Theme`): `Light` is the identity theme; `Sepia`
    recolors the defaults at user origin (publisher colors win); `Dark`
    forces text/background colors with `!important` for night-mode
    readability and flips the device's `prefers-color-scheme`. The page
    ground is the display list's first op, chosen by the caller from the
    theme.

  Layout proper: box tree per CSS 2.1 §9.2
  (anonymous blocks, `::before`/`::after`), block flow, each inline formatting
  context laid out as one `cosmic_text::Buffer` (per-span `Attrs` from
  `ComputedValues`, `metadata` = span index for DOM mapping and locators).
  A streaming page cursor applies CSS fragmentation: forced
  `break-before/after: page` (+ legacy `page-break-*` aliases),
  `break-inside: avoid` (retry on a fresh page, else break anyway),
  widows/orphans (default 2/2), margins discarded at page boundaries,
  oversized monolithic boxes sliced graphically (first/last-slice flags gate
  border painting).
  **Fragmentation sidecar:** servo-mode stylo does not implement the
  fragmentation properties (`break-*`, `page-break-*`, `widows`, `orphans`
  are Gecko-only — they land in its counted-unknown bucket), so
  chapbook-layout runs its own mini-cascade for just those declarations:
  cssparser parses them out of the same sheets, and their selectors match
  through our existing `selectors::Element` impl with standard
  specificity/order rules. `hyphens` (also Gecko-only, inherited) rides in
  the same sidecar.
  **Floats:** `float: left/right` on images (intrinsic size) and on
  non-replaced blocks with an explicit CSS width (laid out in a detached
  sub-paginator, fragments translated into place) places the float against
  a content edge and shortens the line boxes of following inline content
  beside it (the IFC is split at the float's bottom edge and re-shaped —
  the same machinery as first-line indents, which also apply beside
  floats); `clear` works on any block; floats never cross a page
  boundary.
  **Hyphenation:** `hyphens: auto` inserts soft hyphens at embedded en-US
  Knuth-Liang dictionary points before shaping — cosmic-text's line breaker
  already treats U+00AD as a break opportunity and its shaper renders it
  invisible — and lines that break at one get a visible hyphen glyph
  appended, with justified lines re-tightened over their spaces.
  Output: `ChapterLayout` — chapbook-paint `Page`s plus the
  text-specific side tables (`anchors: id→page`, `char_map:
  char_offset→page`, fragment-tag→DOM-node mapping). One spine item = one
  layout run, cached by `(spine_idx, PageMetrics, settings_hash, css_hash)`;
  whole-book page numbers are computed lazily chapter-by-chapter.
  `style_to_attrs.rs` documents the supported `ComputedValues → Attrs` subset
  and its fallbacks.
- **chapbook-paint** — owns the format-neutral page model (`Page`/`Fragment`,
  fragments tagged with an opaque producer-defined `u64`, never a DOM type),
  the `Frame` a backend consumes (ops plus a `FrameIntent` and optional
  damage rect, so an e-ink panel can choose a refresh mode;
  `FrameIntent::update_class` maps it to an `Option<UpdateClass>` — `None`
  for a plain repaint, because nothing to show is the absence of an update
  rather than a kind of one — and damage accumulates independently of the
  intent ordering so a highlight does not discard the region a live
  selection already named), the orientation policy every backend shares
  (`rotate` for the pixels, `panel_rect` for a rect turned the same way, so
  a damage region and the pixels under it agree — properties of the target,
  not of the rasterizer), and the
  dumb display ops. There are exactly three:
  `FillRect`, `GlyphRun { fontdb::ID, glyphs }` (no re-shaping at paint
  time), and `Image`. Borders, box backgrounds, rules, and text
  decorations all lower to `FillRect` before they get here, which is what
  keeps a backend small. Layout produces into it; image
  formats will too. CSS `filter` on an image never reaches a backend
  either: layout asks the `ImageStore` to *derive* a copy — the element's
  background composited first, then the spec's colour functions
  (`invert`, `grayscale`, `sepia`, `saturate`, `hue-rotate`, `brightness`,
  `contrast`, `opacity`; not `blur`/`drop-shadow`/`url`) — and points the
  fragment at it, so a dark-scheme `invert(100%)` is a different resource
  id, not a new op. The background rides along because CSS filters the
  whole element: a book that paints black-on-transparent line art on white
  and inverts it in the dark wants white-on-black, not white-on-white.
- **chapbook-render-tinyskia** — swash glyph raster cache, `image`-decoded
  resources, scale applied here. Glyph baselines snap to whole device
  pixels (swash applies cosmic-text's vertical sub-pixel bin in the
  opposite direction, so the true fraction lifts every line); horizontal
  sub-pixel positioning is kept.
- **chapbook-render-vello** — the GPU backend, over the same display list:
  vello's glyph API takes pre-positioned glyph ids, so a `GlyphRun`
  transcribes onto it, and device scale becomes a scene transform. Renders
  offscreen through wgpu with readback. Its parity test against the CPU
  backend is what keeps the seam a contract rather than a data structure.
- **opds-client** — the OPDS itself, and the one member with no chapbook
  dependency: it is a standalone catalog client that happens to live here.
  OPDS 1.2 Atom as the canonical dialect, parsed at the XML level with
  namespace-aware `quick-xml` (NOT `atom_syndication`/`feed-rs`: both
  silently drop the foreign-namespace link attributes that facets and
  OPDS-PSE page streaming live in); OPDS 2.0 JSON via serde as a secondary
  parser. Pagination (`next`/`previous` + OpenSearch totals), search,
  facets, acquisition download (complete or not at all; no Range resume
  assumed), HTTP Basic at any point in a flow plus OPDS Authentication
  Document login.
  **It opens no sockets.** The caller injects an `HttpClient` — a blocking
  two-method trait (`get`, `send`) over `HttpRequest`/`HttpResponse` —
  because a bundled networking stack is what `docs/PLATFORM.md` found costs
  an iOS app background transfer, system trust and ATS, costs Android
  `WorkManager`, and is simply unavailable in WASM. `UreqHttp` (blocking
  `ureq` + rustls, no async runtime) is one implementation behind the
  default `ureq` feature; `--no-default-features` drops ureq, rustls and
  the root store and the crate still does everything but fetch.
  **A blocking transport is not background transfer**, and no override of
  it can be: a transfer that must survive the app being suspended is a
  job, not a call. `OpdsClient::download` fetches through the transport
  (streams to a `.part`, renames) for a process that stays alive; a phone
  takes the other door — `Entry::download_request` describes the fetch
  (URL, suggested filename, media type; never a credential) and steps
  aside, the host runs it under `WorkManager` or a background
  `URLSession`, and the finished file comes back through the library's
  import. The two sync service links are read *before* the transfer,
  because by the time a background download lands the feed is gone.
  Full requirements: `crates/opds-client/INTEROP.md` (it travels with the
  crate); wire-format fixtures: `fixtures/opds/`.
- **chapbook-opds** — the binding, and the only part of the above that
  could not travel: `StreamedComic`, an OPDS-PSE stream presented as a
  `Publication` (one HTTP fetch per page through a 0-based `{pageNumber}`
  template, disk-cached, lazy stream links followed at open), plus the
  conversion from `OpdsError` into `ChapbookError`. Re-exports
  `opds-client` wholesale, so consumers inside the workspace name one
  crate.
- **chapbook-library** — rusqlite (bundled, WAL): books/authors, positions,
  annotations, opds_sources, collections, plus covers kept at import.
  Positions are `Locator`s and survive relayout via the char_map.
  `BookQuery` is the one browsing entry point — free-text search,
  collection, series, reading state, sort, paging — with `books()` and
  `recent()` the two shapes worth naming over it. A record carries cover,
  progress, last-read, series and collections, because a shelf asking per
  book turns one query into N. Search is an FTS5 index over title, authors
  and series with `remove_diacritics 2`, not `LIKE`: SQLite folds case for
  ASCII only, so the substring version could not match "bronte" against
  Brontë. Reading state is `Unread`/`Reading`/`Finished`, and only
  `Finished` is stored — progress reads 1.0 for a skimmed book and resets
  when a finished one is reopened, so deriving it gets both wrong.
- **chapbook-reader** — the shared reading session, extracted so viewer
  shells stay thin: source dispatch (`.epub`/`.cbz`/OPDS URL), per-unit
  layout+image caches (text units run the full pipeline; comic units
  fabricate a one-page layout around an image fragment), navigation,
  font/theme settings, selection (hit-testing via per-glyph locator offsets
  on `chapbook-paint` fragments; copied as text, or stored as a highlight
  that re-anchors through the locator chain), and layered-locator
  persistence. Comics persist page-unit progression. Output is a `Frame`
  (`Session::frame`) plus the fonts and images its ops resolve against
  (`paint_resources`); `Session::render` is the bundled CPU rasterizer over
  that, not a separate path. The formats beyond EPUB are separately
  compilable — `cbz`, `pdf` and `opds` features, all on by default — so a
  device build drops the ones its hardware will never open; the public API
  is the same in every configuration, and an excluded format fails with
  `ChapbookError::FormatNotBuilt` rather than being mistaken for something
  else.
- **chapbook-viewer** / **chapbook-viewer-gtk** / **chapbook-viewer-win32**
  — winit+softbuffer, GTK4 and Win32 shells over
  `chapbook-reader::Session`; each translates input events and blits the
  session's rasterized page, nothing more. `chapbook-viewer --gpu` is the
  shell that rasterizes for itself: it takes `Session::frame` and
  `paint_resources` and presents through chapbook-render-vello's window
  surface, never calling `render()`. The Win32 one varies a different
  axis — who owns the loop. winit and GTK own it and call into the shell;
  there the shell calls `GetMessageW` itself, which is the shape a host
  embedding chapbook as a child window is in. The session needs no
  knowledge of which one it is talking to, which is the evidence that the
  seam is a seam.
- **tools/chapbook-cli** — `meta|toc|text|styles|layout|render|opds|lib`;
  each subcommand exposes one pipeline stage and generates the snapshot
  inputs for that stage's golden tests. `--features fbdev --example show`
  is the device-shell demonstration: a whole reading session onto
  `/dev/fb0`, written the long way so the display list, panel policy,
  damage, intent and the driver's rules all get walked.

### The panel seam is mezzotint

Everything below "here are the pixels, and here is what kind of change they
are" lives in [mezzotint], an external crate. It holds the `Panel` trait
(`blit`/`submit`/`wait` — submit returns a token because an e-ink update
takes 100ms to a second and blocking on it would make page turns feel
broken), `UpdateClass` as the vendor-neutral half of a waveform choice,
`PanelRect` rounding outward once for every backend, `RefreshPolicy` for
ghosting debt, `PanelDriver` enforcing the rules above a panel (never blit
under an in-flight update that overlaps; repaint what a monochrome update
degraded when `settle` says the gesture ended; ration the flash),
`RecordingPanel` so all of it is testable with no panel attached, the
`encode` reduction path, and the backends — including the Linux framebuffer
one that used to be `chapbook-panel-fbdev`.

It was chapbook's own module until it turned out to have nothing to do with
books: nothing under that seam knows what a page is, and the same code
serves any application that puts pixels on an electrophoretic display. So
chapbook is an ordinary consumer of it now, which is also the honest test of
whether the seam was real.

[mezzotint]: https://crates.io/crates/mezzotint

## Version policy

The stylo lockstep set (`stylo`, `stylo_traits`, `stylo_atoms`,
`stylo_static_prefs`, `stylo_dom`, `selectors`, `cssparser`) is pinned with
`=` in the workspace and upgraded all-at-once as a deliberate task, using
Blitz's corresponding upgrade diff as the migration guide. html5ever/
markup5ever/xml5ever must match the markup5ever minor that stylo's selector
types use. MSRV 1.92 (hayro's floor; stylo 0.20 needs 1.89), stable toolchain — no nightly.

## Explicitly out of scope

Fixed-layout EPUB (detected, rejected with a clear error), JavaScript
(spec-permitted omission for reading systems), inline MathML layout
(block `<math display="block">` renders natively — see "Foreign content"
below — inline math takes the spec-provided fallback for non-MathML
reading systems: `<math altimg>` renders the publisher's equation image,
`<math alttext>` renders as text — `chapbook-layout`'s
`dom::math_fallback`), vertical writing modes, shrink-to-fit floated
blocks (floated images and floated blocks with an explicit width lay out
for real; the rest stay in flow), CSS counters in generated content,
absolute positioning (treated as static), media overlays, DRM.

## Foreign content: MathML and SVG

Both render without the DOM, the cascade, or the paint seam learning they
exist, and both are features of `chapbook-layout` (default on; the
stripped e-ink profile's knobs):

- **MathML** (`mathml`). The DOM rewrite (`dom::math_fallback`) still runs
  unconditionally — the post-rewrite tree is the locator-text authority,
  identical on every build. What the feature adds is rendering: each
  outermost `<math>` is serialized before the rewrite (`dom::foreign`), and
  at paginate time `mathml::prepare` lays out every block-mode formula with
  the `formulary` crate against a MATH-table face from the session fontdb
  (STIX Two Math is embedded as the face of last resort; a publisher's
  `@font-face` math font wins over it). A prepared formula becomes a
  replaced block that lowers to an ordinary line fragment — glyph runs on
  the math face, fraction bars and `mathbackground` fills as decorations —
  so the display list keeps its three ops and both renderers are untouched.
  A formula defers to the publisher's fallback when it is inline, when
  formulary reports unsupported structure and a fallback exists, when the
  layout needs mirrored glyphs (RTL math), or when no MATH font is loaded.
- **SVG** (`svg`). Rasterized by resvg at `collect_images` time into the
  shared `ImageStore` — RGBA at the SVG's intrinsic size (the store
  premultiplies once at insert, so frames composite in place) — so an
  SVG is indistinguishable from a decoded PNG downstream, dithering
  regions included. Covers `<img src>` pointing at SVG (content-sniffed,
  never by extension) and inline `<svg>` subtrees (serialized at parse
  time, replaced in the box tree only once rasterized; otherwise they
  keep flattening to their text, so locator text never depends on the
  feature). `<image>` hrefs inside SVG resolve through the same fetch
  closure as everything else; SVG `<text>` shapes against a fontdb copied
  from the session's `FontSystem`, never the host's font list.

## The text surface

The current page's text with geometry, the layer under accessibility,
TTS, and dictionary lookup — built, not deferred. `Session::page_text_runs`
gives one `TextRun` per visual line (`{text, rect, locator_start,
locator_end}`, page space); `speakable_page` gives the page as one
collapsed string plus a `WordSpan` table mapping speech progress back to
locator space (segmented in locator space, so spans feed `range_rects`
and `select_range` directly); `word_at` answers a dictionary tap. Built
over `LineFragment` text and per-glyph locators — deliberately *not* the
display list, which carries glyph indices and no text; chapbook-reader's
`text_surface` module docs have that argument. Each platform wraps the
runs in its own tree
(`UIAccessibilityElement`, `AccessibilityNodeInfo`, AT-SPI, UI
Automation); the GTK viewer's `PageArea` is the reference, verified
against AT-SPI end to end, and the Win32 viewer's `uia` module is the
second implementation, verified against a real UIA client. Two of them
is what turned the accessor from a data structure into a seam: AT-SPI
asks for text at a granularity around an offset, UIA asks for a range
object that moves its own endpoints by unit, and the same three calls
answer both.
