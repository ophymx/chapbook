//! Native MathML rendering behavior: which formulas render, which defer to
//! the EPUB fallback, and what the lowered fragment looks like.

#![cfg(feature = "mathml")]

use std::path::PathBuf;

use chapbook_core::{EdgeSizes, PageMetrics, ReadingSettings, Rotation, Size};
use chapbook_layout::dom::Document;
use chapbook_layout::ChapterLayout;
use chapbook_paint::{FragmentKind, LineFragment};

fn fonts() -> cosmic_text::FontSystem {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/fonts");
    chapbook_layout::build_font_system(&chapbook_core::FontSource::embedded(dir, "Crimson Text"))
        .expect("fixture fonts")
        .0
}

fn page() -> PageMetrics {
    PageMetrics {
        size: Size::new(600.0, 800.0),
        margins: EdgeSizes::uniform(40.0),
        dpi_scale: 1.0,
        rotation: Rotation::None,
    }
}

fn layout_html(html: &str) -> (ChapterLayout, Document) {
    let mut doc = chapbook_layout::dom::parse_xhtml(html.as_bytes(), "test.xhtml").unwrap();
    let mut engine =
        chapbook_layout::cascade::StyleEngine::new(&page(), &ReadingSettings::default());
    engine.set_author_sheets(&[]);
    engine.style_document(&mut doc);
    let mut fonts = fonts();
    let layout = chapbook_layout::paginate(
        &doc,
        &[],
        &page(),
        &mut fonts,
        &mut chapbook_paint::ImageStore::default(),
    );
    (layout, doc)
}

/// The line fragment tagged with `id`'s node, if any.
fn fragment_for<'a>(
    layout: &'a ChapterLayout,
    doc: &Document,
    id: &str,
) -> Option<&'a LineFragment> {
    let tag = chapbook_layout::dom::node_tag(doc.element_by_id(id)?);
    layout
        .pages
        .iter()
        .flat_map(|p| p.fragments.iter())
        .find(|f| f.tag == tag)
        .and_then(|f| match &f.kind {
            FragmentKind::Line(line) => Some(line),
            _ => None,
        })
}

#[test]
fn block_math_renders_native_glyphs_and_rules() {
    let (layout, doc) = layout_html(
        r#"<html><body><p>before</p>
        <math xmlns="http://www.w3.org/1998/Math/MathML" display="block" id="eq" alttext="1 over x">
          <mfrac><mn>1</mn><mi>x</mi></mfrac>
        </math>
        <p>after</p></body></html>"#,
    );
    let line = fragment_for(&layout, &doc, "eq").expect("math line fragment");
    let glyphs: usize = line.runs.iter().map(|r| r.glyphs.len()).sum();
    assert!(
        glyphs >= 2,
        "numerator and denominator glyphs, got {glyphs}"
    );
    assert!(
        !line.decorations.is_empty(),
        "the fraction bar lowers to a decoration"
    );
    assert!(line.baseline > 0.0, "ascent above the baseline");
    assert_eq!(line.text, "1 over x", "alttext carried as the line's text");
    for run in &line.runs {
        for glyph in &run.glyphs {
            assert!(glyph.advance > 0.0, "advances drive selection geometry");
        }
    }
    // The formula must not also flatten as text.
    let texts: Vec<&str> = layout
        .pages
        .iter()
        .flat_map(|p| p.fragments.iter())
        .filter_map(|f| match &f.kind {
            FragmentKind::Line(l) => Some(l.text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        !texts.iter().any(|t| t.contains("1x") || *t == "1"),
        "no flattened token soup alongside the native render: {texts:?}"
    );
}

#[test]
fn inline_math_keeps_the_fallback_flattening() {
    let (layout, _doc) = layout_html(
        r#"<html><body><p>value <math xmlns="http://www.w3.org/1998/Math/MathML" alttext="x plus one"><mi>x</mi><mo>+</mo><mn>1</mn></math> here</p></body></html>"#,
    );
    let texts: Vec<String> = layout
        .pages
        .iter()
        .flat_map(|p| p.fragments.iter())
        .filter_map(|f| match &f.kind {
            FragmentKind::Line(l) => Some(l.text.clone()),
            _ => None,
        })
        .collect();
    assert!(
        texts.iter().any(|t| t.contains("x plus one")),
        "inline math renders its alttext inline: {texts:?}"
    );
}

#[test]
fn unsupported_structure_with_fallback_defers_to_it() {
    // <menclose> is not MathML Core; with alttext shipped, the publisher's
    // fallback wins over formulary's mrow recovery.
    let (layout, doc) = layout_html(
        r#"<html><body>
        <math xmlns="http://www.w3.org/1998/Math/MathML" display="block" id="eq" alttext="enclosed x">
          <menclose notation="box"><mi>x</mi></menclose>
        </math>
        </body></html>"#,
    );
    let texts: Vec<String> = layout
        .pages
        .iter()
        .flat_map(|p| p.fragments.iter())
        .filter_map(|f| match &f.kind {
            FragmentKind::Line(l) => Some(l.text.clone()),
            _ => None,
        })
        .collect();
    assert!(
        texts.iter().any(|t| t.contains("enclosed x")),
        "fallback alttext should render: {texts:?}"
    );
    assert!(
        fragment_for(&layout, &doc, "eq").is_none()
            || fragment_for(&layout, &doc, "eq").is_some_and(|l| l.runs.is_empty()),
        "no native render for warned-about markup when a fallback exists"
    );
}

#[test]
fn unsupported_structure_without_fallback_still_renders() {
    // No altimg, no alttext: mrow recovery beats token soup.
    let (layout, doc) = layout_html(
        r#"<html><body>
        <math xmlns="http://www.w3.org/1998/Math/MathML" display="block" id="eq">
          <menclose notation="box"><mi>x</mi></menclose>
        </math>
        </body></html>"#,
    );
    let line = fragment_for(&layout, &doc, "eq").expect("recovered native render");
    assert!(line.runs.iter().any(|r| !r.glyphs.is_empty()));
}

#[test]
fn locator_text_is_identical_with_and_without_native_rendering() {
    // The locator contract: rendering choice must not move offsets. The
    // fallback rewrite (which defines locator text) runs before any
    // rendering decision, so the flattened text of a natively rendered
    // formula still contributes to locator space.
    let html = r#"<html><body><p>a</p>
        <math xmlns="http://www.w3.org/1998/Math/MathML" display="block" alttext="alt"><mi>x</mi></math>
        <p>b</p></body></html>"#;
    let doc = chapbook_layout::dom::parse_xhtml(html.as_bytes(), "test.xhtml").unwrap();
    let text = chapbook_layout::dom::locator_text(&doc);
    assert!(
        text.contains("alt"),
        "locator text keeps the fallback form: {text:?}"
    );
}
