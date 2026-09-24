//! Golden-image tests: byte-exact PNG comparison against checked-in
//! references. Deterministic because rendering uses only the vendored
//! fixture fonts; pinned to one CI target in `ci.yml` (rasterization is
//! arch-stable but we don't rely on it across platforms).
//!
//! To regenerate after an intentional rendering change:
//! `UPDATE_RENDER_GOLDENS=1 cargo test -p chapbook-cli --test render_golden`

use std::path::PathBuf;

use chapbook_cli::commands;

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
}

fn check_golden_of(book_rel: &str, spine: usize, page: usize, golden_rel: &str) {
    check_golden_themed(
        book_rel,
        spine,
        page,
        golden_rel,
        chapbook_core::Theme::Light,
    )
}

fn check_golden_themed(
    book_rel: &str,
    spine: usize,
    page: usize,
    golden_rel: &str,
    theme: chapbook_core::Theme,
) {
    let golden = repo(golden_rel);
    let out = std::env::temp_dir().join(format!(
        "chapbook-render-{}-{spine}-{page}.png",
        golden.file_stem().unwrap().to_string_lossy()
    ));
    commands::render(&repo(book_rel), spine, page, &out, theme).unwrap();
    let rendered = std::fs::read(&out).unwrap();

    if std::env::var_os("UPDATE_RENDER_GOLDENS").is_some() {
        std::fs::write(&golden, &rendered).unwrap();
        return;
    }

    let expected = std::fs::read(&golden).unwrap_or_else(|_| {
        panic!("missing golden {golden_rel}; run with UPDATE_RENDER_GOLDENS=1")
    });
    assert!(
        rendered == expected,
        "rendered page (spine {spine}, page {page}) differs from {golden_rel}; \
         if the change is intentional, regenerate with UPDATE_RENDER_GOLDENS=1 \
         (rendered copy left at {})",
        out.display()
    );
}

#[test]
fn render_golden_chapter1_page0() {
    check_golden_of(
        "fixtures/epub/minimal.epub",
        0,
        0,
        "fixtures/render/minimal-s0p0.png",
    );
}

#[test]
fn render_golden_chapter2_page1() {
    check_golden_of(
        "fixtures/epub/minimal.epub",
        1,
        1,
        "fixtures/render/minimal-s1p1.png",
    );
}

/// Images, embedded fonts (incl. de-obfuscated), decorations, and hr in one
/// golden.
#[test]
fn render_golden_illustrated() {
    check_golden_of(
        "fixtures/epub/illustrated.epub",
        0,
        0,
        "fixtures/render/illustrated-s0p0.png",
    );
}

#[test]
fn render_golden_illustrated_dark() {
    check_golden_themed(
        "fixtures/epub/illustrated.epub",
        0,
        0,
        "fixtures/render/illustrated-s0p0-dark.png",
        chapbook_core::Theme::Dark,
    );
}

/// Native block MathML (STIX glyphs and fraction/radical rules), inline
/// math falling back to its alttext, an SVG `<img>`, an inline `<svg>`,
/// and an SVG-wrapped raster `<image>` in one golden.
#[test]
fn render_golden_foreign() {
    check_golden_of(
        "fixtures/epub/foreign.epub",
        0,
        0,
        "fixtures/render/foreign-s0p0.png",
    );
}

/// Hebrew, Arabic that has to join, and an LTR island inside RTL — the
/// only golden with a non-Latin script in it.
///
/// The assertions in `chapbook-reader/tests/bidi.rs` are sharper than a
/// PNG diff and say why they fail; this is here for the thing they cannot
/// check, which is whether it *looks* like Hebrew. Every defect that suite
/// found was invisible to the Latin corpus, and so is every future one.
#[test]
fn render_golden_bidi() {
    check_golden_of(
        "fixtures/epub/bidi.epub",
        0,
        0,
        "fixtures/render/bidi-s0p0.png",
    );
}

#[test]
fn render_golden_comic_page() {
    // Comic pages carry no text, so this golden is font-independent.
    check_golden_of(
        "fixtures/cbz/minimal.cbz",
        0,
        0,
        "fixtures/render/comic-s0.png",
    );
}

#[test]
fn render_golden_night_art_light() {
    check_golden_of(
        "fixtures/epub/night-art.epub",
        0,
        0,
        "fixtures/render/night-art-s0p0.png",
    );
}

/// The dark scheme is where the fixture earns its keep: its line art is
/// black on transparent, painted on white by the book and inverted under
/// `prefers-color-scheme: dark` — white lines on a black box, next to a
/// colour swatch that no rule touches.
#[test]
fn render_golden_night_art_dark() {
    check_golden_themed(
        "fixtures/epub/night-art.epub",
        0,
        0,
        "fixtures/render/night-art-s0p0-dark.png",
        chapbook_core::Theme::Dark,
    );
}
