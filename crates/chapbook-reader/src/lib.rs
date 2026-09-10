//! The shared reading session: everything a viewer shell needs that isn't
//! windowing.
//!
//! [`Session`] owns the open publication (EPUB, local CBZ, or an OPDS-PSE
//! stream — dispatched by [`Session::open`]), the font system, per-chapter
//! layout and image caches, reading settings, the selection, and the
//! library glue (import/match on open, layered-locator persistence on
//! save). Shells — winit, GTK, anything with a keyboard and a pixel
//! buffer — translate input events into `Session` calls and blit the
//! [`Session::render`] result. A shell that rasterizes for itself takes
//! [`Session::frame`] instead and never touches tiny-skia.
//!
//! Text units run the full dom→stylo→layout pipeline; comic units fabricate
//! a one-page [`ChapterLayout`] around a single scaled image fragment, so
//! navigation, the char map, and position persistence are one code path.
//!
//! `docs/SHELLS.md` is the contract from the shell's side — the loop, the
//! loader rule, metrics, position, panel policy — and
//! [`conformance`] is that contract as runnable assertions.
//!
//! Blocking caveat, inherited from `Publication::unit_bytes`: layout of a
//! not-yet-cached unit may block on I/O (seconds, for a cold PSE page).
//! These shells are dev harnesses and call it on the UI thread anyway —
//! the documented debt; a loader thread slots in here, not in the shells.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

#[cfg(feature = "library")]
mod annotations;
mod cache;
pub mod conformance;
mod frame;
mod layout;
#[cfg(feature = "_image-book")]
mod loader;
mod nav;
mod open;
mod render;
mod text_surface;
mod zoom;

#[cfg(feature = "library")]
use chapbook_core::LayeredLocator;
use chapbook_core::{
    BookKind, CredentialStore, FontReport, FontSource, Locator, NoCredentials, PageMetrics,
    PixelFormat, ReadingSettings, Rect,
};
use chapbook_layout::{dom, ChapterLayout};
use chapbook_paint::{FrameIntent, ImageStore};

#[cfg(feature = "library")]
use annotations::StoredAnnotation;
use frame::PendingDamage;
#[cfg(feature = "_image-book")]
use loader::Loader;
use open::OpenBook;

#[cfg(feature = "library")]
pub use annotations::{AnnotationSummary, Highlight};

// Everything a shell needs to consume what the session produces, so it
// depends on chapbook-reader alone and can't skew versions with it: the
// display-list vocabulary, the font database its glyph runs name faces
// in, and the bundled CPU backend.
pub use chapbook_core;
// Annotation kinds and library records surface in this crate's own API.
#[cfg(feature = "library")]
pub use chapbook_library;
pub use chapbook_paint;
pub use chapbook_render_tinyskia;
pub use chapbook_render_tinyskia::tiny_skia;
pub use cosmic_text;
// A shell that owns its networking implements this, so it has to be
// nameable from here — the same rule as the display-list vocabulary above.
#[cfg(feature = "opds")]
pub use chapbook_opds::http::{HttpClient, HttpError, HttpRequest, HttpResponse};
// The catalog client itself, for a binding that browses one. Re-exported
// rather than depended on directly, so a shell keeps depending on this
// crate alone — `docs/STABILITY.md`'s rule — and cannot skew versions
// with the engine it drives.
#[cfg(feature = "opds")]
pub use chapbook_opds;
pub use open::open_publication;

/// Waker slot shared with the loader thread; the shell installs its wakeup
/// (an event-loop proxy, a main-context poke) after the session exists.
type WakerCell = Arc<Mutex<Option<Box<dyn Fn() + Send + Sync>>>>;

/// Metrics-independent record of a loaded image-book unit (the pixels are
/// in the session's image store).
struct LoadedUnit {
    width: u32,
    height: u32,
    text: Vec<chapbook_core::TextLine>,
    natural: (f32, f32),
}

/// Where a settings change should stick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsScope {
    /// The reader's default, for every book without an override.
    Global,
    /// This book only. It keeps these settings when the default changes.
    ThisBook,
}

/// One search hit, in the same locator space positions and annotations
/// live in — so a hit feeds straight into [`Session::goto`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    /// Unit and offset of the match's first character.
    pub locator: Locator,
    /// Offset just past the match, within the same unit.
    pub end: u32,
    /// The match with a little text on either side, whitespace collapsed,
    /// for a results list.
    pub context: String,
    /// Char range of the match within `context`.
    pub match_range: (u32, u32),
}

/// One run of the current page's text with its geometry — the material an
/// accessibility tree, TTS, or a selection loupe is built from, and
/// deliberately not the display list, which carries glyph indices and no
/// text.
///
/// Rects are page space, CSS px; a shell drawing in a rotated panel maps
/// them with [`chapbook_core::PageMetrics::page_to_panel`].
#[derive(Debug, Clone, PartialEq)]
pub struct TextRun {
    /// The line's text as shaped: whitespace collapsed, soft hyphens
    /// stripped, generated marks (list markers, break hyphens) included —
    /// so its char count is *not* the locator span's width.
    pub text: String,
    /// Bounding rect of the line, in page space.
    pub rect: Rect,
    /// Locator range `[start, end)` in the unit's locator space — feeds
    /// [`Session::range_rects`], [`Session::select_range`], and
    /// [`Session::goto`].
    pub locator_start: u32,
    pub locator_end: u32,
}

/// One word on the current page: where it sits in the speakable string
/// and in locator space. A TTS engine reports progress as ranges into the
/// string it was handed; the locator range is how that progress comes
/// back to the page — feed it to [`Session::range_rects`] for the
/// highlight, or [`Session::select_range`] for dictionary lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WordSpan {
    /// Char range `[start, end)` within [`SpeakablePage::text`].
    pub text_start: u32,
    pub text_end: u32,
    /// Locator range `[start, end)` in the unit's locator space.
    pub locator_start: u32,
    pub locator_end: u32,
}

/// The current page as a TTS engine wants it: one collapsed string, plus
/// the word table that maps speech progress back into locator space.
/// Punctuation is spoken but is nobody's word; whitespace collapses the
/// way [`Session::selected_text`] collapses it, so the two agree.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SpeakablePage {
    pub text: String,
    /// Words in reading order; ranges never overlap.
    pub words: Vec<WordSpan>,
}

// Annotations are the library's: without a place to store them there

/// Where the reader is: which spine unit, and which page inside it.
///
/// The pair, never the page alone. `next_page` crosses into the next unit
/// by resetting the page to 0, so a shell comparing `page()` across a turn
/// reads a successful move as "did not move" — and since most books open on
/// a single-page cover, it reads that on the very first turn. That is not
/// hypothetical: it is what stopped the fbdev shell dead on its first run
/// against real hardware.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub spine: usize,
    pub page: usize,
}

/// Something the shell may want to react to, drained from
/// [`Session::drain_events`].
///
/// Deliberately *not* about drawing. What to repaint is
/// [`chapbook_paint::FrameIntent`], which a shell already gets from
/// `frame()` and which says it better — these are the things a shell acts
/// on rather than paints: telling the reader a page will not load, moving
/// a progress bar, marking a book read, pushing a position to a sync
/// service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    /// A background unit finished decoding. Includes prefetches, which
    /// [`Session::poll_loaded`] deliberately reports as "nothing visible
    /// changed" — a shell watching load progress wants both, and a shell
    /// deciding whether to repaint should keep using `poll_loaded`.
    UnitLoaded { spine: usize },
    /// A background unit failed and will not be retried.
    ///
    /// The reason this type exists. The failure was recorded internally so
    /// a retry would not be queued every frame, and there it stopped: a
    /// comic page that failed to download stayed a placeholder forever
    /// with nothing able to say why. `message` is for a person to read and
    /// is free to change — do not match on it.
    UnitFailed { spine: usize, message: String },
    /// The reader is somewhere else. Reported for moves the shell did not
    /// make as well as ones it did — a restored position resolving after
    /// open, a load landing that settles the page — which is what a
    /// progress UI and a sync client both need and neither can see today.
    PositionChanged { spine: usize, page: usize },
    /// The reader reached the last page of the last unit.
    ///
    /// Fires on the transition, not on every drain that finds them there,
    /// and re-arms if they leave and come back. Whether that means "mark
    /// as read" is the shell's policy, not the engine's.
    BookFinished,
}

/// One open book and everything needed to read it.
pub struct Session {
    book: OpenBook,
    title: String,
    fonts: cosmic_text::FontSystem,
    /// What the font source actually produced, kept so a shell can say so.
    font_report: FontReport,
    renderer: chapbook_render_tinyskia::Renderer,
    settings: ReadingSettings,
    metrics: Option<PageMetrics>,
    /// The image-book zoom view, `None` at fit — see [`zoom`](self) for
    /// the vocabulary. View state, not reading state: nothing persists it.
    view: Option<zoom::PageView>,
    /// Everything the session caches per spine unit — see [`UnitState`].
    units: HashMap<usize, UnitState>,
    /// Bytes the unit caches may hold between them.
    cache_budget: usize,
    /// Ticks [`UnitState::used_at`]. A counter rather than a clock:
    /// monotonic, cheap, and it cannot go backwards when the host's
    /// clock does.
    use_clock: u64,
    registered_fonts: HashSet<String>,
    spine: usize,
    page: usize,
    /// Restored char offset, turned into a page once the unit lays out.
    ///
    /// Paired with the unit it was captured in, and *only* applied while
    /// the reader is still there. A restore lands in `frame()`, because
    /// the offset cannot become a page until the unit has laid out, and
    /// nothing obliges a shell to paint before it navigates: a batched
    /// turn, or a harness, can walk the whole book first. Unpaired, the
    /// offset then resolved against whatever unit the reader had reached
    /// and moved them backwards inside it — see
    /// `a_restore_does_not_follow_the_reader_into_another_unit`.
    pending_offset: Option<(usize, u32)>,
    /// Worker for image-book units (comics, PDFs); `None` for EPUBs.
    #[cfg(feature = "_image-book")]
    loader: Option<Loader>,
    /// Units the loader failed on, so a retry isn't queued every frame.
    /// Deliberately not unit state: eviction must not clear the memory
    /// of a failure, or every eviction would queue the failing load again.
    #[cfg(feature = "_image-book")]
    load_errors: HashMap<usize, String>,
    waker: WakerCell,
    /// Discrete events waiting to be drained. Only facts that *happen* —
    /// derived ones are computed at drain time, so no mutation site has to
    /// remember to record them.
    events: Vec<SessionEvent>,
    /// The position the last drain reported, so the next one can tell
    /// whether the reader moved.
    reported_position: Position,
    /// Whether the last drain found the reader at the end, so
    /// [`SessionEvent::BookFinished`] fires on the transition rather than
    /// on every drain.
    reported_finished: bool,
    /// Selection anchor and cursor as locator offsets (unordered).
    selection: Option<(u32, u32)>,
    #[cfg(feature = "library")]
    library: Option<chapbook_library::Library>,
    /// Where the library lives, so [`Session::suspend`] can close it and a
    /// later access can open it again.
    #[cfg(feature = "library")]
    library_dir: Option<std::path::PathBuf>,
    /// Set by [`Session::suspend`]: the database is deliberately closed and
    /// the next access should reopen rather than treat `None` as "this
    /// platform has no library".
    #[cfg(feature = "library")]
    suspended: bool,
    #[cfg(feature = "library")]
    book_id: OpenedBookId,
    /// Annotations as stored, awaiting resolution against unit text.
    #[cfg(feature = "library")]
    stored: Vec<StoredAnnotation>,
    /// The open file is the edition the positions were captured against.
    #[cfg(feature = "library")]
    same_edition: bool,
    /// Handed out for units with no images of their own, so
    /// [`Session::image_store`] can return a reference either way.
    empty_images: ImageStore,
    /// What has changed since the last frame was taken.
    pending: FrameIntent,
    pending_damage: PendingDamage,
    /// The selection as of the last frame — the other half of a selection
    /// change, needed to damage what it used to cover.
    painted_selection: Option<(u32, u32)>,
    /// What the target panel can show; applied to rendered pixels.
    pixel_format: PixelFormat,
    /// Where jumps came from, so a footnote can be returned from. Only
    /// jumps push; ordinary page turns don't.
    back_stack: Vec<Locator>,
    /// Per-unit locator-text char counts, computed once per book: a
    /// property of the file, not of any layout, so it never invalidates.
    /// Position capture needs the whole spine's counts, and without this
    /// every save re-inflated and re-parsed every chapter — on a callback
    /// (`suspend`) with a documented time budget.
    #[cfg(feature = "library")]
    char_counts: std::cell::OnceCell<Vec<u64>>,
    /// The most recently extracted unit locator text. One entry, replaced
    /// on a different unit: selection, capture, search, and highlight
    /// resolution ask for the same unit in bursts, and each ask was an
    /// inflate + parse + walk. Survives relayout by design — locator text
    /// is metrics-independent.
    unit_text_cache: std::cell::RefCell<Option<(usize, String)>>,
    /// Fragment to land on once the target unit has laid out — the
    /// anchor-flavored sibling of `pending_offset`, unit-paired for the
    /// same reason.
    pending_anchor: Option<(usize, String)>,
}

/// Everything the session caches for one spine unit.
///
/// One struct in one map, so the facets of a unit travel together:
/// eviction is `units.remove` and cannot forget one. They used to be
/// eight parallel collections keyed by spine index, and every drop site
/// enumerated them by hand — which is how the side tables that "ride
/// along" with a layout once escaped the cache budget entirely.
///
/// Two lifetimes share the struct. Eviction and `release_caches` drop a
/// unit wholesale. A metrics or settings change drops only what layout
/// derives ([`Session::drop_metrics_dependent`]): `links` and `loaded`
/// survive, being locator-space and pixel facts about the *file*, and an
/// image book's `images` survive with them.
#[derive(Default)]
struct UnitState {
    layout: Option<ChapterLayout>,
    /// `ChapterLayout::approx_bytes`, recorded when `layout` is —
    /// `cache_bytes()` runs in the eviction loop, and recomputing it
    /// deep-walked every glyph of every cached chapter.
    layout_bytes: usize,
    images: Option<ImageStore>,
    /// Metrics-independent metadata of a loaded image-book unit (the
    /// pixels are in `images`).
    loaded: Option<LoadedUnit>,
    /// Showing a placeholder page (relaid once the load lands).
    placeholder: bool,
    /// Hyperlinks in locator space, taken at parse time.
    links: Option<Vec<dom::Link>>,
    /// Highlights resolved into the unit's locator space, cached: that
    /// space doesn't move under relayout, so this survives font-size and
    /// theme changes. `Some(empty)` means resolution ran and found
    /// nothing — distinct from never having run.
    #[cfg(feature = "library")]
    resolved_highlights: Option<Vec<Highlight>>,
    /// Last use, in [`Session::use_clock`] ticks, for eviction order.
    used_at: u64,
}

/// The library's handle on the open book.
///
/// `Option<Infallible>` without the `library` feature: always `None`, zero
/// sized, and impossible to construct — so the open path keeps one shape
/// instead of growing a `cfg` at every step that merely passes it along.
#[cfg(feature = "library")]
type OpenedBookId = Option<chapbook_library::BookId>;
#[cfg(not(feature = "library"))]
type OpenedBookId = Option<std::convert::Infallible>;

/// What a session keeps cached when the host does not say.
///
/// Chosen to hold a comfortable working set of comic pages — a 1600x2400
/// page is 15.4 MB decoded, so this is about twelve of them — while being
/// far below what any target would be killed for. It is a backstop, not a
/// recommendation: a phone should pass its own number, and before this
/// existed the answer was "everything, forever", which measured at 676 MB
/// after forty pages of a comic with no way to give any of it back.
pub const DEFAULT_CACHE_BUDGET: usize = 192 * 1024 * 1024;

/// How far one [`Action::FontUp`] or [`Action::FontDown`] moves the base
/// font size, in CSS px.
///
/// The magnitude is the engine's rather than the shell's on purpose: the
/// action carries a direction and nothing else, so every shell steps by
/// the same amount and a reader who changes device finds the same ladder.
/// Two px over the 10–40 range `adjust_font` clamps to is sixteen stops,
/// which is fine-grained enough to land on a comfortable size and coarse
/// enough that reaching either end is a few presses.
pub const FONT_STEP_PX: f32 = 2.0;

/// What a session needs from its host, instead of assuming a desktop.
///
/// Every field here replaces something the session used to reach for on its
/// own — installed fonts, environment variables — and each of those
/// assumptions holds on exactly one of the platforms chapbook targets.
/// Adding to this struct is how the next capability arrives, which is the
/// point of it being a struct: the alternative is changing `open`'s
/// signature once per capability.
pub struct SessionConfig {
    /// Where fonts come from. Required, and required for the reasons in
    /// [`Session::open`].
    pub fonts: FontSource,
    /// Where secrets come from. Defaults to [`NoCredentials`], because the
    /// engine has no business guessing that a machine has an environment,
    /// a Keychain, or anything at all — a shell says. Desktop shells and
    /// the CLI pass `EnvCredentials`; see `chapbook_core::credential`.
    pub credentials: Arc<dyn CredentialStore>,
    /// How bytes are fetched. `None` uses `opds-client`'s bundled `ureq`
    /// transport, which is right for a desktop process and wrong
    /// everywhere else: a host that reaches the network outside
    /// `URLSession` gives up background transfer, the system trust store
    /// and App Transport Security, and a browser has no sockets at all.
    ///
    /// Shared rather than owned because a host has *one* of these — a
    /// single background `URLSession` whose value is that transfers
    /// outlive the process — and the session builds a client per
    /// authentication attempt.
    ///
    /// Present only with the `opds` feature: without it there is nothing
    /// to fetch, and the whole TLS stack is out of the build.
    #[cfg(feature = "opds")]
    pub transport: Option<Arc<dyn HttpClient>>,
    /// Where the library, the managed book copies, the covers and the PSE
    /// page cache live. `None` asks
    /// [`Library::default_dir`](chapbook_library::Library::default_dir),
    /// which knows the convention for each desktop platform and refuses to
    /// guess anywhere else.
    ///
    /// A sandboxed host knows its own answer and nothing else can: Android
    /// hands an app `context.getFilesDir()`, iOS wants
    /// `Library/Application Support`, and a browser has no filesystem at
    /// all. None of those are reachable through an environment variable.
    pub library_dir: Option<std::path::PathBuf>,
    /// How many bytes of laid-out chapters and decoded page images a
    /// session may keep. `None` takes [`DEFAULT_CACHE_BUDGET`].
    ///
    /// A host that knows its own limits should say. Android kills a
    /// process for exceeding them and hands `onTrimMemory` no argument
    /// about it; the number a device can afford is not one the engine can
    /// guess from inside.
    pub cache_budget: Option<usize>,
}

impl SessionConfig {
    pub fn new(fonts: FontSource) -> SessionConfig {
        SessionConfig {
            fonts,
            credentials: Arc::new(NoCredentials),
            #[cfg(feature = "opds")]
            transport: None,
            library_dir: None,
            cache_budget: None,
        }
    }

    pub fn with_credentials(mut self, credentials: Arc<dyn CredentialStore>) -> SessionConfig {
        self.credentials = credentials;
        self
    }

    /// Fetch through the host's networking instead of the bundled `ureq`.
    #[cfg(feature = "opds")]
    pub fn with_transport(mut self, transport: Arc<dyn HttpClient>) -> SessionConfig {
        self.transport = Some(transport);
        self
    }

    /// Keep the library somewhere this host chose.
    pub fn with_library_dir(mut self, dir: impl Into<std::path::PathBuf>) -> SessionConfig {
        self.library_dir = Some(dir.into());
        self
    }

    /// Cap what the session's caches may hold, in bytes.
    pub fn with_cache_budget(mut self, bytes: usize) -> SessionConfig {
        self.cache_budget = Some(bytes);
        self
    }
}

impl std::fmt::Debug for SessionConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut out = f.debug_struct("SessionConfig");
        out.field("fonts", &self.fonts)
            .field("credentials", &"<dyn CredentialStore>")
            .field("library_dir", &self.library_dir)
            .field("cache_budget", &self.cache_budget);
        #[cfg(feature = "opds")]
        out.field(
            "transport",
            match &self.transport {
                Some(_) => &"<dyn HttpClient>",
                None => &"bundled ureq",
            },
        );
        out.finish()
    }
}

impl Session {
    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn kind(&self) -> BookKind {
        self.book.publication().kind()
    }

    pub fn spine(&self) -> usize {
        self.spine
    }

    pub fn spine_len(&self) -> usize {
        self.book.publication().spine().len()
    }

    pub fn page(&self) -> usize {
        self.page
    }

    /// Where the reader is, as the pair that actually identifies a place.
    /// Prefer this to `spine()`/`page()` for "did anything move" — see
    /// [`Position`] for the turn that made the difference matter.
    pub fn position(&self) -> Position {
        Position {
            spine: self.spine,
            page: self.page,
        }
    }

    /// Take everything that has happened since the last call.
    ///
    /// Pull rather than push, and the reason is the type: a `Session` is
    /// `Send` but not `Sync`, and every mutation takes `&mut self`. A
    /// handler invoked from inside those methods could not call back into
    /// the session — a borrow error here, and undefined behaviour across
    /// the C ABI, where a host will try it anyway. A queue has none of
    /// that, and it composes with the wakeup a shell already installs:
    /// [`Session::set_waker`] says *something happened*, this says what.
    ///
    /// A shell with no background loads may never need this. One that has
    /// them should drain on the same tick it calls
    /// [`Session::poll_loaded`].
    ///
    /// # Why half of these are computed here
    ///
    /// [`SessionEvent::UnitLoaded`] and [`SessionEvent::UnitFailed`] are
    /// discrete: they happen once, on the loader thread, and are queued
    /// when they do. The other two are *derived* — a comparison against
    /// what the last drain reported.
    ///
    /// That is deliberate. The position moves at eleven sites across
    /// navigation, restore, link-following and page-count clamping, and
    /// instrumenting all of them means every future site has to remember
    /// to. Deriving it cannot miss one, and it coalesces for free: a shell
    /// that turns ten pages between drains is told where the reader ended
    /// up, which is the only thing it wanted.
    pub fn drain_events(&mut self) -> Vec<SessionEvent> {
        let mut events = std::mem::take(&mut self.events);

        let now = self.position();
        if now != self.reported_position {
            self.reported_position = now;
            events.push(SessionEvent::PositionChanged {
                spine: now.spine,
                page: now.page,
            });
        }

        let finished = self.at_end_of_book();
        if finished && !self.reported_finished {
            events.push(SessionEvent::BookFinished);
        }
        self.reported_finished = finished;

        events
    }

    /// Whether the reader is on the last page of the last unit.
    ///
    /// Reads only layout that is already cached — never lays a unit out to
    /// answer. Draining events must not be the thing that triggers a
    /// pagination pass, and it does not need to be: the reader is looking
    /// at this unit, so it is laid out, and if it somehow is not then they
    /// are not on its last page either.
    fn at_end_of_book(&self) -> bool {
        if self.spine + 1 != self.spine_len() {
            return false;
        }
        match self.layout(self.spine) {
            Some(layout) => !layout.pages.is_empty() && self.page + 1 == layout.pages.len(),
            None => false,
        }
    }

    /// Queue a discrete event, keeping at most one per subject.
    ///
    /// A shell that installs no wakeup and never drains would otherwise
    /// grow this without limit while a comic prefetches its way through a
    /// long book. Replacing rather than appending bounds it at two per
    /// spine entry and loses nothing: two "unit 5 loaded" events say
    /// exactly what one says.
    #[cfg(feature = "_image-book")]
    pub(crate) fn push_event(&mut self, event: SessionEvent) {
        let same_subject = |existing: &SessionEvent| match (existing, &event) {
            (SessionEvent::UnitLoaded { spine: a }, SessionEvent::UnitLoaded { spine: b })
            | (
                SessionEvent::UnitFailed { spine: a, .. },
                SessionEvent::UnitFailed { spine: b, .. },
            ) => a == b,
            _ => false,
        };
        self.events.retain(|existing| !same_subject(existing));
        self.events.push(event);
    }

    pub fn settings(&self) -> &ReadingSettings {
        &self.settings
    }

    /// Page count of the current unit (`0` until metrics are known).
    pub fn page_count(&mut self) -> usize {
        let spine = self.spine;
        self.layout_unit(spine).map_or(0, |l| l.pages.len())
    }

    /// The page geometry in force, or `None` before a shell has set any.
    ///
    /// A shell needs it back to ask [`chapbook_core::TapZones`] what a tap
    /// means: the zones are fractions of a page, and the rotation they
    /// undo is here rather than in the shell's own bookkeeping.
    pub fn metrics(&self) -> Option<PageMetrics> {
        self.metrics
    }

    /// Set page geometry (window size in CSS px + margins + dpi scale).
    /// A change relayouts, keeping the reading position via the char map.
    pub fn set_metrics(&mut self, metrics: PageMetrics) {
        if self.metrics == Some(metrics) {
            return;
        }
        // A turn alone changes the panel, not the layout: keep the cached
        // pages and just repaint through the new orientation.
        if self
            .metrics
            .is_some_and(|current| current.same_layout(&metrics))
        {
            self.metrics = Some(metrics);
            self.reclamp_view();
            self.mark(FrameIntent::Relayout);
            return;
        }
        let had = self.metrics.is_some();
        let locator = self.current_offset();
        self.metrics = Some(metrics);
        self.drop_metrics_dependent();
        if had {
            let spine = self.spine;
            if let Some(layout) = self.layout_unit(spine) {
                self.page = layout.page_of(locator);
            }
        }
        // The zoom survives a resize, so its pan has to be brought back
        // inside a page box that may have shrunk under it.
        self.reclamp_view();
        self.mark(FrameIntent::Relayout);
    }

    /// What the font source produced: how many faces loaded, and any CSS
    /// generic that resolved to a family nothing carries.
    ///
    /// Worth printing once at startup on a platform you have not run on.
    /// Every font failure this API exists to prevent is silent — a page
    /// still lays out, still renders, still conforms — so this is the only
    /// cheap way to tell a correctly configured device from a broken one
    /// without looking at pixels.
    pub fn font_report(&self) -> &FontReport {
        &self.font_report
    }

    /// Every font family the session can match, deduplicated and sorted.
    ///
    /// The read-back half of [`FontSource`], and what a font-family picker
    /// needs: a shell cannot offer a choice it cannot enumerate. Includes
    /// families registered from a book's own `@font-face` rules once that
    /// unit has laid out, so the list grows as chapters load.
    pub fn font_families(&self) -> Vec<String> {
        let mut families: Vec<String> = self
            .fonts
            .db()
            .faces()
            .filter_map(|face| face.families.first().map(|(name, _)| name.clone()))
            .collect();
        // The embedded math face is a rendering resource, not a reading
        // typeface: a family picker must not offer it.
        #[cfg(feature = "mathml")]
        families.retain(|name| name != chapbook_layout::MATH_FONT_FAMILY);
        families.sort_unstable();
        families.dedup();
        families
    }

    /// The image store backing the current unit's `Image` ops. Empty for
    /// units that carry no images.
    pub fn image_store(&self) -> &ImageStore {
        self.unit_images(self.spine)
    }

    /// What a display list's ops resolve against: the font database its
    /// `GlyphRun`s name faces in — glyphs are shaped already, so a backend
    /// only rasterizes them — and the image store its `Image` ops key
    /// into. A shell rasterizing for itself needs both, and they are
    /// disjoint fields, so they come back together.
    pub fn paint_resources(&mut self) -> (&mut cosmic_text::FontSystem, &ImageStore) {
        // Field accesses rather than `unit_images`: the split borrow
        // (fonts mutably, images not) needs the compiler to see disjoint
        // fields, which a method call would hide.
        let images = self
            .units
            .get(&self.spine)
            .and_then(|unit| unit.images.as_ref())
            .unwrap_or(&self.empty_images);
        (&mut self.fonts, images)
    }

    // ---- Persistence ----

    /// Locator offset of the current page (0 for comics).
    pub fn current_offset(&self) -> u32 {
        self.layout(self.spine)
            .and_then(|l| l.char_map.get(self.page).copied())
            .unwrap_or(0)
    }

    /// The library record this session is reading, once it has one.
    ///
    /// The join between opening a book and everything the library knows
    /// about it — most immediately
    /// [`set_sync_targets`](chapbook_library::Library::set_sync_targets),
    /// which a shell that downloaded from a catalog has to call with the
    /// entry's links: the session imported the book, so only it knows
    /// which row that became.
    ///
    /// `None` for a book that never reached the library — an OPDS page
    /// stream, or a session built without one.
    #[cfg(feature = "library")]
    pub fn book_id(&self) -> Option<chapbook_library::BookId> {
        self.book_id
    }

    /// Capture the position as a full layered locator and persist it.
    /// Comics persist page-unit progression (see `chapbook_core::locator`).
    pub fn save_position(&mut self) {
        #[cfg(feature = "library")]
        {
            let Some(id) = self.book_id else {
                return;
            };
            let offset = self.current_offset();
            let Ok(item) = self.book.publication().spine_item(self.spine) else {
                return;
            };
            let href = item.href.clone();
            let locator = match self.book.publication().kind() {
                BookKind::Epub => {
                    let ctx = self.unit_char_context();
                    LayeredLocator::capture(
                        &href, self.spine, &ctx.text, offset, ctx.prior, ctx.total,
                    )
                }
                // Image books: the progression unit is pages.
                BookKind::Comic | BookKind::Pdf => LayeredLocator::capture(
                    &href,
                    self.spine,
                    "",
                    0,
                    self.spine as u64,
                    self.book.publication().spine().len() as u64,
                ),
            };
            // Computed before the library is borrowed, and only ever
            // set: reaching the end is a thing that happened, so leaving
            // the last page does not un-happen it. Clearing is the
            // reader's own call, through
            // [`Library::set_finished`](chapbook_library::Library::set_finished).
            //
            // Here rather than beside `SessionEvent::BookFinished`,
            // because that event is only observed by a shell that drains
            // — and whether a book was finished is not a fact a shell
            // should have to opt into recording. `save_position` is the
            // call every shell already makes.
            let finished = self.at_end_of_book();
            let Some(library) = self.library_mut() else {
                return;
            };
            if let Err(e) = library.set_position(id, &locator) {
                log::error!("failed to save position: {e}");
            }
            if finished {
                if let Err(e) = library.set_finished(id, true) {
                    log::error!("failed to mark the book finished: {e}");
                }
            }
        }
    }
}
