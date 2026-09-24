//! SVG rendering behavior: `<img>` sources sniffed as SVG, inline `<svg>`
//! subtrees, and `<image>` href resolution through the fetch closure.

#![cfg(feature = "svg")]

use std::path::PathBuf;

use chapbook_core::{EdgeSizes, PageMetrics, ReadingSettings, Rotation, Size};
use chapbook_layout::dom::Document;
use chapbook_paint::FragmentKind;

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

fn styled(html: &str) -> Document {
    let mut doc = chapbook_layout::dom::parse_xhtml(html.as_bytes(), "test.xhtml").unwrap();
    let mut engine =
        chapbook_layout::cascade::StyleEngine::new(&page(), &ReadingSettings::default());
    engine.set_author_sheets(&[]);
    engine.style_document(&mut doc);
    doc
}

const RED_RECT_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="30"><rect width="40" height="30" fill="#ff0000"/></svg>"##;

#[test]
fn img_src_svg_is_rasterized_at_intrinsic_size() {
    let doc = styled(r#"<html><body><p><img src="fig.svg"/></p></body></html>"#);
    let fonts = fonts();
    let images = chapbook_layout::collect_images(&doc, Some(&fonts), |href| {
        (href == "fig.svg").then(|| RED_RECT_SVG.as_bytes().to_vec())
    });

    let mut stack = vec![doc.document_element().unwrap()];
    let mut tag = None;
    while let Some(id) = stack.pop() {
        if doc.is_html_element(id, &markup5ever::local_name!("img")) {
            tag = Some(chapbook_layout::dom::node_tag(id));
        }
        stack.extend(doc.node(id).children.iter().copied());
    }
    let stored = images.get(tag.expect("img node")).expect("decoded svg");
    assert_eq!((stored.width, stored.height), (40, 30));
    // Center pixel is the rect's opaque red.
    let center = ((15 * 40 + 20) * 4) as usize;
    assert_eq!(&stored.rgba[center..center + 4], &[255, 0, 0, 255]);
}

#[test]
fn inline_svg_becomes_a_replaced_image() {
    let doc = styled(
        r##"<html><body><p>before</p>
        <svg xmlns="http://www.w3.org/2000/svg" width="50" height="20">
          <rect width="50" height="20" fill="#0000ff"/>
          <text x="2" y="12">label</text>
        </svg>
        <p>after</p></body></html>"##,
    );
    let mut fonts = fonts();
    let mut images = chapbook_layout::collect_images(&doc, Some(&fonts), |_| None);
    let (svg_tag, xml) = doc.svg_sources().next().expect("captured inline svg");
    let svg_tag = chapbook_layout::dom::node_tag(svg_tag);
    assert!(xml.contains("<rect"), "subtree serialized: {xml}");
    assert_eq!(images.dims(svg_tag), Some((50, 20)));

    let layout = chapbook_layout::paginate(&doc, &[], &page(), &mut fonts, &mut images);
    let mut saw_image = false;
    for fragment in layout.pages.iter().flat_map(|p| p.fragments.iter()) {
        match &fragment.kind {
            FragmentKind::Image { resource } if *resource == svg_tag => saw_image = true,
            FragmentKind::Line(line) => {
                assert!(
                    !line.text.contains("label"),
                    "svg text must not flatten once the svg is replaced"
                );
            }
            _ => {}
        }
    }
    assert!(saw_image, "inline svg should paint as an image fragment");
}

#[test]
fn inline_svg_without_rasterizer_output_still_flattens() {
    // An SVG usvg cannot size (zero dimensions) stays on the flattening
    // path: its text keeps rendering as it did before the feature.
    let doc = styled(
        r#"<html><body>
        <svg xmlns="http://www.w3.org/2000/svg" width="0" height="0"><text>orphan</text></svg>
        </body></html>"#,
    );
    let mut fonts = fonts();
    let mut images = chapbook_layout::collect_images(&doc, Some(&fonts), |_| None);
    let layout = chapbook_layout::paginate(&doc, &[], &page(), &mut fonts, &mut images);
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
        texts.iter().any(|t| t.contains("orphan")),
        "unrasterizable svg should keep flattening its text: {texts:?}"
    );
}

#[test]
fn svg_image_href_resolves_through_fetch() {
    // The EPUB cover-page pattern: an inline svg wrapping an <image> whose
    // href lives in the container. The reference is itself an SVG so the
    // test needs no raster fixture bytes.
    let doc = styled(
        r#"<html><body>
        <svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink"
             width="40" height="30">
          <image xlink:href="cover.svg" width="40" height="30"/>
        </svg>
        </body></html>"#,
    );
    let fonts = fonts();
    let mut asked = Vec::new();
    let images = chapbook_layout::collect_images(&doc, Some(&fonts), |href| {
        asked.push(href.to_string());
        (href == "cover.svg").then(|| RED_RECT_SVG.as_bytes().to_vec())
    });
    assert_eq!(asked, vec!["cover.svg"], "href fetched exactly once");
    let (svg_id, _) = doc.svg_sources().next().expect("captured inline svg");
    let stored = images
        .get(chapbook_layout::dom::node_tag(svg_id))
        .expect("rasterized cover");
    let center = ((15 * 40 + 20) * 4) as usize;
    assert_eq!(&stored.rgba[center..center + 4], &[255, 0, 0, 255]);
}
