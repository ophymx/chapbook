//! The cascade driver: owns stylo's `Stylist` and media `Device`, ships the
//! UA stylesheet, and runs the style traversal over a parsed document.

use std::sync::Once;

use euclid::{Scale, Size2D};
use style::animation::DocumentAnimationSet;
use style::context::{
    QuirksMode, RegisteredSpeculativePainter, RegisteredSpeculativePainters, SharedStyleContext,
    StyleContext,
};
use style::device::Device;
use style::dom::{TElement, TNode};
use style::global_style_data::GLOBAL_STYLE_DATA;
use style::media_queries::{MediaList, MediaType};
use style::properties::style_structs::Font;
use style::properties::ComputedValues;
use style::queries::values::PrefersColorScheme;
use style::selector_parser::SnapshotMap;
use style::servo::media_features::PointerCapabilities;
use style::servo_arc::Arc as ServoArc;
use style::shared_lock::{SharedRwLock, StylesheetGuards};
use style::stylesheets::{AllowImportRules, DocumentStyleSheet, Origin, Stylesheet, UrlExtraData};
use style::stylist::Stylist;
use style::thread_state::{self, ThreadState};
use style::traversal::{recalc_style_at, DomTraversal, PerLevelTraversalData};
use style::traversal_flags::TraversalFlags;
use style::Atom;

use crate::dom::Document;
use chapbook_core::{PageMetrics, ReadingSettings};

use super::fonts::BookFontMetricsProvider;

/// The embedded UA stylesheet — the EPUB 3 CSS profile boundary.
pub const UA_CSS: &str = include_str!("ua.css");

static INIT_PREFS: Once = Once::new();

/// Owns the stylist and drives stylo's cascade over chapter documents.
///
/// One engine outlives many documents: UA and user sheets persist; author
/// sheets are swapped per chapter via [`StyleEngine::set_author_sheets`].
pub struct StyleEngine {
    stylist: Stylist,
    guard: SharedRwLock,
    snapshots: SnapshotMap,
    author_sheets: Vec<DocumentStyleSheet>,
    ua_url_data: UrlExtraData,
}

impl StyleEngine {
    pub fn new(page: &PageMetrics, settings: &ReadingSettings) -> Self {
        INIT_PREFS.call_once(|| {
            // Tolerate properties stylo knows but servo-mode layout doesn't
            // implement, instead of panicking on them.
            style_config::set_pref!("layout.unimplemented", true);
            // Chapbook styles sequentially; keep stylo's pool unspawned.
            style_config::set_pref!("layout.threads", -1);
        });

        let guard = SharedRwLock::new();
        let ua_url_data = UrlExtraData(ServoArc::new(
            url::Url::parse("chapbook:///ua.css").unwrap(),
        ));

        let device = Device::new(
            MediaType::screen(),
            selectors::matching::QuirksMode::NoQuirks,
            Size2D::new(page.size.w, page.size.h),
            Size2D::new(page.size.w * page.dpi_scale, page.size.h * page.dpi_scale),
            Scale::new(page.dpi_scale),
            Box::new(BookFontMetricsProvider),
            ComputedValues::initial_values_with_font_override(Font::initial_values()),
            if settings.theme.is_dark() {
                PrefersColorScheme::Dark
            } else {
                PrefersColorScheme::Light
            },
            PointerCapabilities::default(),
            PointerCapabilities::default(),
        );

        let mut engine = StyleEngine {
            stylist: Stylist::new(device, QuirksMode::NoQuirks),
            guard,
            snapshots: SnapshotMap::new(),
            author_sheets: Vec::new(),
            ua_url_data,
        };

        engine.append_sheet(UA_CSS, Origin::UserAgent);
        engine.append_sheet(&settings_css(settings), Origin::UserAgent);
        if let Some(css) = theme_css(settings.theme) {
            // User origin: beats the UA defaults; whether it also beats
            // publisher declarations is per-theme (see `theme_css`).
            engine.append_sheet(&css, Origin::User);
        }
        if let Some(css) = font_family_css(settings) {
            engine.append_sheet(&css, Origin::User);
        }
        engine
    }

    /// Add a user-origin override sheet (reader themes and preferences that
    /// should beat publisher styles only via `!important`, per the cascade).
    pub fn add_user_sheet(&mut self, css: &str) {
        self.append_sheet(css, Origin::User);
    }

    /// Replace the author-origin sheets with this chapter's, in document
    /// order. Pass no sources (or call with an empty slice) for
    /// publisher-styles-off reading.
    pub fn set_author_sheets(&mut self, css_sources: &[String]) {
        for old in self.author_sheets.drain(..) {
            self.stylist.remove_stylesheet(old, &self.guard.read());
        }
        for css in css_sources {
            let sheet = self.make_sheet(css, Origin::Author);
            self.stylist
                .append_stylesheet(sheet.clone(), &self.guard.read());
            self.author_sheets.push(sheet);
        }
    }

    /// Run the cascade: every element in `doc` ends up with its
    /// `ComputedValues`, readable via `Document::primary_styles` (which it
    /// reaches through its `Deref` to `DocumentInner`).
    pub fn style_document(&mut self, doc: &mut Document) {
        // All Locked<T> values (sheets, style attributes) must belong to the
        // lock our traversal guards come from.
        doc.attach_style_context(self.guard.clone());

        let Some(root_element) = doc.style_root_element() else {
            return; // no <html>: nothing to style
        };

        thread_state::enter(ThreadState::LAYOUT);

        {
            let guard = self.guard.read();
            let guards = StylesheetGuards {
                author: &guard,
                ua_or_user: &guard,
            };

            self.stylist
                .flush(&guards)
                .process_style(root_element, Some(&self.snapshots));

            let context = SharedStyleContext {
                traversal_flags: TraversalFlags::empty(),
                stylist: &self.stylist,
                options: GLOBAL_STYLE_DATA.options.clone(),
                guards,
                visited_styles_enabled: false,
                animations: DocumentAnimationSet::default(),
                current_time_for_animations: 0.0,
                snapshot_map: &self.snapshots,
                registered_speculative_painters: &NoPainters,
            };

            let token = RecalcStyle::pre_traverse(root_element, &context);
            if token.should_traverse() {
                let traverser = RecalcStyle { context };
                // Sequential: no rayon pool.
                style::driver::traverse_dom(&traverser, token, None);
            }
        }

        self.stylist.rule_tree().maybe_gc();
        thread_state::exit(ThreadState::LAYOUT);
    }

    fn append_sheet(&mut self, css: &str, origin: Origin) {
        let sheet = self.make_sheet(css, origin);
        self.stylist.append_stylesheet(sheet, &self.guard.read());
    }

    fn make_sheet(&self, css: &str, origin: Origin) -> DocumentStyleSheet {
        let data = Stylesheet::from_str(
            css,
            self.ua_url_data.clone(),
            origin,
            ServoArc::new(self.guard.wrap(MediaList::empty())),
            self.guard.clone(),
            None, // no loader: @import rules are not fetched (documented gap)
            None,
            QuirksMode::NoQuirks,
            AllowImportRules::Yes,
        );
        DocumentStyleSheet(ServoArc::new(data))
    }
}

/// The theme's user-origin sheet. `Light` is the identity theme and
/// injects nothing, so an unthemed pipeline is byte-identical to the
/// pre-theme one. `Sepia` is gentle: it recolors the *defaults* (normal
/// user-origin declarations, so publisher colors win where specified).
/// `Dark` forces: publisher text/background colors are overridden with
/// `!important` — a light-on-light aside is unreadable in night mode, and
/// readability beats design there (the Readium/Calibre convention).
fn theme_css(theme: chapbook_core::Theme) -> Option<String> {
    use chapbook_core::Theme;
    let hex = |c: chapbook_core::Rgba| format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b);
    let (fg, link) = (hex(theme.foreground()), hex(theme.link()));
    match theme {
        Theme::Light => None,
        Theme::Sepia => Some(format!(
            ":root {{ color: {fg}; }}\na {{ color: {link}; }}\n"
        )),
        // Every background but an image's: for a picture the ground is
        // part of the picture. Line art shipped black on transparent is
        // given a white ground by its book precisely so night mode does
        // not lose it, and Standard Ebooks paints its realistic plates
        // on `currentColor` in the dark for the same reason — a rule the
        // blanket strip used to override, leaving black lines on black.
        Theme::Dark => Some(format!(
            "* {{ color: {fg} !important; }}\n\
             *:not(img) {{ background-color: transparent !important; }}\n\
             a {{ color: {link} !important; }}\n"
        )),
    }
}

/// The reader-settings sheet, appended after the UA sheet at the same
/// (user-agent) origin so publisher styles still win where they specify.
fn settings_css(settings: &ReadingSettings) -> String {
    let mut css = format!(
        "html {{ font-size: {}px; line-height: {}; }}\n",
        settings.base_font_px, settings.line_height
    );
    if settings.justify {
        css.push_str("body { text-align: justify; }\n");
    }
    css
}

/// The reader's chosen typeface, as a user-origin sheet.
///
/// User origin *and* `!important`, which together are the only way to beat
/// an author declaration — and beating it is the point. Nearly every real
/// EPUB sets `body { font-family }`, so the polite version of this rule
/// would do nothing on nearly every book, which is worse than not offering
/// the setting. This is the same instrument [`theme_css`] uses for Dark
/// and for the same reason.
///
/// `*` and not `:root`, which is the trap. `!important` at user origin
/// beats an author declaration *for the same element and property* — it
/// does not stop the author styling a different element further down. A
/// rule on `:root` sets `html`, and then `body { font-family }` — which is
/// where publishers actually put it — wins on `body` and inherits from
/// there, so the reader's choice would lose on almost every real book
/// while passing any test whose fixture styled `html`. Same instrument
/// [`theme_css`] reaches for, and the same reason.
///
/// Monospace is exempt, descendants included: `pre *` and friends carry
/// one type selector where `*` carries none, so they win on specificity
/// whatever the order. Without the descendant half, a `<span>` inside a
/// `<pre>` would take the reader's serif and the listing would come apart
/// mid-line.
fn font_family_css(settings: &ReadingSettings) -> Option<String> {
    let family = settings.font_family.as_deref()?.trim();
    if family.is_empty() {
        return None;
    }
    // A family name is author-controlled data reaching a parser: a name
    // carrying a brace or a semicolon would otherwise close this rule and
    // open whatever came next. Quoting and escaping per CSS string rules
    // keeps it one value.
    let quoted = format!("\"{}\"", family.replace('\\', "\\\\").replace('"', "\\\""));
    Some(format!(
        "* {{ font-family: {quoted} !important; }}\n\
         pre, pre *, code, code *, kbd, kbd *, samp, samp *, tt, tt * \
         {{ font-family: monospace !important; }}\n"
    ))
}

struct NoPainters;

impl RegisteredSpeculativePainters for NoPainters {
    fn get(&self, _name: &Atom) -> Option<&dyn RegisteredSpeculativePainter> {
        None
    }
}

struct RecalcStyle<'a> {
    context: SharedStyleContext<'a>,
}

impl<E: TElement> DomTraversal<E> for RecalcStyle<'_> {
    fn process_preorder<F: FnMut(E::ConcreteNode)>(
        &self,
        traversal_data: &PerLevelTraversalData,
        context: &mut StyleContext<E>,
        node: E::ConcreteNode,
        note_child: F,
    ) {
        if let Some(el) = node.as_element() {
            // SAFETY: the traversal grants exclusive access to `el`.
            let mut data = unsafe { el.ensure_data() };
            recalc_style_at(self, traversal_data, context, el, &mut data, note_child);
            unsafe { el.unset_dirty_descendants() }
        }
    }

    #[inline]
    fn needs_postorder_traversal() -> bool {
        false
    }

    fn process_postorder(&self, _context: &mut StyleContext<E>, _node: E::ConcreteNode) {
        unreachable!("needs_postorder_traversal is false")
    }

    #[inline]
    fn shared_context(&self) -> &SharedStyleContext<'_> {
        &self.context
    }
}

/// Convenience accessor mirroring `Document::primary_styles`, so consumers
/// only need this crate in scope.
pub fn computed(doc: &Document, id: crate::dom::NodeId) -> Option<ServoArc<ComputedValues>> {
    doc.primary_styles(id)
}
