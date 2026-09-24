//! Where the time actually goes, per pipeline stage.
//!
//!     cargo run --release -p chapbook-cli --example timings -- <book.epub> [chapters]
//!
//! Not a statistical benchmark: it runs the real stages over real chapters
//! and reports totals, so the answer to "what should we make faster" comes
//! from measurement rather than from which code looks slow.

use std::path::Path;
use std::time::{Duration, Instant};

use chapbook_core::{PageMetrics, PixelFormat, Publication, ReadingSettings, Size};
use chapbook_epub::Book;
use chapbook_layout::{cascade, dom};

#[derive(Default)]
struct Stage {
    name: &'static str,
    total: Duration,
    calls: u32,
}

impl Stage {
    fn new(name: &'static str) -> Self {
        Stage {
            name,
            total: Duration::ZERO,
            calls: 0,
        }
    }
    fn time<T>(&mut self, f: impl FnOnce() -> T) -> T {
        let t = Instant::now();
        let out = f();
        self.total += t.elapsed();
        self.calls += 1;
        out
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let book_path = args.next().unwrap_or_else(|| {
        eprintln!("usage: timings <book.epub> [chapters]");
        std::process::exit(2);
    });
    let want: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(12);

    // A device-shaped page, not a desktop window.
    let metrics = PageMetrics {
        size: Size::new(800.0, 480.0),
        ..PageMetrics::default()
    };
    let settings = ReadingSettings::default();

    let mut open = Stage::new("open book");
    let book = open.time(|| Book::open(Path::new(&book_path)))?;

    let fonts_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/fonts");
    let (mut fonts, _) = chapbook_layout::build_font_system(&chapbook_core::FontSource::embedded(
        &fonts_dir,
        "Crimson Text",
    ))?;

    let mut unit_bytes = Stage::new("unit_bytes (zip read)");
    let mut parse = Stage::new("parse_xhtml");
    let mut sheets_stage = Stage::new("collect stylesheets");
    let mut cascade = Stage::new("cascade (stylo)");
    let mut assets = Stage::new("assets (fonts+images)");
    let mut paginate = Stage::new("paginate (cosmic-text)");
    let mut render = Stage::new("render page 0 (tiny-skia)");
    let mut quantize = Stage::new("quantize 16-level+dither");
    let mut rotate = Stage::new("rotate none");

    let mut renderer = chapbook_render_tinyskia::Renderer::new();
    let (mut chapters, mut pages, mut bytes_total) = (0u32, 0usize, 0usize);
    // Session keeps this set and registers each @font-face family once per
    // book; a harness without it measures work the real reader never does.
    let mut registered: std::collections::HashSet<String> = std::collections::HashSet::new();

    for spine in 0..book.spine().len().min(want) {
        let href = match book.spine_item(spine) {
            Ok(item) => item.href.clone(),
            Err(_) => continue,
        };
        let Ok(raw) = unit_bytes.time(|| book.unit_bytes(spine)) else {
            continue;
        };
        // Skip cover/image-only units: they say nothing about text layout.
        if raw.len() < 2048 {
            continue;
        }
        bytes_total += raw.len();
        let Ok(mut doc) = parse.time(|| dom::parse_xhtml(&raw, &href)) else {
            continue;
        };

        let css: Vec<(String, String)> = sheets_stage.time(|| {
            let mut css = Vec::new();
            for source in doc.stylesheet_sources() {
                match source {
                    dom::StylesheetSource::Inline(text) => css.push((text, href.to_string())),
                    dom::StylesheetSource::External(rel) => {
                        if let Ok(res) = book.resource(&href, &rel) {
                            css.push((
                                String::from_utf8_lossy(&res.data).into_owned(),
                                chapbook_epub::resolve_href(&href, &rel),
                            ));
                        }
                    }
                }
            }
            css
        });
        let sheet_text: Vec<String> = css.iter().map(|(t, _)| t.clone()).collect();

        cascade.time(|| {
            let mut engine = cascade::StyleEngine::new(&metrics, &settings);
            engine.set_author_sheets(&sheet_text);
            engine.style_document(&mut doc);
        });

        let mut images = assets.time(|| {
            for face in chapbook_layout::extract_font_faces(&css) {
                if !registered.insert(face.family.clone()) {
                    continue;
                }
                for src in &face.sources {
                    if let Ok(res) = book.resource(&face.base, src) {
                        if chapbook_layout::register_font(&mut fonts, &face.family, res.data) {
                            break;
                        }
                    }
                }
            }
            chapbook_layout::collect_images(&doc, Some(&fonts), |h| {
                book.resource(&href, h).ok().map(|r| r.data)
            })
        });

        let layout = paginate.time(|| {
            chapbook_layout::paginate(&doc, &sheet_text, &metrics, &mut fonts, &mut images)
        });
        pages += layout.pages.len();
        chapters += 1;

        // One page through the rest of the pipeline, the way a shell does.
        if let Some(page) = layout.pages.first() {
            let list = chapbook_paint::build_display_list(page, chapbook_core::Rgba::WHITE, &[]);
            let (w, h) = (metrics.size.w as u32, metrics.size.h as u32);
            if let Some(mut pixmap) = chapbook_render_tinyskia::tiny_skia::Pixmap::new(w, h) {
                render.time(|| {
                    renderer.render(&list, &mut fonts, &images, 1.0, &mut pixmap.as_mut())
                });
                let grey = PixelFormat::Grey {
                    levels: 16,
                    dither: true,
                };
                // The whole page as one diffusion region: this stage is
                // named for the dithered cost, and mezzotint diffuses only
                // where it is told to.
                let whole = mezzotint::PanelRect::full(w, h);
                quantize.time(|| {
                    mezzotint::encode::quantize_for(pixmap.data_mut(), w, h, whole, grey, &[whole])
                });
                rotate.time(|| {
                    chapbook_paint::rotate(pixmap.data(), w, h, chapbook_core::Rotation::None)
                });
            }
        }
    }

    let stages = [
        &open,
        &unit_bytes,
        &parse,
        &sheets_stage,
        &cascade,
        &assets,
        &paginate,
        &render,
        &quantize,
        &rotate,
    ];
    let total: Duration = stages.iter().map(|s| s.total).sum();

    println!(
        "{} — {chapters} text chapters, {pages} pages, {:.1} KiB of XHTML, page {}x{}",
        book_path,
        bytes_total as f64 / 1024.0,
        metrics.size.w as u32,
        metrics.size.h as u32
    );
    println!(
        "\n  {:<28} {:>9}  {:>7}  {:>8}  {:>6}",
        "stage", "total", "calls", "per call", "share"
    );
    for s in stages {
        if s.calls == 0 {
            continue;
        }
        println!(
            "  {:<28} {:>7.1}ms  {:>7}  {:>6.2}ms  {:>5.1}%",
            s.name,
            s.total.as_secs_f64() * 1000.0,
            s.calls,
            s.total.as_secs_f64() * 1000.0 / f64::from(s.calls),
            s.total.as_secs_f64() / total.as_secs_f64() * 100.0
        );
    }
    println!("  {:<28} {:>7.1}ms", "TOTAL", total.as_secs_f64() * 1000.0);
    Ok(())
}
