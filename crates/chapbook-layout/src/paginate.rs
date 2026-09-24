//! The streaming page cursor: block flow + cosmic-text inline layout +
//! CSS-fragmentation page breaking.
//!
//! Break rules honored in v1: forced `break-before/after: page` (+ legacy
//! aliases), `break-inside: avoid` (retry on a fresh page, else break
//! anyway), `break-before/after: avoid` (keep-with-next: the anchored
//! block migrates with its successor's first content), widows/orphans
//! (default 2/2), margins collapsed between siblings and discarded at page
//! boundaries. A line taller than a page is placed alone and may overflow
//! (clipped at paint).
//!
//! Floats, v1 scope: `float: left/right` on images (intrinsic size) and on
//! non-replaced blocks with an explicit CSS width (laid out in a detached
//! sub-paginator and translated into place). The float is placed against
//! the content edge without advancing the flow, and line boxes of
//! following inline content shorten beside it — a line sits beside the
//! float only when it fits entirely above the float's bottom margin edge;
//! first-line indents apply beside floats too. `clear` works on any block.
//! Floats never cross a page boundary: bands are dropped at every page
//! break, and a float taller than a page truncates. Shrink-to-fit floated
//! blocks and floats nested in floats stay in normal flow; images and
//! tables ignore float bands (documented degradations).
//!
//! Hyphenation: `hyphens: auto` content arrives with soft hyphens already
//! inserted (see `crate::hyphenate`); cosmic-text breaks after them and
//! renders them zero-width. Any line ending at a soft hyphen gets a visible
//! hyphen glyph appended here, with justified lines re-tightened over their
//! spaces so the measure holds.

use app_units::Au;
use cosmic_text::{Buffer, FontSystem, Metrics, Shaping, Wrap};
use style::properties::ComputedValues;
use style::values::computed::LengthPercentage;

use chapbook_core::{PageMetrics, Point, Rect, Rgba, Rotation, Size};
use chapbook_paint::{
    BoxDecoration, Decoration, Fragment, FragmentKind, Glyph, GlyphRun, ImageFilter, ImageStore,
    LineFragment, Page,
};

use crate::boxtree::{BlockBox, BlockKind, InlineContent};
use crate::fragmentation::BreakRule;
use crate::style_to_attrs::{align_for, attrs_for, font_size_px, line_height_px, text_color};

/// One shaped visual line, ready to be placed on a page.
#[derive(Clone)]
struct ShapedLine {
    height: f32,
    baseline: f32,
    width: f32,
    /// Alignment-induced left offset of the line box within its block.
    x_indent: f32,
    runs: Vec<GlyphRun>,
    decorations: Vec<Decoration>,
    text: String,
    locator_start: u32,
    /// Buffer line index and end byte of the last glyph within that line's
    /// text — the split point the text-indent two-pass shaping needs.
    line_index: usize,
    byte_end: usize,
    /// (run index, glyph index) of every space glyph, for manual
    /// justification of split-off first lines.
    spaces: Vec<(usize, usize)>,
}

pub(crate) struct Paginator<'f> {
    fonts: &'f mut FontSystem,
    page: PageMetrics,
    content: Rect,
    pages: Vec<Page>,
    /// Cursor within the current page's content box.
    y: f32,
    /// Fragments placed on the current page.
    placed_on_page: usize,
    /// Collapsed margin waiting to be committed before the next content.
    pending_margin: f32,
    /// A forced break-after is pending: next content starts a new page.
    force_break: bool,
    /// Where the previous block's fragments start, for keep-with-next.
    last_anchor: Option<KeepAnchor>,
    /// The previous block declared `break-after: avoid`.
    last_avoid_after: bool,
    /// Armed keep: if this block's first content forces a page break, the
    /// anchored fragments migrate with it.
    active_keep: Option<KeepAnchor>,
    /// Per page: smallest locator offset placed on it (u32::MAX = none yet).
    page_locators: Vec<u32>,
    /// Active float exclusion per side, page-local (cleared at page breaks).
    float_left: Option<FloatBand>,
    float_right: Option<FloatBand>,
    /// Hyphen glyph (id, advance per em) per font, for visible hyphens at
    /// soft-hyphen line breaks. `None` = font has no '-' glyph.
    hyphen_cache: std::collections::HashMap<cosmic_text::fontdb::ID, Option<(u16, f32)>>,
    /// True in the detached sub-paginator that lays out a float's content:
    /// suppresses float interception (floats never nest).
    in_float: bool,
    /// Images whose element carries a `filter`, to compose into derived
    /// images once the store can be borrowed for writing (`resolve_filters`).
    pending_filters: Vec<PendingFilter>,
}

/// An image fragment waiting for its filtered copy: the element's tag (also
/// its source resource id), the background CSS would paint under it, and the
/// filter functions in order.
pub(crate) struct PendingFilter {
    pub tag: u64,
    pub background: Option<Rgba>,
    pub filters: Vec<ImageFilter>,
}

/// Point every filtered image fragment at its derived copy. Runs after
/// pagination, when the box tree that read the store for sizes is gone and
/// the store can be written. A source the store does not hold stays as it
/// was — a missing image draws nothing either way.
pub(crate) fn resolve_filters(
    pages: &mut [Page],
    images: &mut ImageStore,
    pending: Vec<PendingFilter>,
) {
    for p in pending {
        let Some(id) = images.derive(p.tag, p.background, &p.filters) else {
            continue;
        };
        for fragment in pages.iter_mut().flat_map(|page| page.fragments.iter_mut()) {
            if fragment.tag == p.tag {
                if let FragmentKind::Image { resource } = &mut fragment.kind {
                    *resource = id;
                }
            }
        }
    }
}

/// The element's `filter`, reduced to the functions a pixel can answer for
/// itself. `blur()`, `drop-shadow()` and `url()` are dropped: a list of
/// only those is empty, and the image paints as if unfiltered.
fn image_filters(style: &ComputedValues) -> Vec<ImageFilter> {
    use style::values::computed::Filter;
    let amount = |v: f32| (v.max(0.0) * 1000.0).round().min(f32::from(u16::MAX)) as u16;
    style
        .get_effects()
        .filter
        .0
        .iter()
        .filter_map(|f| match f {
            Filter::Brightness(v) => Some(ImageFilter::Brightness(amount(v.0))),
            Filter::Contrast(v) => Some(ImageFilter::Contrast(amount(v.0))),
            Filter::Grayscale(v) => Some(ImageFilter::Grayscale(amount(v.0))),
            Filter::HueRotate(a) => Some(ImageFilter::HueRotate(
                a.degrees().rem_euclid(360.0).round() as i16,
            )),
            Filter::Invert(v) => Some(ImageFilter::Invert(amount(v.0))),
            Filter::Opacity(v) => Some(ImageFilter::Opacity(amount(v.0))),
            Filter::Saturate(v) => Some(ImageFilter::Saturate(amount(v.0))),
            Filter::Sepia(v) => Some(ImageFilter::Sepia(amount(v.0))),
            Filter::Blur(_) | Filter::DropShadow(_) | Filter::Url(_) => {
                log::debug!("filter function without a pixel-local answer, skipped");
                None
            }
        })
        .collect()
}

#[derive(Clone, Copy)]
struct KeepAnchor {
    page: usize,
    frag_start: usize,
    /// Content-relative y where the anchored block begins.
    y_start: f32,
}

/// One side's float exclusion: `width` (margin box) insets line boxes until
/// the flow cursor passes `y_end` (content-relative bottom margin edge).
#[derive(Clone, Copy)]
struct FloatBand {
    width: f32,
    y_end: f32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FloatSide {
    Left,
    Right,
}

/// `float` of a block, mapped to physical sides (horizontal-tb ltr books:
/// inline-start = left).
fn float_side(style: &ComputedValues) -> Option<FloatSide> {
    use style::values::computed::Float;
    match style.get_box().float {
        Float::Left | Float::InlineStart => Some(FloatSide::Left),
        Float::Right | Float::InlineEnd => Some(FloatSide::Right),
        Float::None => None,
    }
}

/// `clear` of a block as (clears-left, clears-right).
fn clear_sides(style: &ComputedValues) -> (bool, bool) {
    use style::values::computed::Clear;
    match style.get_box().clear {
        Clear::None => (false, false),
        Clear::Left | Clear::InlineStart => (true, false),
        Clear::Right | Clear::InlineEnd => (false, true),
        Clear::Both => (true, true),
    }
}

impl<'f> Paginator<'f> {
    pub fn new(fonts: &'f mut FontSystem, page: PageMetrics) -> Self {
        let content = Rect::new(
            page.margins.left,
            page.margins.top,
            page.content_width(),
            page.content_height(),
        );
        let mut p = Paginator {
            fonts,
            page,
            content,
            pages: Vec::new(),
            y: 0.0,
            placed_on_page: 0,
            pending_margin: 0.0,
            force_break: false,
            last_anchor: None,
            last_avoid_after: false,
            active_keep: None,
            page_locators: Vec::new(),
            float_left: None,
            float_right: None,
            hyphen_cache: std::collections::HashMap::new(),
            in_float: false,
            pending_filters: Vec::new(),
        };
        p.new_page();
        p
    }

    pub fn finish(mut self) -> (Vec<Page>, Vec<u32>, Vec<PendingFilter>) {
        // Backfill locator starts for pages that carried no text.
        let mut last = 0u32;
        for loc in &mut self.page_locators {
            if *loc == u32::MAX {
                *loc = last;
            } else {
                last = *loc;
            }
        }
        (self.pages, self.page_locators, self.pending_filters)
    }

    fn new_page(&mut self) {
        self.pages.push(Page {
            size: self.page.size,
            content: self.content,
            fragments: Vec::new(),
        });
        self.page_locators.push(u32::MAX);
        self.y = 0.0;
        self.placed_on_page = 0;
        // Margins are discarded at fragmentainer boundaries.
        self.pending_margin = 0.0;
        // Floats never cross a page boundary.
        self.float_left = None;
        self.float_right = None;
    }

    fn at_page_top(&self) -> bool {
        self.placed_on_page == 0
    }

    fn remaining(&self) -> f32 {
        self.content.size.h - self.y
    }

    /// Lay out one block box into the page stream.
    ///
    /// `x` is the left inset of the block's content box within the page
    /// content box; `width` its content width.
    pub fn place_block(&mut self, block: &BlockBox, x: f32, width: f32) {
        let style = &block.style;
        // A float leaves the flow entirely: it is placed against a content
        // edge and registers an exclusion band instead of advancing the
        // cursor. Images use their intrinsic size; non-replaced blocks need
        // an explicit CSS width (shrink-to-fit floats stay in flow — the
        // documented v1 degradation). Floats inside floats also stay in
        // flow.
        if !block.anonymous && !self.in_float {
            if let Some(side) = float_side(style) {
                match &block.kind {
                    BlockKind::Image {
                        width: iw,
                        height: ih,
                    } => {
                        self.place_float_image(block, *iw, *ih, x, width, side);
                        return;
                    }
                    BlockKind::Container(_) | BlockKind::Inline(_) => {
                        if let Some(w) = explicit_width_px(style, width) {
                            self.place_float_block(block, x, width, w, side);
                            return;
                        }
                    }
                    _ => {}
                }
            }
        }
        let cw = width;

        let (margin_top, margin_bottom, margin_left, margin_right) = if block.anonymous {
            (0.0, 0.0, 0.0, 0.0)
        } else {
            let m = style.get_margin();
            (
                resolve_margin(&m.margin_top, cw),
                resolve_margin(&m.margin_bottom, cw),
                resolve_margin(&m.margin_left, cw),
                resolve_margin(&m.margin_right, cw),
            )
        };
        let (pad_top, pad_bottom, pad_left, pad_right) = if block.anonymous {
            (0.0, 0.0, 0.0, 0.0)
        } else {
            let p = style.get_padding();
            (
                resolve_padding(&p.padding_top, cw),
                resolve_padding(&p.padding_bottom, cw),
                resolve_padding(&p.padding_left, cw),
                resolve_padding(&p.padding_right, cw),
            )
        };
        // An image's ground is its own box, not a band across the measure:
        // `push_image` paints it at the image's rect — or, under a
        // `filter`, composes it into the derived image, since CSS filters
        // the whole element, ground included.
        let decoration = if block.anonymous || matches!(block.kind, BlockKind::Image { .. }) {
            None
        } else {
            box_decoration_of(style)
        };
        let border = decoration
            .as_ref()
            .map(|d| d.border_widths)
            .unwrap_or_default();

        if !block.anonymous {
            if block.frag.break_before == BreakRule::Page && !self.at_page_top() {
                self.new_page();
            }
            if self.force_break && !self.at_page_top() {
                self.new_page();
            }
            self.force_break = false;
            let (clear_left, clear_right) = clear_sides(style);
            if clear_left || clear_right {
                self.apply_clear(clear_left, clear_right);
            }
            // Arm keep-with-next when the previous block asked to stay with
            // us (`break-after: avoid`) or we ask to stay with it
            // (`break-before: avoid`).
            self.active_keep =
                if self.last_avoid_after || block.frag.break_before == BreakRule::Avoid {
                    self.last_anchor
                } else {
                    None
                };
        }

        self.pending_margin = self.pending_margin.max(margin_top);
        let block_start = (
            self.pages.len() - 1,
            self.pages.last().unwrap().fragments.len(),
        );

        let inner_x = x + margin_left + border.left + pad_left;
        let inner_w = (width
            - margin_left
            - margin_right
            - border.left
            - border.right
            - pad_left
            - pad_right)
            .max(1.0);

        // A decorated box pins its top edge here: commit margins and record
        // where the box begins so slices can be emitted per page.
        let span_start = if decoration.is_some() {
            self.commit_margin();
            Some((
                self.pages.len() - 1,
                self.pages.last().unwrap().fragments.len(),
                self.y,
            ))
        } else {
            None
        };
        if border.top > 0.0 {
            self.commit_margin();
            self.y += border.top;
        }
        if pad_top > 0.0 {
            self.commit_margin();
            self.y += pad_top;
        }

        match &block.kind {
            BlockKind::Container(children) => {
                for child in children {
                    self.place_block(child, inner_x, inner_w);
                }
            }
            BlockKind::Inline(inline) => {
                // Anonymous boxes carry the indent only when they are their
                // container's first in-flow content (text-indent inherits,
                // so the parent's value is on the borrowed style).
                let indent = if block.anonymous && !block.indent_first {
                    0.0
                } else {
                    text_indent_px(style, inner_w)
                };
                let tag = crate::dom::node_tag(block.node);
                let y_flow = self.y
                    + if self.at_page_top() {
                        0.0
                    } else {
                        self.pending_margin
                    };
                let band_active = [self.float_left, self.float_right]
                    .iter()
                    .any(|b| b.is_some_and(|b| b.y_end > y_flow + 0.01));
                if band_active {
                    self.place_inline_with_floats(inline, block, inner_x, inner_w, indent, tag);
                } else {
                    let lines = if indent > 0.5 {
                        self.shape_inline_indented(inline, style, inner_w, indent)
                    } else {
                        self.shape_inline(inline, style, inner_w)
                    };
                    self.place_lines(lines, block, inner_x, tag);
                }
            }
            BlockKind::Image { width, height } => {
                self.place_image(block, *width, *height, inner_x, inner_w);
            }
            #[cfg(feature = "mathml")]
            BlockKind::Math(math) => {
                self.place_math(block, math, inner_x, inner_w);
            }
            BlockKind::Table(table) => {
                self.place_table(block, table, inner_x, inner_w);
            }
            BlockKind::Rule => {
                self.commit_margin();
                let color = text_color(style);
                let rect = Rect {
                    origin: Point::new(
                        self.content.origin.x + inner_x,
                        self.content.origin.y + self.y,
                    ),
                    size: Size::new(inner_w, 1.0),
                };
                self.pages.last_mut().unwrap().fragments.push(Fragment {
                    rect,
                    kind: FragmentKind::Rule { color },
                    tag: crate::dom::node_tag(block.node),
                });
                self.y += 1.0;
                self.placed_on_page += 1;
            }
        }

        if pad_bottom > 0.0 {
            self.y += pad_bottom;
        }
        if border.bottom > 0.0 {
            self.y += border.bottom;
        }
        if let (Some(decoration), Some((start_page, start_index, start_y))) =
            (decoration, span_start)
        {
            self.emit_box_slices(
                decoration,
                start_page,
                start_index,
                start_y,
                x + margin_left,
                (width - margin_left - margin_right).max(1.0),
                crate::dom::node_tag(block.node),
            );
        }
        self.pending_margin = self.pending_margin.max(margin_bottom);
        if !block.anonymous {
            if block.frag.break_after == BreakRule::Page {
                self.force_break = true;
            }
            // Record this block as a keep anchor for its successor: only
            // when it sits entirely on the current page and is short enough
            // to migrate (a heading, not a chapter).
            self.last_avoid_after = block.frag.break_after == BreakRule::Avoid;
            let (start_page, frag_start) = block_start;
            let current = self.pages.len() - 1;
            self.last_anchor = (start_page == current
                && frag_start > 0
                && frag_start < self.pages[current].fragments.len())
            .then(|| {
                let y_start =
                    self.pages[current].fragments[frag_start].rect.origin.y - self.content.origin.y;
                KeepAnchor {
                    page: current,
                    frag_start,
                    y_start,
                }
            })
            .filter(|a| (self.y - a.y_start) <= self.content.size.h / 3.0);
            self.active_keep = None;
        }
    }

    /// Break to a new page; when a keep-with-next anchor is armed, migrate
    /// the anchored trailing fragments (the kept heading) onto it.
    fn new_page_keeping(&mut self) {
        let keep = self.active_keep.take();
        let old_page = self.pages.len() - 1;
        let old_y = self.y;
        self.new_page();
        let Some(anchor) = keep else { return };
        if anchor.page != old_page {
            return;
        }
        let moved: Vec<Fragment> = self.pages[old_page].fragments.split_off(anchor.frag_start);
        if moved.is_empty() {
            return;
        }
        let count = moved.len();
        for mut fragment in moved {
            fragment.rect.origin.y -= anchor.y_start;
            self.pages.last_mut().unwrap().fragments.push(fragment);
        }
        self.placed_on_page = count;
        // Cursor resumes where the block would have started, relative to
        // the migrated anchor (its height plus any committed margins).
        self.y = old_y - anchor.y_start;
    }

    /// Insert one background/border slice per page the box touched, under
    /// the content fragments that were placed meanwhile.
    #[allow(clippy::too_many_arguments)]
    fn emit_box_slices(
        &mut self,
        decoration: BoxDecoration,
        start_page: usize,
        start_index: usize,
        start_y: f32,
        x: f32,
        border_box_w: f32,
        tag: u64,
    ) {
        let end_page = self.pages.len() - 1;
        let end_y = self.y;
        for page_idx in start_page..=end_page {
            let (slice_top, slice_bottom) = (
                if page_idx == start_page { start_y } else { 0.0 },
                if page_idx == end_page {
                    end_y
                } else {
                    self.content.size.h
                },
            );
            if slice_bottom - slice_top <= 0.0 {
                continue;
            }
            let mut slice = decoration.clone();
            slice.first_slice = page_idx == start_page;
            slice.last_slice = page_idx == end_page;
            let fragment = Fragment {
                rect: Rect {
                    origin: Point::new(
                        self.content.origin.x + x,
                        self.content.origin.y + slice_top,
                    ),
                    size: Size::new(border_box_w, slice_bottom - slice_top),
                },
                kind: FragmentKind::Box(slice),
                tag,
            };
            let insert_at = if page_idx == start_page {
                start_index
            } else {
                0
            };
            self.pages[page_idx].fragments.insert(insert_at, fragment);
        }
    }

    /// Emit an image fragment with what its element's style adds. A
    /// background (or border) is a box decoration at the image's own rect
    /// — the element's border box, which is the image — rather than the
    /// full-measure band `place_block` gives a block. Under a `filter` the
    /// background travels *with* the filter instead, into a derived image
    /// composed after pagination (`resolve_filters`), because CSS filters
    /// the whole element, ground included; the fragment is then pointed at
    /// that copy, and a border is not painted under it.
    fn push_image(&mut self, block: &BlockBox, rect: Rect) {
        let tag = crate::dom::node_tag(block.node);
        let decoration = box_decoration_of(&block.style);
        let filters = image_filters(&block.style);
        let page = self.pages.last_mut().unwrap();
        if filters.is_empty() {
            if let Some(decoration) = decoration {
                page.fragments.push(Fragment {
                    rect,
                    kind: FragmentKind::Box(decoration),
                    tag,
                });
            }
        } else {
            self.pending_filters.push(PendingFilter {
                tag,
                background: decoration.and_then(|d| d.background),
                filters,
            });
        }
        self.pages.last_mut().unwrap().fragments.push(Fragment {
            rect,
            kind: FragmentKind::Image { resource: tag },
            tag,
        });
    }

    /// Place a replaced image block: scaled to fit the content width and
    /// page height, kept whole (an image never splits across pages), and
    /// centered horizontally.
    fn place_image(&mut self, block: &BlockBox, width: u32, height: u32, x: f32, inner_w: f32) {
        if width == 0 || height == 0 {
            return;
        }
        self.commit_margin();
        let mut w = (width as f32).min(inner_w);
        let mut h = height as f32 * w / width as f32;
        let max_h = self.content.size.h;
        if h > max_h {
            w *= max_h / h;
            h = max_h;
        }
        if h > self.remaining() + 0.01 && !self.at_page_top() {
            self.new_page_keeping();
        }
        let x_center = x + (inner_w - w) / 2.0;
        let rect = Rect {
            origin: Point::new(
                self.content.origin.x + x_center,
                self.content.origin.y + self.y,
            ),
            size: Size::new(w, h),
        };
        self.push_image(block, rect);
        self.y += h;
        self.placed_on_page += 1;
    }

    /// Place a natively rendered block formula: scaled down to the measure
    /// like an image, kept whole, centered (MathML Core block math centers
    /// on its container), and lowered to an ordinary line fragment — glyph
    /// runs on the math face, rules and `mathbackground` fills as
    /// decorations (decorations paint under the glyphs, and formulary
    /// emits backgrounds before the content they sit behind).
    #[cfg(feature = "mathml")]
    fn place_math(
        &mut self,
        block: &BlockBox,
        math: &crate::mathml::PreparedMath,
        x: f32,
        inner_w: f32,
    ) {
        use formulary::Item;
        let to_rgba = |c: formulary::Color| chapbook_core::Rgba::new(c.r, c.g, c.b, c.a);

        let (width, height) = (math.width, math.ascent + math.descent);
        if width <= 0.0 || height <= 0.0 {
            return;
        }
        self.commit_margin();
        // Shrink to the measure, never grow. A formula taller than a page
        // is placed alone and overflows, clipped at paint, like any
        // too-tall line.
        let scale = (inner_w / width).min(1.0);
        let (w, h) = (width * scale, height * scale);
        if h > self.remaining() + 0.01 && !self.at_page_top() {
            self.new_page_keeping();
        }
        let fallback_color = text_color(&block.style);
        let mut runs: Vec<GlyphRun> = Vec::new();
        let mut decorations: Vec<Decoration> = Vec::new();
        for item in &math.items {
            match *item {
                Item::Glyph {
                    id,
                    x: gx,
                    y: gy,
                    size,
                    advance,
                    color,
                    // `prepare` refused any layout with a mirrored glyph.
                    mirrored: _,
                } => {
                    let advance = advance * scale;
                    let color = color.map(to_rgba).unwrap_or(fallback_color);
                    let size = size * scale;
                    let glyph = Glyph {
                        id: id.0,
                        x: gx * scale,
                        y: gy * scale,
                        advance,
                        locator: math.locator,
                        // formulary lays out left to right, and `prepare`
                        // already refused any formula with a mirrored glyph.
                        rtl: false,
                    };
                    match runs.last_mut() {
                        Some(run) if run.font_size == size && run.color == color => {
                            run.glyphs.push(glyph);
                        }
                        _ => runs.push(GlyphRun {
                            font: math.font,
                            font_size: size,
                            font_weight: 400,
                            color,
                            glyphs: vec![glyph],
                        }),
                    }
                }
                Item::Rule {
                    x: rx,
                    y: ry,
                    w: rw,
                    h: rh,
                    color,
                } => decorations.push(Decoration {
                    x: rx * scale,
                    width: rw * scale,
                    y: (math.ascent + ry) * scale,
                    thickness: rh * scale,
                    color: color.map(to_rgba).unwrap_or(fallback_color),
                }),
                Item::Background {
                    x: rx,
                    y: ry,
                    w: rw,
                    h: rh,
                    color,
                } => decorations.push(Decoration {
                    x: rx * scale,
                    width: rw * scale,
                    y: (math.ascent + ry) * scale,
                    thickness: rh * scale,
                    color: to_rgba(color),
                }),
                // `Item` is non-exhaustive; an unknown future item kind
                // simply does not draw.
                _ => {}
            }
        }
        let rect = Rect {
            origin: Point::new(
                self.content.origin.x + x + (inner_w - w) / 2.0,
                self.content.origin.y + self.y,
            ),
            size: Size::new(w, h),
        };
        self.pages.last_mut().unwrap().fragments.push(Fragment {
            rect,
            kind: FragmentKind::Line(LineFragment {
                baseline: math.ascent * scale,
                runs,
                decorations,
                text: math.text.clone(),
                locator_start: math.locator,
            }),
            tag: crate::dom::node_tag(block.node),
        });
        self.y += h.min(self.content.size.h);
        self.placed_on_page += 1;
    }

    /// Place a floated image against the left or right content edge without
    /// advancing the flow cursor, and register its exclusion band. Margins
    /// are the float's own (they do not collapse with the flow).
    fn place_float_image(
        &mut self,
        block: &BlockBox,
        iw: u32,
        ih: u32,
        x: f32,
        width: f32,
        side: FloatSide,
    ) {
        if iw == 0 || ih == 0 {
            return;
        }
        let m = block.style.get_margin();
        let (mt, mb, ml, mr) = (
            resolve_margin(&m.margin_top, width),
            resolve_margin(&m.margin_bottom, width),
            resolve_margin(&m.margin_left, width),
            resolve_margin(&m.margin_right, width),
        );
        let mut w = (iw as f32).min((width - ml - mr).max(1.0));
        let mut h = ih as f32 * w / iw as f32;
        let max_h = (self.content.size.h - mt - mb).max(1.0);
        if h > max_h {
            w *= max_h / h;
            h = max_h;
        }
        // The float's top aligns with where following flow content would
        // start (pending margin included, but left uncommitted for it).
        let mut y0 = self.y
            + if self.at_page_top() {
                0.0
            } else {
                self.pending_margin
            };
        // A second float on the same side stacks below the first.
        let same_side = match side {
            FloatSide::Left => &self.float_left,
            FloatSide::Right => &self.float_right,
        };
        if let Some(band) = same_side {
            y0 = y0.max(band.y_end);
        }
        if y0 + mt + h + mb > self.content.size.h + 0.01 && !self.at_page_top() {
            self.new_page();
            y0 = 0.0;
        }
        let x_pos = match side {
            FloatSide::Left => x + ml,
            FloatSide::Right => x + width - mr - w,
        };
        let rect = Rect {
            origin: Point::new(
                self.content.origin.x + x_pos,
                self.content.origin.y + y0 + mt,
            ),
            size: Size::new(w, h),
        };
        self.push_image(block, rect);
        self.placed_on_page += 1;
        let band = FloatBand {
            width: ml + w + mr,
            y_end: (y0 + mt + h + mb).min(self.content.size.h),
        };
        match side {
            FloatSide::Left => self.float_left = Some(band),
            FloatSide::Right => self.float_right = Some(band),
        }
    }

    /// Place a floated non-replaced block (an aside, a pull quote) with an
    /// explicit CSS width: its content is laid out in a detached
    /// sub-paginator the width of the float's margin box, then the
    /// fragments are translated to the float position and an exclusion
    /// band is registered. A float taller than a page truncates at the
    /// first sub-page (documented degradation).
    fn place_float_block(&mut self, block: &BlockBox, x: f32, cw: f32, w: f32, side: FloatSide) {
        let style = &block.style;
        let m = style.get_margin();
        let (mt, mb, ml, mr) = (
            resolve_margin(&m.margin_top, cw),
            resolve_margin(&m.margin_bottom, cw),
            resolve_margin(&m.margin_left, cw),
            resolve_margin(&m.margin_right, cw),
        );
        // CSS width is the content-box width; the sub-paginator gets the
        // full margin-box width and re-derives the content box inside it
        // (percentage padding then resolves against the margin box — an
        // accepted approximation).
        let p = style.get_padding();
        let border = box_decoration_of(style)
            .map(|d| d.border_widths)
            .unwrap_or_default();
        let w = w.max(1.0).min((cw - ml - mr).max(1.0));
        let margin_box_w = ml
            + border.left
            + resolve_padding(&p.padding_left, cw)
            + w
            + resolve_padding(&p.padding_right, cw)
            + border.right
            + mr;

        let sub_metrics = PageMetrics {
            size: Size::new(margin_box_w, self.content.size.h),
            margins: chapbook_core::EdgeSizes::uniform(0.0),
            dpi_scale: self.page.dpi_scale,
            rotation: Rotation::None,
        };
        let mut sub = Paginator::new(&mut *self.fonts, sub_metrics);
        sub.in_float = true;
        sub.place_block(block, 0.0, margin_box_w);
        let (mut sub_pages, _, sub_pending) = sub.finish();
        self.pending_filters.extend(sub_pending);
        let fragments = std::mem::take(&mut sub_pages[0].fragments);
        let height = fragments
            .iter()
            .map(|f| f.rect.origin.y + f.rect.size.h)
            .fold(0.0f32, f32::max);
        if fragments.is_empty() || height <= 0.0 {
            return;
        }

        let mut y0 = self.y
            + if self.at_page_top() {
                0.0
            } else {
                self.pending_margin
            };
        let same_side = match side {
            FloatSide::Left => &self.float_left,
            FloatSide::Right => &self.float_right,
        };
        if let Some(band) = same_side {
            y0 = y0.max(band.y_end);
        }
        if y0 + mt + height + mb > self.content.size.h + 0.01 && !self.at_page_top() {
            self.new_page();
            y0 = 0.0;
        }
        let x_pos = match side {
            FloatSide::Left => x,
            FloatSide::Right => x + cw - margin_box_w,
        };
        for mut fragment in fragments {
            fragment.rect.origin.x += self.content.origin.x + x_pos;
            fragment.rect.origin.y += self.content.origin.y + y0 + mt;
            if let FragmentKind::Line(line) = &fragment.kind {
                let page_loc = self.page_locators.last_mut().unwrap();
                *page_loc = (*page_loc).min(line.locator_start);
            }
            self.pages.last_mut().unwrap().fragments.push(fragment);
            self.placed_on_page += 1;
        }
        let band = FloatBand {
            width: margin_box_w,
            y_end: (y0 + mt + height + mb).min(self.content.size.h),
        };
        match side {
            FloatSide::Left => self.float_left = Some(band),
            FloatSide::Right => self.float_right = Some(band),
        }
    }

    /// `clear`: move the cursor below the named floats' bottom edges.
    fn apply_clear(&mut self, left: bool, right: bool) {
        let mut target = self.y;
        if left {
            if let Some(band) = &self.float_left {
                target = target.max(band.y_end);
            }
        }
        if right {
            if let Some(band) = &self.float_right {
                target = target.max(band.y_end);
            }
        }
        self.commit_margin();
        if target > self.y {
            self.y = target;
        }
        let y = self.y;
        self.expire_bands(y);
    }

    fn expire_bands(&mut self, y: f32) {
        if self.float_left.is_some_and(|b| b.y_end <= y + 0.01) {
            self.float_left = None;
        }
        if self.float_right.is_some_and(|b| b.y_end <= y + 0.01) {
            self.float_right = None;
        }
    }

    /// Active band insets at `y`: (left inset, right inset, nearest bottom
    /// edge). Expired bands are dropped first.
    fn bands_at(&mut self, y: f32) -> (f32, f32, f32) {
        self.expire_bands(y);
        let l = self.float_left.map_or(0.0, |b| b.width);
        let r = self.float_right.map_or(0.0, |b| b.width);
        let edge = [self.float_left, self.float_right]
            .iter()
            .flatten()
            .map(|b| b.y_end)
            .fold(f32::INFINITY, f32::min);
        (l, r, edge)
    }

    /// Lay out an inline formatting context that starts beside one or more
    /// floats: shape at the reduced measure, emit the lines that fit above
    /// the float's bottom edge, split the IFC there, and repeat until the
    /// bands expire — the remainder flows through the normal path (which
    /// restores widow/orphan handling). A first-line indent narrows the
    /// first emitted line's measure like the normal indent path does.
    fn place_inline_with_floats(
        &mut self,
        inline: &InlineContent,
        block: &BlockBox,
        x: f32,
        inner_w: f32,
        indent: f32,
        tag: u64,
    ) {
        self.commit_margin();
        let min_measure = font_size_px(&block.style) * 2.0;
        let mut content = inline.clone();
        let mut indent_pending = indent.max(0.0);
        loop {
            if content.runs.is_empty() {
                return;
            }
            let y = self.y;
            let (l, r, edge) = self.bands_at(y);
            if l == 0.0 && r == 0.0 {
                let lines = if indent_pending > 0.5 {
                    self.shape_inline_indented(&content, &block.style, inner_w, indent_pending)
                } else {
                    self.shape_inline(&content, &block.style, inner_w)
                };
                self.place_lines(lines, block, x, tag);
                return;
            }
            let avail = inner_w - l - r;
            if avail < min_measure {
                // Not enough measure beside the float: drop below it.
                self.y = edge.min(self.content.size.h);
                continue;
            }
            // First-line indent beside a float: split the first line off at
            // the narrowed measure and emit it; the remainder re-enters the
            // loop with the indent consumed.
            if indent_pending > 0.5 {
                let indent = indent_pending.min(avail * 0.8);
                indent_pending = 0.0;
                let probe = self.shape_inline(&content, &block.style, avail - indent);
                let Some(first) = probe.first() else { return };
                if self.y + first.height > edge + 0.01 {
                    // The indented first line doesn't fit beside the float;
                    // drop below and re-run with the indent restored.
                    indent_pending = indent;
                    self.y = edge.min(self.content.size.h);
                    continue;
                }
                if first.line_index == 0 && first.byte_end > 0 {
                    if probe.len() == 1 {
                        let mut lines = probe;
                        lines[0].x_indent += indent;
                        self.emit_lines(&mut lines, x + l, tag);
                        return;
                    }
                    let split = first.byte_end;
                    let (first_part, rest) = split_inline(&content, split);
                    let mut lines = self.shape_inline(&first_part, &block.style, avail - indent);
                    if align_for(&block.style) == Some(cosmic_text::Align::Justified) {
                        for line in &mut lines {
                            justify_line(line, avail - indent);
                        }
                    }
                    for line in &mut lines {
                        line.x_indent += indent;
                    }
                    self.emit_lines(&mut lines, x + l, tag);
                    content = rest;
                    continue;
                }
                // Degenerate first line (leading <br>): fall through with
                // the indent dropped.
            }
            let mut lines = self.shape_inline(&content, &block.style, avail);
            if lines.is_empty() {
                return;
            }
            let mut used = 0.0f32;
            let mut take = 0usize;
            for line in &lines {
                if self.y + used + line.height > edge + 0.01 {
                    break;
                }
                used += line.height;
                take += 1;
            }
            if take == lines.len() {
                self.emit_lines(&mut lines, x + l, tag);
                return;
            }
            if take == 0 {
                self.y = edge.min(self.content.size.h);
                continue;
            }
            let last = &lines[take - 1];
            let split = buffer_line_start(&content, last.line_index) + last.byte_end;
            if split == 0 {
                self.y = edge.min(self.content.size.h);
                continue;
            }
            let (_, rest) = split_inline(&content, split);
            self.emit_lines(&mut lines[..take], x + l, tag);
            content = rest;
        }
    }

    /// First-line indent: cosmic-text has no hanging-indent support, so the
    /// first line is shaped at `width - indent` to find its break point, the
    /// IFC is split there, and the remainder re-shapes at full width.
    ///
    /// Known limitation: in justified paragraphs the split makes the first
    /// line its buffer's last line, which cosmic-text leaves unjustified —
    /// indented first lines of justified text render ragged-right.
    fn shape_inline_indented(
        &mut self,
        inline: &InlineContent,
        block_style: &ComputedValues,
        width: f32,
        indent: f32,
    ) -> Vec<ShapedLine> {
        // Never indent away more than most of the measure. The probe is a
        // full shape whose lines are reused verbatim when no split is
        // needed (dialogue: most indented paragraphs are one line). A
        // probe that stopped at the first line was tried and measured
        // slower — cosmic-text shapes per buffer line (per paragraph)
        // regardless, so it saved only materialization while costing
        // one-line paragraphs a second full shape.
        let indent = indent.min(width * 0.8);
        let probe = self.shape_inline(inline, block_style, width - indent);
        let needs_split = probe.len() > 1
            && probe
                .first()
                .is_some_and(|l| l.line_index == 0 && l.byte_end > 0);
        if !needs_split {
            let mut lines = probe;
            for line in &mut lines {
                line.x_indent += indent;
            }
            return lines;
        }

        let split_byte = probe[0].byte_end;
        let (first_part, rest_part) = split_inline(inline, split_byte);
        let mut lines = self.shape_inline(&first_part, block_style, width - indent);
        // The split-off line is its own buffer's last line, which
        // cosmic-text never justifies — distribute the slack over its
        // spaces manually so justified paragraphs stay justified.
        if align_for(block_style) == Some(cosmic_text::Align::Justified) {
            for line in &mut lines {
                justify_line(line, width - indent);
            }
        }
        for line in &mut lines {
            line.x_indent += indent;
        }
        lines.extend(self.shape_inline(&rest_part, block_style, width));
        lines
    }

    fn commit_margin(&mut self) {
        if !self.at_page_top() {
            self.y += self.pending_margin;
        }
        self.pending_margin = 0.0;
    }

    fn shape_inline(
        &mut self,
        inline: &InlineContent,
        block_style: &ComputedValues,
        width: f32,
    ) -> Vec<ShapedLine> {
        if inline.runs.is_empty() {
            return Vec::new();
        }
        let metrics = Metrics {
            font_size: font_size_px(block_style),
            line_height: line_height_px(block_style),
        };
        let mut buffer = Buffer::new(self.fonts, metrics);
        buffer.set_size(Some(width), None);
        buffer.set_wrap(Wrap::WordOrGlyph);

        let default_attrs = attrs_for(block_style, 0);
        let spans = inline
            .runs
            .iter()
            .enumerate()
            .map(|(i, run)| (run.text.as_str(), attrs_for(&run.style, i)));
        buffer.set_rich_text(
            spans,
            &default_attrs,
            Shaping::Advanced,
            align_for(block_style),
        );
        buffer.shape_until_scroll(self.fonts, false);

        // Byte→locator mapping of the buffer's text, split per buffer line
        // ('\n' from <br> starts a new line and is not part of any line's
        // text). Spans concatenate in order, so buffer byte order == run
        // concatenation order.
        let mut line_maps: Vec<Vec<(usize, u32)>> = vec![Vec::new()];
        let mut byte_in_line = 0usize;
        for run in &inline.runs {
            let mut offsets = run.offsets.iter();
            for ch in run.text.chars() {
                let locator = offsets.next().copied().unwrap_or(0);
                if ch == '\n' {
                    line_maps.push(Vec::new());
                    byte_in_line = 0;
                } else {
                    line_maps.last_mut().unwrap().push((byte_in_line, locator));
                    byte_in_line += ch.len_utf8();
                }
            }
        }
        let locator_at = |line_i: usize, byte: usize| -> u32 {
            let map = match line_maps.get(line_i) {
                Some(m) if !m.is_empty() => m,
                _ => return 0,
            };
            let idx = map.partition_point(|(b, _)| *b <= byte);
            map[idx.saturating_sub(1)].1
        };

        let fallback_color = text_color(block_style);
        let mut lines = Vec::new();
        for run in buffer.layout_runs() {
            let mut glyph_runs: Vec<GlyphRun> = Vec::new();
            let mut spaces: Vec<(usize, usize)> = Vec::new();
            let mut text_start = usize::MAX;
            let mut text_end = 0usize;
            for glyph in run.glyphs {
                text_start = text_start.min(glyph.start);
                text_end = text_end.max(glyph.end);
                let color = glyph
                    .color_opt
                    .map(|c| chapbook_core::Rgba::new(c.r(), c.g(), c.b(), c.a()))
                    .unwrap_or(fallback_color);
                let g = Glyph {
                    id: glyph.glyph_id,
                    x: glyph.x + glyph.x_offset,
                    y: glyph.y - glyph.y_offset,
                    advance: glyph.w,
                    locator: locator_at(run.line_i, glyph.start),
                    rtl: glyph.level.is_rtl(),
                };
                let is_space = run.text.get(glyph.start..glyph.end) == Some(" ");
                match glyph_runs.last_mut() {
                    Some(last)
                        if last.font == glyph.font_id
                            && last.font_size == glyph.font_size
                            && last.font_weight == glyph.font_weight.0
                            && last.color == color =>
                    {
                        last.glyphs.push(g);
                    }
                    _ => glyph_runs.push(GlyphRun {
                        font: glyph.font_id,
                        font_size: glyph.font_size,
                        font_weight: glyph.font_weight.0,
                        color,
                        glyphs: vec![g],
                    }),
                }
                if is_space {
                    let run_idx = glyph_runs.len() - 1;
                    let glyph_idx = glyph_runs[run_idx].glyphs.len() - 1;
                    spaces.push((run_idx, glyph_idx));
                }
            }
            let mut text = if text_start <= text_end && text_end <= run.text.len() {
                run.text[text_start..text_end].to_string()
            } else {
                String::new()
            };
            // Soft hyphens are zero-width invisibles in the glyph stream;
            // keep them out of the reported line text too. A line *ending*
            // at one broke there and gets a visible hyphen appended below.
            let ends_at_soft_hyphen = run
                .glyphs
                .last()
                .is_some_and(|g| run.text.get(g.start..g.end) == Some("\u{AD}"));
            if text.contains('\u{AD}') {
                text = text.replace('\u{AD}', "");
            }
            let locator_start = run
                .glyphs
                .first()
                .map(|g| locator_at(run.line_i, g.start))
                .unwrap_or(0);
            // Alignment (center/right/justify) shifts glyph x's within the
            // buffer; normalize so glyphs are relative to the line box and
            // the fragment rect reports the actual visual extent.
            let x_min = glyph_runs
                .iter()
                .flat_map(|r| r.glyphs.iter().map(|g| g.x))
                .fold(f32::INFINITY, f32::min);
            let x_min = if x_min.is_finite() { x_min } else { 0.0 };
            for run_mut in &mut glyph_runs {
                for g in &mut run_mut.glyphs {
                    g.x -= x_min;
                }
            }

            // Decorations: underline/strikethrough spans with font-derived
            // offsets and thickness (EM units scaled by the span font size).
            let baseline = run.line_y - run.line_top;
            let mut decorations = Vec::new();
            for span in run.decorations {
                let span_glyphs = &run.glyphs[span.glyph_range.clone()];
                let Some(first) = span_glyphs.first() else {
                    continue;
                };
                let last = span_glyphs.last().unwrap();
                let x0 = first.x - x_min;
                let x1 = last.x + last.w - x_min;
                let color = span
                    .color_opt
                    .map(|c| chapbook_core::Rgba::new(c.r(), c.g(), c.b(), c.a()))
                    .unwrap_or(fallback_color);
                // Font size lives on the span, not the metrics data.
                let size = span.font_size;
                let mut push = |metrics: cosmic_text::DecorationMetrics| {
                    let thickness = (metrics.thickness * size).max(1.0);
                    decorations.push(Decoration {
                        x: x0,
                        width: (x1 - x0).max(0.0),
                        y: baseline - metrics.offset * size,
                        thickness,
                        color,
                    });
                };
                if span.data.text_decoration.underline != cosmic_text::UnderlineStyle::None {
                    push(span.data.underline_metrics);
                }
                if span.data.text_decoration.strikethrough {
                    push(span.data.strikethrough_metrics);
                }
            }

            let mut line = ShapedLine {
                height: run.line_height,
                baseline,
                width: run.line_w,
                x_indent: x_min,
                runs: glyph_runs,
                decorations,
                text,
                locator_start,
                line_index: run.line_i,
                byte_end: if text_end >= text_start { text_end } else { 0 },
                spaces,
            };
            if ends_at_soft_hyphen {
                self.append_hyphen(&mut line, width);
            }
            lines.push(line);
        }
        lines
    }

    /// Append a visible hyphen glyph to a line that broke at a soft hyphen.
    /// If that pushes past the measure (justified lines are already
    /// stretched to it), the line re-tightens over its spaces.
    fn append_hyphen(&mut self, line: &mut ShapedLine, avail: f32) {
        let Some((font_id, font_size, font_weight)) = line
            .runs
            .last()
            .map(|r| (r.font, r.font_size, r.font_weight))
        else {
            return;
        };
        let cached = match self.hyphen_cache.get(&font_id) {
            Some(entry) => *entry,
            None => {
                let computed = self
                    .fonts
                    .get_font(font_id, cosmic_text::fontdb::Weight(font_weight))
                    .and_then(|font| {
                        let swash = font.as_swash();
                        let gid = swash.charmap().map('-');
                        (gid != 0)
                            .then(|| (gid, swash.glyph_metrics(&[]).scale(1.0).advance_width(gid)))
                    });
                self.hyphen_cache.insert(font_id, computed);
                computed
            }
        };
        let Some((glyph_id, advance_per_em)) = cached else {
            return;
        };
        let advance = advance_per_em * font_size;
        let x_end = line
            .runs
            .iter()
            .flat_map(|r| r.glyphs.iter())
            .map(|g| g.x + g.advance)
            .fold(0.0f32, f32::max);
        let y = line
            .runs
            .last()
            .and_then(|r| r.glyphs.last())
            .map_or(0.0, |g| g.y);
        // The hyphen belongs to the word it broke, so it takes that word's
        // last glyph's offset and its direction. `x_end` puts it at the
        // line's right edge, which is where a visible hyphen goes in an
        // LTR line and is wrong in an RTL one — but `hyphenation` only
        // ships en-US dictionaries, so no RTL line reaches here today.
        // See the module docs' note on non-English dictionaries.
        let (locator, rtl) = line
            .runs
            .last()
            .and_then(|r| r.glyphs.last())
            .map_or((0, false), |g| (g.locator, g.rtl));
        line.runs.last_mut().unwrap().glyphs.push(Glyph {
            id: glyph_id,
            x: x_end,
            y,
            advance,
            locator,
            rtl,
        });
        line.width = line.width.max(x_end + advance);
        line.text.push('-');
        if line.width > avail + 0.01 {
            justify_line(line, avail);
        }
    }

    fn place_lines(&mut self, mut lines: Vec<ShapedLine>, block: &BlockBox, x: f32, tag: u64) {
        if lines.is_empty() {
            return;
        }
        let orphans = block.frag.orphans.max(1) as usize;
        let widows = block.frag.widows.max(1) as usize;
        let total: usize = lines.len();
        let total_height: f32 = lines.iter().map(|l| l.height).sum();

        self.commit_margin();

        // break-inside: avoid — move the whole box to a fresh page when it
        // would otherwise split and can fit on one page.
        if block.frag.break_inside_avoid
            && !self.at_page_top()
            && total_height > self.remaining()
            && total_height <= self.content.size.h
        {
            self.new_page_keeping();
        }

        let mut i = 0usize;
        while i < total {
            // How many of the remaining lines fit on this page?
            let mut fits = 0usize;
            let mut used = 0.0f32;
            for line in &lines[i..] {
                if self.y + used + line.height > self.content.size.h + 0.01 {
                    break;
                }
                used += line.height;
                fits += 1;
            }
            let rest_after_page = total - i - fits;

            if rest_after_page == 0 {
                self.emit_lines(&mut lines[i..], x, tag);
                break;
            }

            let mut take = fits;
            // Orphans: too few lines would open the split on this page.
            if take < orphans {
                if !self.at_page_top() {
                    if i == 0 {
                        self.new_page_keeping();
                    } else {
                        self.new_page();
                    }
                    continue;
                }
                // Already at page top: force progress, overflow allowed.
                take = take.max(1);
            }
            // Widows: too few lines would land after the break.
            let rest = total - i - take;
            if rest > 0 && rest < widows {
                let deficit = widows - rest;
                if take > deficit && take - deficit >= orphans {
                    take -= deficit;
                } else if !self.at_page_top() {
                    if i == 0 {
                        self.new_page_keeping();
                    } else {
                        self.new_page();
                    }
                    continue;
                }
            }

            self.emit_lines(&mut lines[i..i + take], x, tag);
            i += take;
            if i < total {
                self.new_page();
            }
        }
    }

    /// Emit shaped lines as page fragments. Takes the lines' contents:
    /// every caller emits a given line exactly once, so the glyph vectors
    /// and text move instead of cloning — a chapter's worth of lines is
    /// megabytes of glyphs.
    fn emit_lines(&mut self, lines: &mut [ShapedLine], x: f32, tag: u64) {
        for line in lines {
            let rect = Rect {
                origin: Point::new(
                    self.content.origin.x + x + line.x_indent,
                    self.content.origin.y + self.y,
                ),
                size: Size::new(line.width, line.height),
            };
            let page_loc = self.page_locators.last_mut().unwrap();
            *page_loc = (*page_loc).min(line.locator_start);
            self.pages.last_mut().unwrap().fragments.push(Fragment {
                rect,
                kind: FragmentKind::Line(LineFragment {
                    baseline: line.baseline,
                    runs: std::mem::take(&mut line.runs),
                    decorations: std::mem::take(&mut line.decorations),
                    text: std::mem::take(&mut line.text),
                    locator_start: line.locator_start,
                }),
                tag,
            });
            self.y += line.height;
            self.placed_on_page += 1;
        }
    }
}

fn resolve_lp(lp: &LengthPercentage, containing: f32) -> f32 {
    lp.to_used_value(Au::from_f64_px(containing as f64))
        .to_f64_px() as f32
}

fn resolve_margin(margin: &style::values::computed::length::Margin, containing: f32) -> f32 {
    use style::values::generics::length::GenericMargin;
    match margin {
        GenericMargin::LengthPercentage(lp) => resolve_lp(lp, containing),
        _ => 0.0, // auto and anchor forms: v1 treats as zero
    }
}

fn resolve_padding(
    padding: &style::values::computed::NonNegativeLengthPercentage,
    containing: f32,
) -> f32 {
    resolve_lp(&padding.0, containing)
}

fn text_indent_px(style: &ComputedValues, containing: f32) -> f32 {
    let indent = &style.get_inherited_text().text_indent;
    // `hanging` / `each-line` keywords unsupported; negative indents clamp
    // to zero (they would escape the content box).
    if indent.hanging || indent.each_line {
        return 0.0;
    }
    resolve_lp(&indent.length, containing).max(0.0)
}

/// Byte offset (within the concatenation of `inline`'s runs) where buffer
/// line `line_index` starts — buffer lines are separated by '\n' runs from
/// `<br>`. Together with a `ShapedLine`'s `byte_end` this addresses a split
/// point anywhere in the IFC, not just on the first buffer line.
fn buffer_line_start(inline: &InlineContent, line_index: usize) -> usize {
    if line_index == 0 {
        return 0;
    }
    let mut abs = 0usize;
    let mut line = 0usize;
    for run in &inline.runs {
        for ch in run.text.chars() {
            abs += ch.len_utf8();
            if ch == '\n' {
                line += 1;
                if line == line_index {
                    return abs;
                }
            }
        }
    }
    abs
}

/// Split an inline formatting context at a byte offset into the
/// concatenation of its runs (the offset always falls on a char boundary:
/// glyph clusters end on them). Leading collapsible spaces of the remainder
/// are dropped — the line break consumed them.
fn split_inline(inline: &InlineContent, split_byte: usize) -> (InlineContent, InlineContent) {
    let mut first = InlineContent::default();
    let mut rest = InlineContent::default();
    let mut consumed = 0usize;
    for run in &inline.runs {
        let len = run.text.len();
        if consumed + len <= split_byte {
            first.runs.push(run.clone());
        } else if consumed >= split_byte {
            rest.runs.push(run.clone());
        } else {
            let local = split_byte - consumed;
            let char_count = run.text[..local].chars().count();
            let (a_text, b_text) = run.text.split_at(local);
            let (a_off, b_off) = run.offsets.split_at(char_count);
            if !a_text.is_empty() {
                first.runs.push(crate::boxtree::InlineRun {
                    text: a_text.to_string(),
                    offsets: a_off.to_vec(),
                    style: run.style.clone(),
                });
            }
            if !b_text.is_empty() {
                rest.runs.push(crate::boxtree::InlineRun {
                    text: b_text.to_string(),
                    offsets: b_off.to_vec(),
                    style: run.style.clone(),
                });
            }
        }
        consumed += len;
    }
    rest.trim_leading_space();
    (first, rest)
}

/// Background color + solid borders of a block, when it has any. Border
/// styles other than none/hidden all render solid (v1); per-side colors
/// collapse to the top border's color.
fn box_decoration_of(style: &ComputedValues) -> Option<BoxDecoration> {
    use style::values::specified::BorderStyle;
    let bg = style.get_background().background_color.clone();
    let bg = style.resolve_color(&bg);
    let background = {
        let c = crate::style_to_attrs::rgba(&bg);
        (!c.is_transparent()).then_some(c)
    };

    let border = style.get_border();
    let edge =
        |shown: BorderStyle, width: &style::values::computed::border::BorderSideWidth| -> f32 {
            if matches!(shown, BorderStyle::None | BorderStyle::Hidden) {
                0.0
            } else {
                width.0.to_f64_px() as f32
            }
        };
    let widths = chapbook_core::EdgeSizes {
        top: edge(border.border_top_style, &border.border_top_width),
        right: edge(border.border_right_style, &border.border_right_width),
        bottom: edge(border.border_bottom_style, &border.border_bottom_width),
        left: edge(border.border_left_style, &border.border_left_width),
    };
    let has_border =
        widths.top > 0.0 || widths.right > 0.0 || widths.bottom > 0.0 || widths.left > 0.0;
    if background.is_none() && !has_border {
        return None;
    }
    let border_color =
        crate::style_to_attrs::rgba(&style.resolve_color(&border.border_top_color.clone()));
    Some(BoxDecoration {
        background,
        border_widths: widths,
        border_color,
        first_slice: true,
        last_slice: true,
    })
}

// ---- Table layout ----

/// Per-cell box extras (padding + border on each axis) and decoration.
struct CellChrome {
    decoration: Option<BoxDecoration>,
    left: f32,
    right: f32,
    top: f32,
    bottom: f32,
}

#[derive(Clone, Copy)]
enum CellVAlign {
    Top,
    Middle,
    Bottom,
}

fn cell_valign(style: &ComputedValues) -> CellVAlign {
    // vertical-align is a shorthand over the css-inline-3 baseline
    // properties in stylo 0.20; `middle`/`text-bottom` are recoverable from
    // the computed alignment-baseline. Everything else (incl. baseline)
    // approximates to top in the v1 grid.
    use style::values::specified::box_::AlignmentBaseline;
    match style.get_box().alignment_baseline {
        AlignmentBaseline::Middle => CellVAlign::Middle,
        AlignmentBaseline::TextBottom => CellVAlign::Bottom,
        _ => CellVAlign::Top,
    }
}

fn cell_chrome(style: &ComputedValues, containing: f32) -> CellChrome {
    let p = style.get_padding();
    let decoration = box_decoration_of(style);
    let b = decoration
        .as_ref()
        .map(|d| d.border_widths)
        .unwrap_or_default();
    CellChrome {
        left: resolve_padding(&p.padding_left, containing) + b.left,
        right: resolve_padding(&p.padding_right, containing) + b.right,
        top: resolve_padding(&p.padding_top, containing) + b.top,
        bottom: resolve_padding(&p.padding_bottom, containing) + b.bottom,
        decoration,
    }
}

impl Paginator<'_> {
    /// Lay out a table: CSS auto column sizing (min/max content
    /// measurement), colspan distribution, rowspan (grid columns resolved
    /// in `crate::table`; spanned rows grow to fit and paginate as one
    /// band), border-spacing, atomic-row pagination. See `crate::table`
    /// for the v1 scope.
    fn place_table(
        &mut self,
        block: &BlockBox,
        table: &crate::table::TableBox,
        x: f32,
        width: f32,
    ) {
        // Caption first, as an ordinary block of lines.
        if let (Some(caption), Some(caption_style)) = (&table.caption, &table.caption_style) {
            let lines = self.shape_inline(caption, caption_style, width);
            self.place_lines(lines, block, x, crate::dom::node_tag(block.node));
        }

        let columns = table.columns;
        if columns == 0 {
            return;
        }
        let style = &block.style;
        let spacing = style.get_inherited_table().border_spacing;
        let (spacing_h, spacing_v) = (spacing.0.width.0.px(), spacing.0.height.0.px());
        let total_spacing = spacing_h * (columns as f32 + 1.0);

        // --- Column sizing: min/max content measurement ---
        let mut col_min = vec![0.0f32; columns];
        let mut col_max = vec![0.0f32; columns];
        let mut chrome_cache: Vec<Vec<CellChrome>> = Vec::with_capacity(table.rows.len());
        for row in &table.rows {
            let mut chromes = Vec::with_capacity(row.cells.len());
            for cell in &row.cells {
                let chrome = cell_chrome(&cell.style, width);
                let extras = chrome.left + chrome.right;
                let min_content = self.measure_width(&cell.content, &cell.style, 1.0) + extras;
                let max_content = self.measure_width(&cell.content, &cell.style, 1.0e9) + extras;
                let col = cell.col.min(columns - 1);
                let span = cell.colspan.min(columns - col);
                // Distribute a spanning cell's demand evenly over its columns.
                for i in 0..span {
                    let idx = (col + i).min(columns - 1);
                    col_min[idx] = col_min[idx].max(min_content / span as f32);
                    col_max[idx] = col_max[idx].max(max_content / span as f32);
                }
                chromes.push(chrome);
            }
            chrome_cache.push(chromes);
        }

        // --- Table width: shrink-to-fit unless an explicit width is set ---
        let available = (width - total_spacing).max(1.0);
        let explicit = explicit_width_px(style, width).map(|w| (w - total_spacing).max(1.0));
        let sum_min: f32 = col_min.iter().sum();
        let sum_max: f32 = col_max.iter().sum();
        let target = match explicit {
            Some(w) => w.min(available),
            None => sum_max.min(available),
        };
        let widths: Vec<f32> = if sum_max <= target {
            if explicit.is_some() && sum_max > 0.0 {
                // Stretch to the requested width, proportional to max.
                col_max
                    .iter()
                    .map(|m| m + (target - sum_max) * m / sum_max)
                    .collect()
            } else {
                col_max.clone()
            }
        } else if sum_min >= target {
            col_min.clone() // overflow allowed
        } else {
            // min + share of the slack proportional to (max - min).
            let slack = target - sum_min;
            let range = (sum_max - sum_min).max(0.001);
            col_min
                .iter()
                .zip(&col_max)
                .map(|(mn, mx)| mn + slack * (mx - mn) / range)
                .collect()
        };
        let table_w: f32 = widths.iter().sum::<f32>() + total_spacing;

        // --- Rows: pre-shape everything, then emit with pagination ---
        self.commit_margin();
        self.y += spacing_v;

        struct ShapedCell {
            x_rel: f32,
            span_w: f32,
            row_span: usize,
            chrome_left: f32,
            chrome_top: f32,
            chrome_bottom: f32,
            decoration: Option<BoxDecoration>,
            valign: CellVAlign,
            lines: Vec<ShapedLine>,
            content_h: f32,
            tag: u64,
        }
        struct ShapedRow {
            cells: Vec<ShapedCell>,
        }

        // Left edge of each grid column, relative to the table's left.
        let col_x: Vec<f32> = {
            let mut xs = Vec::with_capacity(columns);
            let mut cx = spacing_h;
            for w in &widths {
                xs.push(cx);
                cx += w + spacing_h;
            }
            xs
        };

        let mut shaped_rows: Vec<ShapedRow> = Vec::with_capacity(table.rows.len());
        // Heights from non-spanning cells first; spanning cells then grow
        // their rows below.
        let mut row_heights: Vec<f32> = Vec::with_capacity(table.rows.len());
        for (row_idx, row) in table.rows.iter().enumerate() {
            let chromes = &chrome_cache[row_idx];
            let mut cells = Vec::with_capacity(row.cells.len());
            for (cell, chrome) in row.cells.iter().zip(chromes) {
                let col = cell.col.min(columns - 1);
                let span = cell.colspan.min(columns - col);
                let span_w: f32 =
                    widths[col..col + span].iter().sum::<f32>() + spacing_h * (span as f32 - 1.0);
                let content_w = (span_w - chrome.left - chrome.right).max(1.0);
                let lines = self.shape_inline(&cell.content, &cell.style, content_w);
                let content_h: f32 = lines.iter().map(|l| l.height).sum();
                cells.push(ShapedCell {
                    x_rel: col_x[col],
                    span_w,
                    row_span: cell.rowspan.max(1),
                    chrome_left: chrome.left,
                    chrome_top: chrome.top,
                    chrome_bottom: chrome.bottom,
                    decoration: chrome.decoration.clone(),
                    valign: cell_valign(&cell.style),
                    lines,
                    content_h,
                    tag: crate::dom::node_tag(cell.node),
                });
            }
            let row_h = cells
                .iter()
                .filter(|c| c.row_span == 1)
                .map(|c| c.content_h + c.chrome_top + c.chrome_bottom)
                .fold(0.0f32, f32::max)
                .max(1.0);
            row_heights.push(row_h);
            shaped_rows.push(ShapedRow { cells });
        }

        // A rowspanning cell taller than its rows grows them, the excess
        // split evenly (CSS 2.1 leaves the distribution unspecified).
        for (r, row) in shaped_rows.iter().enumerate() {
            for cell in &row.cells {
                if cell.row_span <= 1 {
                    continue;
                }
                let end = (r + cell.row_span).min(row_heights.len());
                let have: f32 =
                    row_heights[r..end].iter().sum::<f32>() + spacing_v * (end - r - 1) as f32;
                let need = cell.content_h + cell.chrome_top + cell.chrome_bottom;
                if need > have {
                    let grow = (need - have) / (end - r) as f32;
                    for h in &mut row_heights[r..end] {
                        *h += grow;
                    }
                }
            }
        }

        // Border-box height of a cell starting at row `r`: its spanned
        // rows plus the border-spacing between them.
        let cell_height = |r: usize, row_span: usize, row_heights: &[f32]| -> f32 {
            let end = (r + row_span).min(row_heights.len());
            row_heights[r..end].iter().sum::<f32>() + spacing_v * (end - r - 1) as f32
        };

        // Header row repeats at the top of continuation pages.
        let header: Option<&ShapedRow> = table
            .rows
            .first()
            .filter(|r| r.is_header)
            .and_then(|_| shaped_rows.first());

        let emit_row = |this: &mut Self, row: &ShapedRow, r_idx: usize, row_heights: &[f32]| {
            let y_top = this.y;
            for cell in &row.cells {
                // A rowspanning cell is drawn once, at its starting row,
                // with the combined height of the rows it spans.
                let cell_h = cell_height(r_idx, cell.row_span, row_heights);
                if let Some(decoration) = cell.decoration.clone() {
                    this.pages.last_mut().unwrap().fragments.push(Fragment {
                        rect: Rect {
                            origin: Point::new(
                                this.content.origin.x + x + cell.x_rel,
                                this.content.origin.y + y_top,
                            ),
                            size: Size::new(cell.span_w, cell_h),
                        },
                        kind: FragmentKind::Box(decoration),
                        tag: cell.tag,
                    });
                    this.placed_on_page += 1;
                }
                // vertical-align: distribute the slack above the content.
                let slack =
                    (cell_h - cell.content_h - cell.chrome_top - cell.chrome_bottom).max(0.0);
                let mut ly = y_top
                    + cell.chrome_top
                    + match cell.valign {
                        CellVAlign::Top => 0.0,
                        CellVAlign::Middle => slack / 2.0,
                        CellVAlign::Bottom => slack,
                    };
                for line in &cell.lines {
                    let rect = Rect {
                        origin: Point::new(
                            this.content.origin.x
                                + x
                                + cell.x_rel
                                + cell.chrome_left
                                + line.x_indent,
                            this.content.origin.y + ly,
                        ),
                        size: Size::new(line.width, line.height),
                    };
                    let page_loc = this.page_locators.last_mut().unwrap();
                    *page_loc = (*page_loc).min(line.locator_start);
                    this.pages.last_mut().unwrap().fragments.push(Fragment {
                        rect,
                        kind: FragmentKind::Line(LineFragment {
                            baseline: line.baseline,
                            runs: line.runs.clone(),
                            decorations: line.decorations.clone(),
                            text: line.text.clone(),
                            locator_start: line.locator_start,
                        }),
                        tag: cell.tag,
                    });
                    this.placed_on_page += 1;
                    ly += line.height;
                }
            }
            this.y += row_heights[r_idx] + spacing_v;
        };

        // Rows tied together by a rowspan paginate as one atomic band:
        // moving only part of a spanned group would strand the spanning
        // cell's content on the previous page.
        let mut bands: Vec<(usize, usize)> = Vec::new();
        let mut start = 0usize;
        while start < shaped_rows.len() {
            let mut end = start + 1;
            let mut i = start;
            while i < end {
                for cell in &shaped_rows[i].cells {
                    end = end.max(i + cell.row_span);
                }
                i += 1;
            }
            let end = end.min(shaped_rows.len());
            bands.push((start, end));
            start = end;
        }

        for &(start, end) in &bands {
            // Move a multi-row band whole when it would otherwise split and
            // can fit on one page.
            if end - start > 1 {
                let band_h: f32 = row_heights[start..end].iter().sum::<f32>()
                    + spacing_v * (end - start - 1) as f32;
                if band_h > self.remaining() + 0.01
                    && band_h <= self.content.size.h
                    && !self.at_page_top()
                {
                    if start == 0 {
                        self.new_page_keeping();
                    } else {
                        self.new_page();
                    }
                    self.y += spacing_v;
                    if start > 0 {
                        if let Some(header) = header {
                            emit_row(self, header, 0, &row_heights);
                        }
                    }
                }
            }
            for r in start..end {
                // Rows stay atomic; an over-tall band still breaks between
                // its rows and the spanning cell truncates at the page edge
                // (clipped at paint — documented degradation).
                if row_heights[r] > self.remaining() + 0.01 && !self.at_page_top() {
                    if r == 0 {
                        self.new_page_keeping();
                    } else {
                        self.new_page();
                    }
                    self.y += spacing_v;
                    if r > 0 {
                        if let Some(header) = header {
                            emit_row(self, header, 0, &row_heights);
                        }
                    }
                }
                emit_row(self, &shaped_rows[r], r, &row_heights);
            }
        }
        let _ = table_w; // width available for future table-level decoration
    }

    /// Widest visual line of `content` when shaped at `probe_width` — the
    /// min-content (probe ≈ 0) and max-content (probe ≈ ∞) measurement.
    fn measure_width(
        &mut self,
        content: &InlineContent,
        style: &ComputedValues,
        probe_width: f32,
    ) -> f32 {
        self.shape_inline(content, style, probe_width)
            .iter()
            .map(|l| l.width)
            .fold(0.0, f32::max)
    }
}

/// Explicit CSS width of a block (a table, a floated aside), resolved
/// against the containing block, when specified.
fn explicit_width_px(style: &ComputedValues, containing: f32) -> Option<f32> {
    use style::values::generics::length::GenericSize;
    match &style.get_position().width {
        GenericSize::LengthPercentage(lp) => Some(resolve_lp(&lp.0, containing)),
        _ => None,
    }
}

/// Distribute slack across a line's space glyphs so it fills `target`
/// width — stretching (split-off first lines of justified paragraphs,
/// which cosmic-text treats as buffer-last and never justifies) or
/// tightening (a line that grew a visible hyphen past the measure).
/// Decoration spans move and stretch with the spaces they cover.
fn justify_line(line: &mut ShapedLine, target: f32) {
    let extra = target - line.width;
    if extra.abs() <= 0.1 || line.spaces.is_empty() {
        return;
    }
    let add = extra / line.spaces.len() as f32;
    // Pre-shift x of each space glyph, for adjusting decoration spans.
    let space_xs: Vec<f32> = line
        .spaces
        .iter()
        .map(|&(r, g)| line.runs[r].glyphs[g].x)
        .collect();
    let mut spaces = line.spaces.iter().peekable();
    let mut shift = 0.0f32;
    for (run_idx, run) in line.runs.iter_mut().enumerate() {
        for (glyph_idx, glyph) in run.glyphs.iter_mut().enumerate() {
            glyph.x += shift;
            if spaces.peek() == Some(&&(run_idx, glyph_idx)) {
                glyph.advance += add;
                shift += add;
                spaces.next();
            }
        }
    }
    for dec in &mut line.decorations {
        let before = space_xs.iter().filter(|x| **x < dec.x - 0.01).count();
        let within = space_xs
            .iter()
            .filter(|x| **x >= dec.x - 0.01 && **x < dec.x + dec.width - 0.01)
            .count();
        dec.x += add * before as f32;
        dec.width = (dec.width + add * within as f32).max(0.0);
    }
    line.width = target;
}
