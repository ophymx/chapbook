//! `filter` on images. The colour functions compose into a derived image at
//! layout time, with the element's background under them, so a book's
//! dark-scheme `invert()` reaches the pixels while the display list keeps
//! its three dumb ops.

use std::path::PathBuf;

use chapbook_core::{EdgeSizes, PageMetrics, ReadingSettings, Rgba, Rotation, Size, Theme};
use chapbook_paint::{Fragment, FragmentKind, ImageStore};

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

/// The Standard Ebooks pattern: a white ground always, an inversion of the
/// whole element under a dark scheme.
const NIGHT_ART: &str = "img.art { background: #fff !important; } \
    @media all and (prefers-color-scheme: dark) { img.art { filter: invert(100%); } }";

const HTML: &str =
    r#"<html><body><p>before</p><img class="art" src="lines.png"/><p>after</p></body></html>"#;

/// Lay the page out at `theme` with a 2x2 image under the `<img>`: one
/// opaque black pixel and three transparent ones — line art in miniature.
fn laid_out(theme: Theme, css: &str) -> (Vec<Fragment>, u64, ImageStore) {
    let mut doc = chapbook_layout::dom::parse_xhtml(HTML.as_bytes(), "test.xhtml").unwrap();
    let settings = ReadingSettings {
        theme,
        ..ReadingSettings::default()
    };
    let sheets = vec![css.to_string()];
    let mut engine = chapbook_layout::cascade::StyleEngine::new(&page(), &settings);
    engine.set_author_sheets(&sheets);
    engine.style_document(&mut doc);

    let mut tag = None;
    let mut stack = vec![doc.document_element().unwrap()];
    while let Some(id) = stack.pop() {
        if doc.is_html_element(id, &markup5ever::local_name!("img")) {
            tag = Some(chapbook_layout::dom::node_tag(id));
        }
        stack.extend(doc.node(id).children.iter().copied());
    }
    let tag = tag.expect("an img");
    let mut images = ImageStore::default();
    images.insert(
        tag,
        2,
        2,
        vec![0, 0, 0, 255, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    );

    let mut fonts = fonts();
    let layout = chapbook_layout::paginate(&doc, &sheets, &page(), &mut fonts, &mut images);
    (layout.pages[0].fragments.clone(), tag, images)
}

fn image_index(fragments: &[Fragment], tag: u64) -> usize {
    fragments
        .iter()
        .position(|f| f.tag == tag && matches!(f.kind, FragmentKind::Image { .. }))
        .expect("an image fragment")
}

fn resource(fragments: &[Fragment], at: usize) -> u64 {
    match fragments[at].kind {
        FragmentKind::Image { resource } => resource,
        _ => unreachable!(),
    }
}

#[test]
fn in_the_light_the_ground_is_a_fill_and_the_pixels_are_the_books() {
    let (fragments, tag, _) = laid_out(Theme::Light, NIGHT_ART);
    let at = image_index(&fragments, tag);
    assert_eq!(
        resource(&fragments, at),
        tag,
        "no derived copy without a filter"
    );
    // The background is a box decoration under the image, same rect, same tag.
    let under = &fragments[at - 1];
    assert_eq!(under.tag, tag);
    assert_eq!(under.rect, fragments[at].rect);
    match &under.kind {
        FragmentKind::Box(decoration) => assert_eq!(decoration.background, Some(Rgba::WHITE)),
        other => panic!("expected the image's background under it, got {other:?}"),
    }
}

#[test]
fn in_the_dark_the_filter_composes_into_a_derived_image() {
    let (fragments, tag, images) = laid_out(Theme::Dark, NIGHT_ART);
    let at = image_index(&fragments, tag);
    let derived = resource(&fragments, at);
    assert_ne!(derived, tag, "the fragment points at a derived copy");
    assert!(
        !fragments
            .iter()
            .any(|f| f.tag == tag && matches!(f.kind, FragmentKind::Box(_))),
        "nothing is painted under a filtered image"
    );
    assert_eq!(images.dims(derived), Some((2, 2)));
    let pixels = &images.get(derived).unwrap().rgba;
    // The black line went white and the white ground went black: the
    // dark theme strips every background but an image's, so the book's
    // ground reached the filter and was inverted with the lines.
    assert_eq!(&pixels[0..4], &[255, 255, 255, 255]);
    assert_eq!(&pixels[4..8], &[0, 0, 0, 255]);
}

#[test]
fn a_plate_the_book_keeps_upright_in_the_dark_gets_its_ground_in_the_text_colour() {
    // Standard Ebooks' other branch: no inversion, and a ground the dark
    // theme leaves alone. The book asks for `currentColor` here but its
    // own `!important` white outranks that, as the cascade says it must;
    // either way the plate has a ground, which is the point. The image is
    // the book's pixels and the ground is a fill under it, at the image's
    // rect rather than across the measure.
    let css = "img.art { background: #fff !important; } \
        @media all and (prefers-color-scheme: dark) { img.art { background: currentColor; filter: none; } }";
    let (fragments, tag, _) = laid_out(Theme::Dark, css);
    let at = image_index(&fragments, tag);
    assert_eq!(resource(&fragments, at), tag, "no filter, no derived copy");
    let under = &fragments[at - 1];
    assert_eq!(under.rect, fragments[at].rect);
    match &under.kind {
        FragmentKind::Box(decoration) => assert_eq!(decoration.background, Some(Rgba::WHITE)),
        other => panic!("expected the plate's ground under it, got {other:?}"),
    }
}

#[test]
fn a_ground_the_theme_leaves_alone_composes_under_the_filter() {
    // The order of operations, on a theme that keeps author backgrounds:
    // white under the pixels first, then the inversion over both — so the
    // ground goes black and the line goes white, rather than the line
    // going white on a white box that a separate fill would have painted.
    let css = "img.art { background: #fff !important; filter: invert(100%); }";
    let (fragments, tag, images) = laid_out(Theme::Light, css);
    let at = image_index(&fragments, tag);
    let derived = resource(&fragments, at);
    assert_ne!(derived, tag);
    assert!(!fragments
        .iter()
        .any(|f| f.tag == tag && matches!(f.kind, FragmentKind::Box(_))));
    let pixels = &images.get(derived).unwrap().rgba;
    assert_eq!(&pixels[0..4], &[255, 255, 255, 255]);
    assert_eq!(&pixels[4..8], &[0, 0, 0, 255]);
}

#[test]
fn a_filter_the_engine_cannot_answer_leaves_the_image_as_it_was() {
    // `blur()` needs neighbours and is not attempted: the list reduces to
    // nothing, and the image paints the unfiltered way, ground and all.
    let css = "img.art { background: #fff !important; filter: blur(2px); }";
    let (fragments, tag, _) = laid_out(Theme::Light, css);
    let at = image_index(&fragments, tag);
    assert_eq!(resource(&fragments, at), tag);
    assert!(matches!(fragments[at - 1].kind, FragmentKind::Box(_)));
}

#[test]
fn the_theme_is_what_the_media_query_answers() {
    // The same sheet, the same book: only the reader's theme differs, and
    // that alone decides whether the inversion exists.
    let (light, tag, _) = laid_out(Theme::Light, NIGHT_ART);
    let (dark, _, _) = laid_out(Theme::Dark, NIGHT_ART);
    assert_eq!(resource(&light, image_index(&light, tag)), tag);
    assert_ne!(resource(&dark, image_index(&dark, tag)), tag);
}
