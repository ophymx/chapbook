//! **Internal to chapbook — no API stability.** Depend on
//! `chapbook-reader`; see `docs/STABILITY.md`.
//!
//! The pagination-first layout engine — chapbook's differentiator.
//!
//! The whole stylo-facing half of chapbook lives here, because it moves as
//! one: [`dom`] is the arena DOM and stylo's DOM-trait bindings, [`cascade`]
//! drives stylo's `Stylist` over it, and the rest of the crate turns the
//! result into pages. A stylo upgrade rewrites all three together.
//!
//! Pipeline: parse → cascade → fragmentation sidecar cascade (servo-mode stylo
//! lacks the break properties) → box tree (CSS 2.1 §9.2 anonymous boxes) →
//! cosmic-text inline layout per inline formatting context → streaming page
//! cursor applying break rules and widows/orphans → [`ChapterLayout`].
//!
//! One spine item is one layout run. Locator offsets (see chapbook-core's
//! locator docs) thread through every line fragment, so positions survive
//! relayout via [`ChapterLayout::page_of`].
//!
//! Gaps, documented in the module that owns each (all in `crate::boxtree`):
//! `counter()`/`counters()` and images in generated `content`, and
//! shrink-to-fit non-replaced floats, which stay in flow.

mod boxtree;
pub mod cascade;
pub mod dom;
mod fonts;
mod fragmentation;
mod hyphenate;
#[cfg(feature = "mathml")]
mod mathml;
mod paginate;
mod style_to_attrs;
#[cfg(feature = "svg")]
mod svg;
mod table;
mod webfonts;

use std::collections::HashMap;

use cosmic_text::FontSystem;

use chapbook_core::PageMetrics;
use chapbook_paint::{ImageStore, Page};

use crate::dom::Document;

pub use fonts::build_font_system;
#[cfg(feature = "mathml")]
pub use fonts::MATH_FONT_FAMILY;
pub use fragmentation::{BreakRule, FragRules, FragStyle};
pub use webfonts::{extract_font_faces, register_font, FontFace};

/// The paginated result of laying out one spine item.
pub struct ChapterLayout {
    pub pages: Vec<Page>,
    /// Per page: locator-text char offset at which the page starts
    /// (monotonic non-decreasing). The relayout-survival map for
    /// `Locator::char_offset`.
    pub char_map: Vec<u32>,
    /// Element `id` attribute → page index (TOC fragment jumps).
    pub anchors: HashMap<String, usize>,
}

impl ChapterLayout {
    /// Roughly how many heap bytes this laid-out chapter holds. See
    /// [`Page::approx_bytes`](chapbook_paint::Page::approx_bytes) for why
    /// approximate is the right precision here.
    pub fn approx_bytes(&self) -> usize {
        use std::mem::size_of;
        self.pages.capacity() * size_of::<chapbook_paint::Page>()
            + self.pages.iter().map(|p| p.approx_bytes()).sum::<usize>()
            + self.char_map.capacity() * size_of::<u32>()
            + self
                .anchors
                .keys()
                .map(|k| k.len() + size_of::<usize>())
                .sum::<usize>()
    }

    /// Page containing the given locator offset.
    pub fn page_of(&self, char_offset: u32) -> usize {
        page_of(&self.char_map, char_offset)
    }
}

fn page_of(char_map: &[u32], char_offset: u32) -> usize {
    if char_map.is_empty() {
        return 0;
    }
    char_map
        .partition_point(|start| *start <= char_offset)
        .saturating_sub(1)
}

/// Paginate a styled document (the cascade must have run: see
/// [`cascade::StyleEngine::style_document`]).
///
/// `css_sources` are the same author sheets given to the style engine — the
/// fragmentation sidecar re-reads them for the break properties stylo
/// doesn't carry. `images` is read for intrinsic sizes and written for
/// images whose element carries a `filter`: those get a derived copy in
/// the store (see [`ImageStore::derive`]) and their fragment points at it.
pub fn paginate(
    doc: &Document,
    css_sources: &[String],
    page: &PageMetrics,
    fonts: &mut FontSystem,
    images: &mut ImageStore,
) -> ChapterLayout {
    let frag = FragRules::parse(css_sources).resolve(doc);
    let locator = crate::dom::locator_offsets(doc);
    #[cfg(feature = "mathml")]
    let math = crate::mathml::prepare(doc, fonts, &locator);
    let (mut pages, char_map, pending) = {
        let input = boxtree::BoxTreeInput {
            doc,
            frag: &frag,
            locator: &locator,
            images: &*images,
            #[cfg(feature = "mathml")]
            math: &math,
            quote_depth: std::cell::Cell::new(0),
        };

        let mut paginator = paginate::Paginator::new(fonts, *page);
        if let Some(root) = boxtree::build_box_tree(&input) {
            paginator.place_block(&root, 0.0, page.content_width());
        }
        paginator.finish()
    };
    // An image whose element carries a `filter` paints as a derived copy:
    // composed once, here, where the element's style, the theme's media
    // state and the pixels all meet — so the display list keeps its three
    // dumb ops and no backend learns colour maths. The store is borrowed
    // mutably only now, after the box tree that read it for sizes is gone.
    paginate::resolve_filters(&mut pages, images, pending);

    // Anchors: element id → page, via each element's locator offset.
    let mut anchors = HashMap::new();
    for id in doc.descendants(doc.root()) {
        if let crate::dom::NodeData::Element(el) = &doc.node(id).data {
            if let (Some(id_attr), Some(offset)) = (&el.id, locator.get(&id)) {
                anchors.insert(id_attr.to_string(), page_of(&char_map, *offset));
            }
        }
    }

    ChapterLayout {
        pages,
        char_map,
        anchors,
    }
}

/// Decode every `<img>`'s bytes — and rasterize every inline `<svg>` — into
/// an [`ImageStore`] keyed by node tag. `fetch` resolves an `src` attribute
/// or SVG `<image>` href (as written) to raw bytes — callers close over
/// their container (e.g. `Book::resource` against the chapter path).
/// Undecodable or unresolvable images are skipped; layout degrades them to
/// nothing (an inline `<svg>` degrades to its flattened text).
///
/// `fonts` shapes any `<text>` inside SVG content; `None` renders SVG
/// without text. Ignored entirely when the `svg` feature is off.
#[cfg_attr(not(feature = "svg"), allow(unused_variables, unused_mut))]
pub fn collect_images(
    doc: &Document,
    fonts: Option<&FontSystem>,
    mut fetch: impl FnMut(&str) -> Option<Vec<u8>>,
) -> ImageStore {
    let mut store = ImageStore::default();
    // Built on first use: rebuilding the session's faces as usvg's fontdb
    // is not free, and most books have no SVG at all — and of those that
    // do, most carry no `<text>`, so `rasterize` may never ask.
    #[cfg(feature = "svg")]
    let mut svg_fonts: Option<std::sync::Arc<resvg::usvg::fontdb::Database>> = None;
    #[cfg(feature = "svg")]
    let mut svg_db = || {
        svg_fonts
            .get_or_insert_with(|| match fonts {
                Some(fonts) => crate::svg::svg_fontdb(fonts),
                None => std::sync::Arc::new(resvg::usvg::fontdb::Database::new()),
            })
            .clone()
    };

    // `<img>` nodes and their sources up front, so a source referenced by
    // several elements (a repeated ornament, a shared figure) fetches and
    // decodes once. Only repeated sources pay for a cache entry, and the
    // cache dies with this call — the single-use image costs what it did.
    let mut img_nodes: Vec<(crate::dom::NodeId, &str)> = Vec::new();
    let mut uses: HashMap<&str, u32> = HashMap::new();
    for id in doc.descendants(doc.root()) {
        let crate::dom::NodeData::Element(el) = &doc.node(id).data else {
            continue;
        };
        if *el.local_name() != markup5ever::local_name!("img") {
            continue;
        }
        let Some(src) = el.attr(&markup5ever::local_name!("src")) else {
            continue;
        };
        img_nodes.push((id, src));
        *uses.entry(src).or_insert(0) += 1;
    }

    type Decoded = Option<(u32, u32, Vec<u8>)>;
    let mut decoded_cache: HashMap<&str, Decoded> = HashMap::new();
    for (id, src) in img_nodes {
        if let Some(cached) = decoded_cache.get(src) {
            if let Some((w, h, rgba)) = cached {
                store.insert(crate::dom::node_tag(id), *w, *h, rgba.clone());
            }
            continue;
        }
        let decoded = fetch(src).and_then(|bytes| {
            #[cfg(feature = "svg")]
            if crate::svg::sniff(&bytes) {
                // Hrefs inside the SVG file resolve relative to the file,
                // not the chapter that embedded it.
                let mut nested = |href: &str| fetch(&join_href(src, href));
                return crate::svg::rasterize(&bytes, &mut nested, &mut svg_db);
            }
            let rgba = image::load_from_memory(&bytes).ok()?.to_rgba8();
            let (w, h) = (rgba.width(), rgba.height());
            Some((w, h, rgba.into_raw()))
        });
        if uses[src] > 1 {
            decoded_cache.insert(src, decoded.clone());
        }
        if let Some((w, h, rgba)) = decoded {
            store.insert(crate::dom::node_tag(id), w, h, rgba);
        }
    }

    // Inline `<svg>` subtrees, serialized at parse time. Their hrefs are
    // chapter-relative, exactly like an `<img>` src.
    #[cfg(feature = "svg")]
    for (id, xml) in doc.svg_sources() {
        if let Some((w, h, rgba)) = crate::svg::rasterize(xml.as_bytes(), &mut fetch, &mut svg_db) {
            store.insert(crate::dom::node_tag(id), w, h, rgba);
        }
    }
    store
}

/// Resolve `href` against the directory of `src` (both as written in the
/// book). The container's own resolution handles any `..` segments.
#[cfg(feature = "svg")]
fn join_href(src: &str, href: &str) -> String {
    match src.rsplit_once('/') {
        Some((dir, _)) => format!("{dir}/{href}"),
        None => href.to_string(),
    }
}
