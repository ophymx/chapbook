//! Laying out spine units — the full text pipeline for EPUB chapters, a
//! fabricated single-image page for image books — plus the loader-thread
//! glue that feeds the latter.

use std::collections::HashMap;

use chapbook_core::{BookKind, PageMetrics, Publication};
use chapbook_layout::{cascade, dom, ChapterLayout};
#[cfg(feature = "_image-book")]
use chapbook_paint::FrameIntent;
use chapbook_paint::ImageStore;

#[cfg(feature = "_image-book")]
use crate::loader::{DecodedUnit, Loader};
use crate::open::OpenBook;
#[cfg(feature = "_image-book")]
use crate::LoadedUnit;
use crate::Session;

impl Session {
    /// Install the shell's wakeup: invoked from the loader thread whenever
    /// a unit finishes, so the shell can call [`Session::poll_loaded`] and
    /// redraw. Shells without a thread-safe wakeup may poll instead while
    /// [`Session::has_pending_loads`] is true.
    pub fn set_waker(&mut self, wake: impl Fn() + Send + Sync + 'static) {
        *self.waker.lock().unwrap() = Some(Box::new(wake));
    }

    /// Drain finished background loads into the caches; returns true when
    /// the page on screen changed and the shell should redraw.
    ///
    /// Always false in a build with no format that loads units in the
    /// background — the signature stays put so shells compile unchanged.
    #[cfg(not(feature = "_image-book"))]
    pub fn poll_loaded(&mut self) -> bool {
        false
    }

    /// Drain finished background loads into the caches; returns true when
    /// the page on screen changed and the shell should redraw.
    ///
    /// A prefetched unit landing is *not* that. It changes nothing the
    /// reader can see, and saying otherwise cost a full-page panel update
    /// per prefetch — on e-ink, a visible flash of a page that did not
    /// move.
    #[cfg(feature = "_image-book")]
    pub fn poll_loaded(&mut self) -> bool {
        let Some(loader) = self.loader.as_mut() else {
            return false;
        };
        let results = loader.drain();
        let mut visible = false;
        for (spine, result) in results {
            visible |= spine == self.spine;
            match result {
                Ok(unit) => {
                    let DecodedUnit {
                        width,
                        height,
                        rgba,
                        text,
                        natural,
                    } = unit;
                    let unit = self.unit_mut(spine);
                    unit.images.get_or_insert_with(Default::default).insert(
                        spine as u64 + 1,
                        width,
                        height,
                        rgba,
                    );
                    unit.loaded = Some(LoadedUnit {
                        width,
                        height,
                        text,
                        natural,
                    });
                    self.push_event(crate::SessionEvent::UnitLoaded { spine });
                }
                Err(message) => {
                    log::error!("page {} failed to load: {message}", spine + 1);
                    self.load_errors.insert(spine, message.clone());
                    // Recorded above so a retry is not queued every frame,
                    // and reported here so a shell can say so: until this
                    // existed the page simply stayed a placeholder.
                    self.push_event(crate::SessionEvent::UnitFailed { spine, message });
                }
            }
            let unit = self.unit_mut(spine);
            if std::mem::take(&mut unit.placeholder) {
                unit.layout = None;
                unit.layout_bytes = 0;
            }
            // A page that just landed is 15 MB of decoded RGBA for a comic;
            // this is the moment the cache grows, so it is the moment to
            // check it still fits. Sparing the arrival keeps a load that
            // was asked for from being thrown away before it is drawn.
            self.touch(spine);
            self.evict_keeping(Some(spine));
        }
        if visible {
            // The region is exactly where the image was placed: a page
            // image is letterboxed into the content box, so the ground
            // around it is the same ground the placeholder painted. A unit
            // that failed to load has no fragment to point at, and takes
            // the whole page.
            let spine = self.spine;
            let placed = self
                .layout_unit(spine)
                .and_then(|layout| layout.pages.first())
                .and_then(|page| page.fragments.first())
                .map(|fragment| fragment.rect);
            match placed {
                Some(rect) => self.mark_rect(FrameIntent::ContentArrived, rect),
                None => self.mark(FrameIntent::ContentArrived),
            }
        }
        visible
    }

    /// Whether background loads are in flight (placeholder pages showing).
    pub fn has_pending_loads(&self) -> bool {
        #[cfg(not(feature = "_image-book"))]
        {
            false
        }
        #[cfg(feature = "_image-book")]
        {
            self.loader.as_ref().is_some_and(Loader::has_pending)
        }
    }

    // ---- Layout ----

    /// Lay out one spine unit (cached per metrics+settings): the full
    /// text pipeline for EPUB chapters, a fabricated single-image page for
    /// comic units.
    pub(crate) fn layout_unit(&mut self, spine: usize) -> Option<&ChapterLayout> {
        let metrics = self.metrics?;
        if self.layout(spine).is_none() {
            let built = match self.book.publication().kind() {
                BookKind::Epub => self.layout_text_unit(spine, &metrics),
                // Image books load on the worker; a placeholder shows
                // until the decoded unit arrives. Their pixels live in the
                // image store already (inserted by poll_loaded).
                BookKind::Comic | BookKind::Pdf => {
                    let layout = self.layout_image_unit(spine, &metrics);
                    self.cache_layout(spine, layout);
                    self.touch(spine);
                    self.evict_keeping(Some(spine));
                    return self.layout(spine);
                }
            };
            let (layout, images) = match built {
                Some((layout, images)) => (layout, Some(images)),
                None => return None,
            };
            self.cache_layout(spine, layout);
            if let Some(images) = images {
                self.unit_mut(spine).images = Some(images);
            }
        }
        // Touch on every call, not only on a build: eviction order is
        // about what is being *read*, and a unit served from cache is the
        // most-used thing there is.
        self.touch(spine);
        self.evict_keeping(Some(spine));
        self.layout(spine)
    }

    /// Build the one-page layout for an image-book unit. When the unit
    /// hasn't loaded yet, this queues it on the worker (plus a one-page
    /// prefetch) and returns an empty placeholder page — the shell redraws
    /// via the waker when the pixels arrive. PDF units also get their
    /// hidden text layer, scaled from natural (point) coordinates into the
    /// placed image rect, so selection works on them.
    fn layout_image_unit(&mut self, spine: usize, metrics: &PageMetrics) -> ChapterLayout {
        // Prefetch the next unit while we're here. Without a
        // background-loading format there is no thread to prefetch onto —
        // and no unit that could reach here to want one.
        #[cfg(feature = "_image-book")]
        if let Some(loader) = self.loader.as_mut() {
            let next = spine + 1;
            if next < self.book.publication().spine().len()
                && !self.units.get(&next).is_some_and(|u| u.loaded.is_some())
                && !self.load_errors.contains_key(&next)
            {
                loader.request(next);
            }
        }
        let Some(unit) = self.units.get(&spine).and_then(|u| u.loaded.as_ref()) else {
            #[cfg(feature = "_image-book")]
            if !self.load_errors.contains_key(&spine) {
                if let Some(loader) = self.loader.as_mut() {
                    loader.request(spine);
                    // Field access, not `unit_mut`: the loader borrow is
                    // still live, and a method call would borrow all of
                    // `self` across it.
                    self.units.entry(spine).or_default().placeholder = true;
                }
            }
            // Placeholder: an empty themed page until the load lands.
            let content = chapbook_core::Rect::new(
                metrics.margins.left,
                metrics.margins.top,
                metrics.content_width(),
                metrics.content_height(),
            );
            return ChapterLayout {
                pages: vec![chapbook_paint::Page {
                    size: metrics.size,
                    content,
                    fragments: Vec::new(),
                }],
                char_map: vec![0],
                anchors: HashMap::new(),
            };
        };
        let resource = spine as u64 + 1;
        let mut page = chapbook_paint::image_page(metrics, unit.width, unit.height, resource);
        if !unit.text.is_empty() {
            if let Some(image_rect) = page.fragments.first().map(|f| f.rect) {
                push_hidden_text(&mut page, &unit.text, unit.natural, image_rect);
            }
        }
        ChapterLayout {
            pages: vec![page],
            char_map: vec![0],
            anchors: HashMap::new(),
        }
    }

    fn layout_text_unit(
        &mut self,
        spine: usize,
        metrics: &PageMetrics,
    ) -> Option<(ChapterLayout, ImageStore)> {
        // Irrefutable when EPUB is the only format compiled in, and the
        // guard is still the right thing to write: which variants exist is
        // a build option, and this function is only correct for one of them.
        #[allow(irrefutable_let_patterns)]
        let OpenBook::Epub(epub) = &self.book
        else {
            return None;
        };
        let href = epub.spine_item(spine).ok()?.href.clone();
        let bytes = epub.unit_bytes(spine).ok()?;
        let mut doc = dom::parse_xhtml(&bytes, &href).ok()?;
        let css: Vec<(String, String)> = doc
            .stylesheet_sources()
            .iter()
            .filter_map(|s| match s {
                dom::StylesheetSource::Inline(t) => Some((t.clone(), href.clone())),
                dom::StylesheetSource::External(rel) => epub.resource(&href, rel).ok().map(|r| {
                    (
                        String::from_utf8_lossy(&r.data).into_owned(),
                        chapbook_epub::resolve_href(&href, rel),
                    )
                }),
            })
            .collect();

        for face in chapbook_layout::extract_font_faces(&css) {
            if !self.registered_fonts.insert(face.family.clone()) {
                continue;
            }
            for src in &face.sources {
                if let Ok(res) = epub.resource(&face.base, src) {
                    if chapbook_layout::register_font(&mut self.fonts, &face.family, res.data) {
                        break;
                    }
                }
            }
        }
        let mut images = chapbook_layout::collect_images(&doc, Some(&self.fonts), |img_href| {
            epub.resource(&href, img_href).ok().map(|r| r.data)
        });

        let sheets: Vec<String> = css.iter().map(|(text, _)| text.clone()).collect();
        // NOT kept across chapters, although StyleEngine is built for it
        // (`set_author_sheets` swaps sheets in place): stylo's `Device`
        // owns a `Box<dyn FontMetricsProvider>` without a `Send` bound, so
        // a retained engine would cost `Session: Send` — the FFI contract.
        // Revisit at the next stylo upgrade.
        let mut engine = cascade::StyleEngine::new(metrics, &self.settings);
        engine.set_author_sheets(&sheets);
        engine.style_document(&mut doc);
        // The document is parsed here and nowhere else; take its links
        // while we have it.
        // Field access, not `unit_mut`: `epub` borrows `self.book` for
        // the rest of this function.
        self.units.entry(spine).or_default().links = Some(dom::links(&doc));
        let layout =
            chapbook_layout::paginate(&doc, &sheets, metrics, &mut self.fonts, &mut images);
        Some((layout, images))
    }
}

/// Map a PDF unit's extracted text lines into hidden-text fragments over
/// the placed page image: natural (point) coordinates scale uniformly into
/// the image rect, glyph offsets become the page's locator space.
fn push_hidden_text(
    page: &mut chapbook_paint::Page,
    lines: &[chapbook_core::TextLine],
    natural: (f32, f32),
    image_rect: chapbook_core::Rect,
) {
    let factor = image_rect.size.w / natural.0.max(0.001);
    for line in lines {
        let Some(first) = line.glyphs.first() else {
            continue;
        };
        let min_x = line
            .glyphs
            .iter()
            .map(|g| g.x)
            .fold(f32::INFINITY, f32::min);
        let max_x = line
            .glyphs
            .iter()
            .map(|g| g.x + g.width)
            .fold(f32::NEG_INFINITY, f32::max);
        if max_x <= min_x {
            continue;
        }
        let rect = chapbook_core::Rect::new(
            image_rect.origin.x + min_x * factor,
            image_rect.origin.y + line.top * factor,
            (max_x - min_x) * factor,
            line.height * factor,
        );
        let glyphs: Vec<chapbook_paint::Glyph> = line
            .glyphs
            .iter()
            .map(|g| chapbook_paint::Glyph {
                id: 0,
                x: (g.x - min_x) * factor,
                y: 0.0,
                advance: g.width * factor,
                locator: g.offset,
                // A PDF's text layer is positioned glyphs, not a bidi
                // paragraph: hayro hands over what the page draws, in the
                // order it draws it, with no embedding levels to carry.
                rtl: false,
            })
            .collect();
        page.fragments.push(chapbook_paint::Fragment {
            rect,
            kind: chapbook_paint::FragmentKind::HiddenText(chapbook_paint::LineFragment {
                baseline: rect.size.h * 0.8,
                runs: vec![chapbook_paint::GlyphRun {
                    font: cosmic_text::fontdb::ID::dummy(),
                    font_size: rect.size.h,
                    font_weight: 400,
                    color: chapbook_core::Rgba::new(0, 0, 0, 0),
                    glyphs,
                }],
                decorations: Vec::new(),
                text: line.text.clone(),
                locator_start: first.offset,
            }),
            tag: 0,
        });
    }
}
