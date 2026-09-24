//! Pagination behavior: forced breaks, break-inside avoidance,
//! widows/orphans, and the layout invariants (containment, text order,
//! conservation).

use std::path::PathBuf;

use chapbook_core::{EdgeSizes, PageMetrics, ReadingSettings, Rotation, Size};
use chapbook_layout::dom::Document;
use chapbook_layout::ChapterLayout;
use chapbook_paint::FragmentKind;

fn fonts() -> cosmic_text::FontSystem {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/fonts");
    chapbook_layout::build_font_system(&chapbook_core::FontSource::embedded(dir, "Crimson Text"))
        .expect("fixture fonts")
        .0
}

/// Page with room for exactly `lines` default lines (18px font, 1.5 line
/// height = 27px each).
fn page_for_lines(lines: u32) -> PageMetrics {
    PageMetrics {
        size: Size::new(600.0, lines as f32 * 27.0 + 80.0),
        margins: EdgeSizes::uniform(40.0),
        dpi_scale: 1.0,
        rotation: Rotation::None,
    }
}

fn layout_html(html: &str, css: &str, page: &PageMetrics) -> (ChapterLayout, Document) {
    let mut doc = chapbook_layout::dom::parse_xhtml(html.as_bytes(), "test.xhtml").unwrap();
    let css_sources = vec![css.to_string()];
    let mut engine = chapbook_layout::cascade::StyleEngine::new(page, &ReadingSettings::default());
    engine.set_author_sheets(&css_sources);
    engine.style_document(&mut doc);
    let mut fonts = fonts();
    let layout = chapbook_layout::paginate(
        &doc,
        &css_sources,
        page,
        &mut fonts,
        &mut chapbook_paint::ImageStore::default(),
    );
    (layout, doc)
}

fn line_texts_in_order(layout: &ChapterLayout) -> Vec<String> {
    layout
        .pages
        .iter()
        .flat_map(|p| p.fragments.iter())
        .filter_map(|f| match &f.kind {
            FragmentKind::Line(l) => Some(l.text.clone()),
            _ => None,
        })
        .collect()
}

fn lines_per_page(layout: &ChapterLayout) -> Vec<usize> {
    layout.pages.iter().map(|p| p.fragments.len()).collect()
}

/// N hard lines via <br>: line counts independent of font wrap behavior.
fn para_of_lines(n: usize, word: &str) -> String {
    let lines: Vec<String> = (0..n).map(|i| format!("{word} line {i}")).collect();
    format!("<p>{}</p>", lines.join("<br/>"))
}

#[test]
fn forced_break_before_starts_new_page() {
    let html = format!(
        "<html><body>{}{}</body></html>",
        para_of_lines(2, "first"),
        r#"<p class="pb">after the break</p>"#
    );
    let (layout, _) = layout_html(
        &html,
        ".pb { page-break-before: always; }",
        &page_for_lines(10),
    );
    assert_eq!(layout.pages.len(), 2);
    let texts = line_texts_in_order(&layout);
    assert!(texts.last().unwrap().contains("after the break"));
    assert_eq!(layout.pages[1].fragments.len(), 1);
}

#[test]
fn break_after_forces_page_for_next_block() {
    let html = "<html><body><h1>Title</h1><p>body text</p></body></html>";
    let (layout, _) = layout_html(html, "h1 { break-after: page; }", &page_for_lines(10));
    assert_eq!(layout.pages.len(), 2);
    assert!(matches!(&layout.pages[0].fragments[0].kind,
        FragmentKind::Line(l) if l.text == "Title"));
    assert!(matches!(&layout.pages[1].fragments[0].kind,
        FragmentKind::Line(l) if l.text == "body text"));
}

#[test]
fn long_paragraph_fills_and_breaks() {
    let html = format!("<html><body>{}</body></html>", para_of_lines(10, "w"));
    let (layout, _) = layout_html(&html, "p { margin: 0; }", &page_for_lines(4));
    assert_eq!(lines_per_page(&layout), vec![4, 4, 2]);
}

#[test]
fn orphans_move_paragraph_start_to_next_page() {
    // Page holds 4 lines. First paragraph takes 3, leaving room for 1 line
    // of the second — fewer than orphans(2), so it moves entirely.
    let html = format!(
        "<html><body>{}{}</body></html>",
        para_of_lines(3, "first"),
        para_of_lines(3, "second")
    );
    let (layout, _) = layout_html(&html, "p { margin: 0; }", &page_for_lines(4));
    assert_eq!(lines_per_page(&layout), vec![3, 3]);
}

#[test]
fn widows_pull_extra_line_to_next_page() {
    // Page holds 4 lines; 5-line paragraph would leave 1 widow — the break
    // moves up so the next page gets 2 lines.
    let html = format!("<html><body>{}</body></html>", para_of_lines(5, "w"));
    let (layout, _) = layout_html(&html, "p { margin: 0; }", &page_for_lines(4));
    assert_eq!(lines_per_page(&layout), vec![3, 2]);
}

#[test]
fn widows_orphans_custom_values_respected() {
    let html = format!("<html><body>{}</body></html>", para_of_lines(6, "w"));
    let (layout, _) = layout_html(
        &html,
        "p { margin: 0; widows: 3; orphans: 3; }",
        &page_for_lines(4),
    );
    // 6 lines, page of 4: plain fill would be 4+2, but widows:3 forces 3+3.
    assert_eq!(lines_per_page(&layout), vec![3, 3]);
}

#[test]
fn break_inside_avoid_moves_whole_block() {
    // First paragraph takes 2 of 4 lines; the second (3 lines,
    // break-inside: avoid) doesn't fit in the remaining 2 → whole block
    // moves to page 2 instead of splitting.
    let keep_para = format!(
        "<p class=\"keep\">{}</p>",
        (0..3)
            .map(|i| format!("keep line {i}"))
            .collect::<Vec<_>>()
            .join("<br/>")
    );
    let html = format!(
        "<html><body>{}{keep_para}</body></html>",
        para_of_lines(2, "first"),
    );
    let (layout, _) = layout_html(
        &html,
        "p { margin: 0; } .keep { break-inside: avoid; }",
        &page_for_lines(4),
    );
    assert_eq!(lines_per_page(&layout), vec![2, 3]);
}

#[test]
fn margins_discarded_at_page_top() {
    // Two paragraphs with big margins; the second starts a fresh page — its
    // top margin must not push it down the fresh page.
    let html = format!(
        "<html><body>{}{}</body></html>",
        para_of_lines(4, "first"),
        para_of_lines(2, "second")
    );
    let (layout, _) = layout_html(&html, "p { margin: 30px 0; }", &page_for_lines(4));
    assert_eq!(layout.pages.len(), 2);
    let first_on_page2 = &layout.pages[1].fragments[0];
    assert!(
        (first_on_page2.rect.origin.y - 40.0).abs() < 0.5,
        "expected content at page top, got y={}",
        first_on_page2.rect.origin.y
    );
}

// ---- Invariants over the fixture book ----

fn fixture_book_layout(spine: usize) -> (ChapterLayout, Document, String) {
    use chapbook_core::Publication;
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/epub/minimal.epub");
    let book = chapbook_epub::Book::open(&path).unwrap();
    let href = book.spine_item(spine).unwrap().href.clone();
    let bytes = book.unit_bytes(spine).unwrap();
    let mut doc = chapbook_layout::dom::parse_xhtml(&bytes, &href).unwrap();
    let css: Vec<String> = doc
        .stylesheet_sources()
        .iter()
        .filter_map(|s| match s {
            chapbook_layout::dom::StylesheetSource::Inline(t) => Some(t.clone()),
            chapbook_layout::dom::StylesheetSource::External(rel) => book
                .resource(&href, rel)
                .ok()
                .map(|r| String::from_utf8_lossy(&r.data).into_owned()),
        })
        .collect();
    let page = PageMetrics::default();
    let mut engine = chapbook_layout::cascade::StyleEngine::new(&page, &ReadingSettings::default());
    engine.set_author_sheets(&css);
    engine.style_document(&mut doc);
    let mut fonts = fonts();
    let layout = chapbook_layout::paginate(
        &doc,
        &css,
        &page,
        &mut fonts,
        &mut chapbook_paint::ImageStore::default(),
    );
    let display_text = chapbook_layout::dom::extract_text(&doc);
    (layout, doc, display_text)
}

fn strip_ws(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

fn is_subsequence(needle: &str, haystack: &str) -> bool {
    let mut hay = haystack.chars();
    needle.chars().all(|n| hay.by_ref().any(|h| h == n))
}

#[test]
fn invariants_on_fixture_chapters() {
    for spine in 0..2 {
        let (layout, _doc, display_text) = fixture_book_layout(spine);

        // 1. Every fragment inside the page content box.
        for (i, page) in layout.pages.iter().enumerate() {
            for frag in &page.fragments {
                assert!(
                    page.content.contains_rect(&frag.rect),
                    "spine {spine} page {i}: fragment {:?} outside content {:?}",
                    frag.rect,
                    page.content
                );
            }
        }

        // 2. Locator offsets never decrease across the fragment stream, and
        //    the char_map is monotonic.
        let mut last = 0u32;
        for page in &layout.pages {
            for frag in &page.fragments {
                if let FragmentKind::Line(l) = &frag.kind {
                    assert!(l.locator_start >= last, "locator order violated");
                    last = l.locator_start;
                }
            }
        }
        assert!(layout.char_map.windows(2).all(|w| w[0] <= w[1]));

        // 3. Conservation: every visible char of the extracted display text
        //    appears, in order, in the laid-out lines (which may add list
        //    markers but never drop or reorder text).
        let laid_out: String = layout
            .pages
            .iter()
            .flat_map(|p| &p.fragments)
            .filter_map(|f| match &f.kind {
                FragmentKind::Line(l) => Some(l.text.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            is_subsequence(&strip_ws(&display_text), &strip_ws(&laid_out)),
            "spine {spine}: text lost or reordered in layout"
        );
    }
}

// ---- Typography fidelity sweep ----

#[test]
fn text_indent_shifts_first_line_only() {
    let html = "<html><body><p>one two three four five six seven eight nine ten \
                eleven twelve thirteen fourteen fifteen sixteen seventeen</p></body></html>";
    let (layout, _) = layout_html(
        html,
        "p { margin: 0; text-indent: 36px; }",
        &page_for_lines(10),
    );
    let lines: Vec<&chapbook_paint::Fragment> = layout.pages[0].fragments.iter().collect();
    assert!(lines.len() >= 2, "paragraph must wrap");
    let first_x = lines[0].rect.origin.x;
    let second_x = lines[1].rect.origin.x;
    assert!(
        (first_x - (second_x + 36.0)).abs() < 0.5,
        "first line indented by 36px: first={first_x} second={second_x}"
    );
}

#[test]
fn generated_content_wraps_element_text() {
    let html = r#"<html><body><p class="q">quoted</p></body></html>"#;
    let (layout, _) = layout_html(
        html,
        r#".q::before { content: "« "; } .q::after { content: " »"; }"#,
        &page_for_lines(10),
    );
    let texts = line_texts_in_order(&layout);
    assert_eq!(texts, vec!["« quoted »"]);
}

#[test]
fn letter_spacing_widens_lines() {
    let html = "<html><body><p>letter spacing sample</p></body></html>";
    let (plain, _) = layout_html(html, "p { margin: 0; }", &page_for_lines(10));
    let (spaced, _) = layout_html(
        html,
        "p { margin: 0; letter-spacing: 2px; }",
        &page_for_lines(10),
    );
    let width = |l: &ChapterLayout| l.pages[0].fragments[0].rect.size.w;
    assert!(
        width(&spaced) > width(&plain) + 10.0,
        "tracking must widen the line: plain={} spaced={}",
        width(&plain),
        width(&spaced)
    );
}

#[test]
fn box_decoration_slices_across_pages() {
    // A bordered block tall enough to span two pages: one Box fragment per
    // page, top edge only on the first slice, bottom only on the last.
    let html = format!(
        "<html><body><div class=\"framed\">{}</div></body></html>",
        para_of_lines(6, "boxed")
    );
    let (layout, _) = layout_html(
        &html,
        ".framed { border: 2px solid #000; background-color: #eee; } p { margin: 0; }",
        &page_for_lines(4),
    );
    assert_eq!(layout.pages.len(), 2);
    let slices: Vec<&chapbook_paint::BoxDecoration> = layout
        .pages
        .iter()
        .flat_map(|p| &p.fragments)
        .filter_map(|f| match &f.kind {
            chapbook_paint::FragmentKind::Box(b) => Some(b),
            _ => None,
        })
        .collect();
    assert_eq!(slices.len(), 2, "one slice per page");
    assert!(slices[0].first_slice && !slices[0].last_slice);
    assert!(!slices[1].first_slice && slices[1].last_slice);
    assert_eq!(
        slices[0].background,
        Some(chapbook_core::Rgba::new(0xee, 0xee, 0xee, 255))
    );
    assert_eq!(slices[0].border_widths.top, 2.0);

    // The background paints under the page's text: Box fragment comes first.
    assert!(matches!(
        layout.pages[0].fragments[0].kind,
        chapbook_paint::FragmentKind::Box(_)
    ));
    assert!(matches!(
        layout.pages[1].fragments[0].kind,
        chapbook_paint::FragmentKind::Box(_)
    ));
}

#[test]
fn undecorated_blocks_emit_no_box_fragments() {
    let html = format!("<html><body>{}</body></html>", para_of_lines(2, "plain"));
    let (layout, _) = layout_html(&html, "p { margin: 0; }", &page_for_lines(4));
    assert!(layout
        .pages
        .iter()
        .flat_map(|p| &p.fragments)
        .all(|f| !matches!(f.kind, chapbook_paint::FragmentKind::Box(_))));
}

// ---- Tables ----

fn cell_lines(layout: &ChapterLayout) -> Vec<(f32, f32, String)> {
    layout
        .pages
        .iter()
        .flat_map(|p| &p.fragments)
        .filter_map(|f| match &f.kind {
            FragmentKind::Line(l) => Some((f.rect.origin.x, f.rect.origin.y, l.text.clone())),
            _ => None,
        })
        .collect()
}

#[test]
fn table_columns_lay_out_side_by_side() {
    let html = r#"<html><body><table>
        <tr><td>a</td><td>considerably longer cell content here</td></tr>
        <tr><td>b</td><td>short</td></tr>
    </table></body></html>"#;
    let (layout, _) = layout_html(html, "td { padding: 2px; }", &page_for_lines(10));
    let lines = cell_lines(&layout);
    let a = lines.iter().find(|(_, _, t)| t == "a").unwrap();
    let long = lines
        .iter()
        .find(|(_, _, t)| t.starts_with("considerably"))
        .unwrap();
    let b = lines.iter().find(|(_, _, t)| t == "b").unwrap();
    // Same row: same y, different x; column B right of A.
    assert!((a.1 - long.1).abs() < 0.5, "row cells align vertically");
    assert!(long.0 > a.0 + 5.0, "second column right of first");
    // Column x stable across rows.
    assert!((a.0 - b.0).abs() < 0.5, "first column x consistent");
    // Long content did not wrap: column got its max-content width.
    assert_eq!(
        lines
            .iter()
            .filter(|(_, _, t)| t.contains("longer cell"))
            .count(),
        1
    );
}

#[test]
fn table_narrow_page_wraps_long_column() {
    let html = r#"<html><body><table>
        <tr><td>key</td><td>a rather long value that cannot fit on one narrow line at all</td></tr>
    </table></body></html>"#;
    let mut page = page_for_lines(10);
    page.size.w = 300.0;
    let (layout, _) = layout_html(html, "", &page);
    let lines = cell_lines(&layout);
    let value_lines = lines.iter().filter(|(_, _, t)| t != "key").count();
    assert!(
        value_lines >= 2,
        "long column must wrap when space is short"
    );
    // Everything stays inside the content box.
    for page in &layout.pages {
        for frag in &page.fragments {
            assert!(page.content.contains_rect(&frag.rect), "{:?}", frag.rect);
        }
    }
}

#[test]
fn table_colspan_spans_columns() {
    let html = r#"<html><body><table>
        <tr><th colspan="2">Spanning Header</th></tr>
        <tr><td>left cell text</td><td>right cell text</td></tr>
    </table></body></html>"#;
    let (layout, _) = layout_html(
        html,
        "th, td { border: 1px solid #000; padding: 2px; }",
        &page_for_lines(10),
    );
    // Box fragments per cell: 3 (one spanning + two normal).
    let boxes: Vec<&chapbook_paint::Fragment> = layout
        .pages
        .iter()
        .flat_map(|p| &p.fragments)
        .filter(|f| matches!(f.kind, FragmentKind::Box(_)))
        .collect();
    assert_eq!(boxes.len(), 3);
    let spanning = boxes[0];
    let left = boxes[1];
    let right = boxes[2];
    let spanned = right.rect.max_x() - left.rect.origin.x;
    assert!(
        (spanning.rect.size.w - spanned).abs() < 1.0,
        "header spans both columns: header={} cells={spanned}",
        spanning.rect.size.w
    );
}

#[test]
fn table_rows_break_atomically_across_pages() {
    let mut rows = String::new();
    for i in 0..8 {
        rows.push_str(&format!("<tr><td>row {i} cell</td></tr>"));
    }
    let html = format!("<html><body><table>{rows}</table></body></html>");
    let (layout, _) = layout_html(&html, "td { padding: 0; }", &page_for_lines(4));
    assert!(layout.pages.len() >= 2, "table must paginate");
    // No row's text is split across pages: each "row N cell" line appears
    // exactly once, and y positions restart near the top on later pages.
    let lines = cell_lines(&layout);
    assert_eq!(lines.len(), 8);
    let first_on_page2 = &layout.pages[1].fragments[0];
    assert!(first_on_page2.rect.origin.y < 40.0 + 30.0 + 5.0);
}

#[test]
fn table_caption_and_text_order_preserved() {
    let html = r#"<html><body>
      <table>
        <caption>Table One</caption>
        <tr><td>alpha</td><td>beta</td></tr>
        <tr><td>gamma</td><td>delta</td></tr>
      </table></body></html>"#;
    let (layout, _) = layout_html(html, "", &page_for_lines(10));
    let texts: Vec<String> = cell_lines(&layout).into_iter().map(|(_, _, t)| t).collect();
    assert_eq!(texts[0], "Table One", "caption first");
    for word in ["alpha", "beta", "gamma", "delta"] {
        assert!(texts.iter().any(|t| t == word), "missing {word}");
    }
    // Locator monotonicity across the whole fragment stream still holds.
    let mut last = 0u32;
    for page in &layout.pages {
        for frag in &page.fragments {
            if let FragmentKind::Line(l) = &frag.kind {
                assert!(l.locator_start >= last);
                last = l.locator_start;
            }
        }
    }
}

// ---- Layout batch 2: keeps, @media, table polish, justified indents ----

#[test]
fn keep_with_next_migrates_heading_to_new_page() {
    // Filler leaves exactly one line of room; the heading lands there, and
    // its paragraph (2+ lines) can't follow — both must move to page 2.
    let html = format!(
        "<html><body>{}<h3>Kept Heading</h3>{}</body></html>",
        para_of_lines(3, "filler"),
        para_of_lines(3, "body")
    );
    let (layout, _) = layout_html(
        &html,
        "p { margin: 0; } h3 { margin: 0; font-size: 18px; break-after: avoid; }",
        &page_for_lines(4),
    );
    assert_eq!(layout.pages.len(), 2);
    let page2_texts: Vec<String> = layout.pages[1]
        .fragments
        .iter()
        .filter_map(|f| match &f.kind {
            FragmentKind::Line(l) => Some(l.text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        page2_texts.first().map(String::as_str),
        Some("Kept Heading"),
        "heading must migrate with its paragraph: page2 = {page2_texts:?}"
    );
    // And nothing of the heading remains on page 1.
    assert!(layout.pages[0]
        .fragments
        .iter()
        .all(|f| !matches!(&f.kind, FragmentKind::Line(l) if l.text == "Kept Heading")));
}

#[test]
fn break_before_avoid_is_equivalent_keep() {
    let html = format!(
        "<html><body>{}<h3>Kept Too</h3>{}</body></html>",
        para_of_lines(3, "filler"),
        para_of_lines(3, "body")
    );
    let (layout, _) = layout_html(
        &html,
        "p { margin: 0; } h3 { margin: 0; font-size: 18px; } p + h3 + p { break-before: avoid; }",
        &page_for_lines(4),
    );
    // The paragraph after the heading declares break-before: avoid.
    let page2_first = layout.pages.get(1).and_then(|p| {
        p.fragments.iter().find_map(|f| match &f.kind {
            FragmentKind::Line(l) => Some(l.text.clone()),
            _ => None,
        })
    });
    assert_eq!(page2_first.as_deref(), Some("Kept Too"));
}

#[test]
fn media_screen_break_rules_apply_print_ones_do_not() {
    let html = format!(
        "<html><body>{}{}</body></html>",
        para_of_lines(1, "first"),
        r#"<p class="scr">screen-broken</p><p class="prn">print-broken</p>"#
    );
    let css = r#"
        p { margin: 0; }
        @media screen { .scr { page-break-before: always; } }
        @media print { .prn { page-break-before: always; } }
    "#;
    let (layout, _) = layout_html(&html, css, &page_for_lines(10));
    // screen rule honored → page break before .scr; print rule ignored →
    // .prn flows right after on the same page.
    assert_eq!(layout.pages.len(), 2);
    let page2: Vec<String> = layout.pages[1]
        .fragments
        .iter()
        .filter_map(|f| match &f.kind {
            FragmentKind::Line(l) => Some(l.text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(page2, vec!["screen-broken", "print-broken"]);
}

#[test]
fn table_cell_vertical_align_middle() {
    let html = r#"<html><body><table><tr>
        <td class="mid">short</td>
        <td>a much longer cell<br/>with a second line<br/>and a third line</td>
    </tr></table></body></html>"#;
    let (layout, _) = layout_html(
        html,
        ".mid { vertical-align: middle; } td { padding: 0; }",
        &page_for_lines(10),
    );
    let lines = cell_lines(&layout);
    let short = lines.iter().find(|(_, _, t)| t == "short").unwrap();
    let first_long = lines
        .iter()
        .find(|(_, _, t)| t.starts_with("a much longer"))
        .unwrap();
    assert!(
        short.1 > first_long.1 + 10.0,
        "middle-aligned cell sits below the top: short_y={} long_y={}",
        short.1,
        first_long.1
    );
}

#[test]
fn table_header_repeats_on_continuation_pages() {
    let mut rows = String::from("<tr><th>Col A</th><th>Col B</th></tr>");
    for i in 0..8 {
        rows.push_str(&format!("<tr><td>row {i}</td><td>value {i}</td></tr>"));
    }
    let html = format!("<html><body><table>{rows}</table></body></html>");
    let (layout, _) = layout_html(&html, "td, th { padding: 0; }", &page_for_lines(5));
    assert!(layout.pages.len() >= 2);
    for (i, page) in layout.pages.iter().enumerate() {
        let first_text = page.fragments.iter().find_map(|f| match &f.kind {
            FragmentKind::Line(l) => Some(l.text.clone()),
            _ => None,
        });
        assert_eq!(
            first_text.as_deref(),
            Some("Col A"),
            "page {i} must open with the repeated header"
        );
    }
}

#[test]
fn indented_justified_first_line_fills_measure() {
    let html = "<html><body><p>words that repeat words that repeat words that \
                repeat words that repeat words that repeat words that repeat \
                words that repeat words that repeat</p></body></html>";
    let (layout, _) = layout_html(
        html,
        "p { margin: 0; text-indent: 40px; text-align: justify; }",
        &page_for_lines(10),
    );
    let lines: Vec<&chapbook_paint::Fragment> = layout.pages[0].fragments.iter().collect();
    assert!(lines.len() >= 3, "needs several lines");
    // Content width is 520; first line starts at 40 + 40 indent and must
    // reach the right edge like the justified middle lines do.
    let first = &lines[0];
    let middle = &lines[1];
    let first_right = first.rect.origin.x + first.rect.size.w;
    let middle_right = middle.rect.origin.x + middle.rect.size.w;
    assert!(
        (first_right - middle_right).abs() < 1.0,
        "justified first line must fill the measure: first_right={first_right} middle_right={middle_right}"
    );
}

// ---- Floats ----

/// Layout with a synthetic 100×54 image bound to every `<img>` in the doc
/// (54px = two 27px lines tall).
fn layout_html_with_image(
    html: &str,
    css: &str,
    page: &PageMetrics,
    dims: (u32, u32),
) -> (ChapterLayout, Document) {
    let mut doc = chapbook_layout::dom::parse_xhtml(html.as_bytes(), "test.xhtml").unwrap();
    let css_sources = vec![css.to_string()];
    let mut engine = chapbook_layout::cascade::StyleEngine::new(page, &ReadingSettings::default());
    engine.set_author_sheets(&css_sources);
    engine.style_document(&mut doc);
    let mut images = chapbook_paint::ImageStore::default();
    let mut stack = vec![doc.document_element().unwrap()];
    while let Some(id) = stack.pop() {
        if doc.is_html_element(id, &markup5ever::local_name!("img")) {
            images.insert(
                chapbook_layout::dom::node_tag(id),
                dims.0,
                dims.1,
                vec![0u8; (dims.0 * dims.1 * 4) as usize],
            );
        }
        stack.extend(doc.node(id).children.iter().copied());
    }
    let mut fonts = fonts();
    let layout = chapbook_layout::paginate(&doc, &css_sources, page, &mut fonts, &mut images);
    (layout, doc)
}

fn line_frags(layout: &ChapterLayout) -> Vec<(f32, f32, f32, String)> {
    layout.pages[0]
        .fragments
        .iter()
        .filter_map(|f| match &f.kind {
            FragmentKind::Line(l) => Some((
                f.rect.origin.x,
                f.rect.origin.y,
                f.rect.size.w,
                l.text.clone(),
            )),
            _ => None,
        })
        .collect()
}

#[test]
fn float_left_image_wraps_text_beside_then_below() {
    let words = "wrap ".repeat(60);
    let html = format!("<html><body><p><img src=\"x.png\"/>{words}</p></body></html>");
    let (layout, _) = layout_html_with_image(
        &html,
        "p { margin: 0; } img { float: left; margin: 0 10px 10px 0; }",
        &page_for_lines(12),
        (100, 54),
    );
    // The image sits at the left content edge, top of page.
    let img = layout.pages[0]
        .fragments
        .iter()
        .find(|f| matches!(f.kind, FragmentKind::Image { .. }))
        .expect("float image placed");
    assert_eq!(img.rect.origin.x, 40.0);
    assert_eq!(img.rect.origin.y, 40.0);
    // Lines beside the float are inset by its band (100 + 10 margin) and
    // shortened; lines below return to the full measure at x=40.
    let lines = line_frags(&layout);
    assert!(lines.len() >= 4);
    let beside: Vec<_> = lines.iter().filter(|l| l.0 > 145.0).collect();
    let below: Vec<_> = lines.iter().filter(|l| l.0 < 45.0).collect();
    assert!(
        beside.len() >= 2,
        "expected shortened lines beside the float: {lines:?}"
    );
    assert!(!below.is_empty(), "expected full lines below the float");
    for l in &beside {
        assert!(l.2 <= 520.0 - 110.0 + 0.5, "beside line too wide: {l:?}");
        assert!(
            l.1 + 27.0 <= 40.0 + 64.0 + 0.5,
            "beside line beyond band: {l:?}"
        );
    }
    // Every below-line starts after the band expires.
    for l in &below {
        assert!(l.1 >= 40.0 + 64.0 - 0.5, "full line overlaps float: {l:?}");
    }
}

#[test]
fn float_right_image_keeps_text_at_left_edge() {
    let words = "wrap ".repeat(60);
    let html = format!("<html><body><p><img src=\"x.png\"/>{words}</p></body></html>");
    let (layout, _) = layout_html_with_image(
        &html,
        "p { margin: 0; } img { float: right; margin: 0 0 10px 10px; }",
        &page_for_lines(12),
        (100, 54),
    );
    let img = layout.pages[0]
        .fragments
        .iter()
        .find(|f| matches!(f.kind, FragmentKind::Image { .. }))
        .expect("float image placed");
    // Right edge: 40 + 520 - 100.
    assert_eq!(img.rect.origin.x, 460.0);
    let lines = line_frags(&layout);
    // All text stays at the left edge; beside-lines are just shortened.
    for l in &lines {
        assert!((l.0 - 40.0).abs() < 0.5, "line not at left edge: {l:?}");
    }
    assert!(
        lines
            .iter()
            .any(|l| l.1 < 40.0 + 54.0 && l.2 <= 520.0 - 110.0 + 0.5),
        "expected shortened lines beside the right float: {lines:?}"
    );
    assert!(
        lines.iter().any(|l| l.2 > 450.0),
        "expected full-measure lines below the float: {lines:?}"
    );
}

#[test]
fn clear_starts_below_float() {
    let html = "<html><body>\
                <p><img src=\"x.png\"/>short text beside</p>\
                <p class=\"c\">cleared paragraph</p>\
                </body></html>";
    let (layout, _) = layout_html_with_image(
        html,
        "p { margin: 0; } img { float: left; margin: 0 10px 10px 0; } .c { clear: left; }",
        &page_for_lines(12),
        (100, 54),
    );
    let lines = line_frags(&layout);
    let cleared = lines
        .iter()
        .find(|l| l.3.contains("cleared"))
        .expect("cleared paragraph present");
    // Band bottom = 54 + 10 margin-bottom = 64 below the content top (40).
    assert!(
        cleared.1 >= 40.0 + 64.0 - 0.5,
        "clear must move below the float: {cleared:?}"
    );
    assert!((cleared.0 - 40.0).abs() < 0.5);
}

// ---- Hyphenation ----

#[test]
fn hyphens_auto_breaks_words_with_visible_hyphen() {
    let words = "extraordinary consideration photography ".repeat(8);
    let html = format!("<html><body><p>{words}</p></body></html>");
    let narrow = PageMetrics {
        size: Size::new(240.0, 500.0),
        margins: EdgeSizes::uniform(40.0),
        dpi_scale: 1.0,
        rotation: Rotation::None,
    };
    let (with, _) = layout_html(&html, "p { margin: 0; hyphens: auto; }", &narrow);
    let (without, _) = layout_html(&html, "p { margin: 0; }", &narrow);
    let hyphen_lines = line_texts_in_order(&with)
        .iter()
        .filter(|t| t.ends_with('-'))
        .count();
    assert!(hyphen_lines >= 2, "expected hyphenated line ends");
    assert!(
        line_texts_in_order(&without)
            .iter()
            .all(|t| !t.ends_with('-')),
        "control must not hyphenate"
    );
    // Hyphenation must not lose text.
    let strip = |l: &ChapterLayout| {
        line_texts_in_order(l)
            .join(" ")
            .replace('-', "")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join("")
    };
    assert_eq!(strip(&with), strip(&without));
    // No soft hyphens leak into reported line text.
    assert!(line_texts_in_order(&with)
        .iter()
        .all(|t| !t.contains('\u{AD}')));
}

#[test]
fn authored_soft_hyphen_respected_without_hyphens_auto() {
    // The author's soft hyphen is the only break point cosmic-text gets in
    // this overlong word; the visible hyphen must appear at the break.
    let html = "<html><body><p>inter\u{AD}nationalizationism</p></body></html>";
    let narrow = PageMetrics {
        size: Size::new(150.0, 500.0),
        margins: EdgeSizes::uniform(40.0),
        dpi_scale: 1.0,
        rotation: Rotation::None,
    };
    let (layout, _) = layout_html(html, "p { margin: 0; }", &narrow);
    let texts = line_texts_in_order(&layout);
    assert_eq!(
        texts.first().map(String::as_str),
        Some("inter-"),
        "{texts:?}"
    );
}

#[test]
fn hyphenated_justified_line_holds_the_measure() {
    let words = "extraordinary consideration photography magnificent ".repeat(6);
    let html = format!("<html><body><p>{words}</p></body></html>");
    let narrow = PageMetrics {
        size: Size::new(260.0, 600.0),
        margins: EdgeSizes::uniform(40.0),
        dpi_scale: 1.0,
        rotation: Rotation::None,
    };
    let (layout, _) = layout_html(
        &html,
        "p { margin: 0; hyphens: auto; text-align: justify; }",
        &narrow,
    );
    let mut checked = 0;
    for page in &layout.pages {
        for f in &page.fragments {
            let FragmentKind::Line(l) = &f.kind else {
                continue;
            };
            if l.text.ends_with('-') && l.text.contains(' ') {
                let right = f.rect.origin.x + f.rect.size.w;
                assert!(
                    (right - (40.0 + 180.0)).abs() < 1.0,
                    "hyphenated justified line must end at the measure: {:?} right={right}",
                    l.text
                );
                checked += 1;
            }
        }
    }
    assert!(checked >= 1, "no hyphenated justified lines found");
}

// ---- Rowspan ----

/// (x, y, w, h) of box-decoration fragments.
type BoxRects = Vec<(f32, f32, f32, f32)>;
/// (x, y, text) of line fragments.
type LinePositions = Vec<(f32, f32, String)>;

fn boxes_and_lines(layout: &ChapterLayout, page: usize) -> (BoxRects, LinePositions) {
    let mut boxes = Vec::new();
    let mut lines = Vec::new();
    for f in &layout.pages[page].fragments {
        match &f.kind {
            FragmentKind::Box(_) => boxes.push((
                f.rect.origin.x,
                f.rect.origin.y,
                f.rect.size.w,
                f.rect.size.h,
            )),
            FragmentKind::Line(l) => lines.push((f.rect.origin.x, f.rect.origin.y, l.text.clone())),
            _ => {}
        }
    }
    (boxes, lines)
}

#[test]
fn rowspan_cell_occupies_column_across_rows() {
    let html = "<html><body><table>\
                <tr><td rowspan=\"2\">Span</td><td>TopRight</td></tr>\
                <tr><td>BottomRight</td></tr>\
                </table></body></html>";
    let (layout, _) = layout_html(
        html,
        "td { border: 1px solid black; padding: 0; }",
        &page_for_lines(10),
    );
    let (boxes, lines) = boxes_and_lines(&layout, 0);
    let find = |t: &str| {
        lines
            .iter()
            .find(|l| l.2.contains(t))
            .unwrap_or_else(|| panic!("missing line {t}"))
            .clone()
    };
    let span = find("Span");
    let top = find("TopRight");
    let bottom = find("BottomRight");
    // The second row's only cell sits in the SECOND column, not the first.
    assert!(
        (bottom.0 - top.0).abs() < 0.5,
        "bottom-right cell must align under top-right: top={top:?} bottom={bottom:?}"
    );
    assert!(bottom.0 > span.0 + 10.0);
    assert!(bottom.1 > top.1, "second row is below the first");
    // The spanning cell's border box covers both rows: it is the tallest.
    let span_box = boxes
        .iter()
        .filter(|b| b.0 < top.0) // first column
        .cloned()
        .fold(
            (0.0, 0.0, 0.0, 0.0f32),
            |a, b| if b.3 > a.3 { b } else { a },
        );
    let right_box_h = boxes
        .iter()
        .filter(|b| b.0 >= top.0 - 2.0)
        .map(|b| b.3)
        .fold(0.0f32, f32::max);
    // Two 27px rows + 2px border-spacing between them + borders.
    assert!(
        span_box.3 >= right_box_h * 1.8,
        "rowspan box must span both rows: span_h={} single_h={right_box_h}",
        span_box.3
    );
}

#[test]
fn rowspan_content_grows_spanned_rows() {
    // The spanning cell holds 4 hard lines; each spanned row alone holds 1.
    let html = "<html><body><table>\
                <tr><td rowspan=\"2\">a<br/>b<br/>c<br/>d</td><td>one</td></tr>\
                <tr><td>two</td></tr>\
                </table></body></html>";
    let (layout, _) = layout_html(
        html,
        "td { padding: 0; border: 1px solid black; }",
        &page_for_lines(10),
    );
    let (_, lines) = boxes_and_lines(&layout, 0);
    let find = |t: &str| lines.iter().find(|l| l.2 == t).unwrap().clone();
    let one = find("one");
    let two = find("two");
    // 4 lines (108px) split over two rows: the second row starts ~2 lines
    // down, not 1 line down.
    let gap = two.1 - one.1;
    assert!(
        gap > 27.0 * 1.5,
        "spanned rows must grow to hold the tall cell: gap={gap}"
    );
    // All four spanning-cell lines are present, in order, in column 1.
    let col1: Vec<_> = lines.iter().filter(|l| l.0 < one.0).collect();
    assert_eq!(
        col1.iter().map(|l| l.2.as_str()).collect::<Vec<_>>(),
        vec!["a", "b", "c", "d"]
    );
}

#[test]
fn rowspan_band_paginates_atomically() {
    // Page holds 4 lines; 3 lines of filler leave 1 line of room. The
    // 2-row band tied by the rowspan needs 2 lines and must move whole.
    let html = format!(
        "<html><body>{}<table>\
         <tr><td rowspan=\"2\">Span</td><td>RowOne</td></tr>\
         <tr><td>RowTwo</td></tr>\
         </table></body></html>",
        para_of_lines(3, "filler")
    );
    let (layout, _) = layout_html(
        &html,
        "p { margin: 0; } table { margin: 0; } td { padding: 0; }",
        &page_for_lines(4),
    );
    assert_eq!(layout.pages.len(), 2);
    let (_, page0_lines) = boxes_and_lines(&layout, 0);
    let (_, page1_lines) = boxes_and_lines(&layout, 1);
    assert!(
        page0_lines
            .iter()
            .all(|l| !l.2.contains("Row") && !l.2.contains("Span")),
        "table band must not start on page 0: {page0_lines:?}"
    );
    for t in ["Span", "RowOne", "RowTwo"] {
        assert!(
            page1_lines.iter().any(|l| l.2.contains(t)),
            "{t} must be on page 1: {page1_lines:?}"
        );
    }
}

#[test]
fn rowspan_zero_spans_to_last_row() {
    let html = "<html><body><table>\
                <tr><td rowspan=\"0\">Span</td><td>r1</td></tr>\
                <tr><td>r2</td></tr>\
                <tr><td>r3</td></tr>\
                </table></body></html>";
    let (layout, _) = layout_html(
        html,
        "td { border: 1px solid black; padding: 0; }",
        &page_for_lines(10),
    );
    let (_, lines) = boxes_and_lines(&layout, 0);
    let r1 = lines.iter().find(|l| l.2 == "r1").unwrap().clone();
    // Every body row's cell lands in column 2.
    for t in ["r2", "r3"] {
        let l = lines.iter().find(|l| l.2 == t).unwrap();
        assert!(
            (l.0 - r1.0).abs() < 0.5,
            "{t} must sit in the second column: {l:?} vs r1 {r1:?}"
        );
    }
}

// ---- Floated blocks, indent beside floats, content items, decoration
// ---- stretch ----

#[test]
fn floated_block_with_width_wraps_text() {
    let words = "wrap ".repeat(60);
    let html = format!(
        "<html><body>\
         <aside>Pull quote line one<br/>and line two</aside>\
         <p>{words}</p></body></html>"
    );
    let (layout, _) = layout_html(
        &html,
        "p { margin: 0; } aside { float: right; width: 150px; margin: 0 0 10px 10px; \
         border: 1px solid black; padding: 4px; }",
        &page_for_lines(12),
    );
    let (boxes, lines) = boxes_and_lines(&layout, 0);
    // The aside's border box sits at the right content edge.
    // Margin box = 10 + 1 + 4 + 150 + 4 + 1 = 170; box at 40 + 520 - 160.
    let aside_box = boxes.first().expect("aside box decoration");
    assert!(
        aside_box.0 > 380.0,
        "aside must sit at the right edge: {aside_box:?}"
    );
    // Its content lines line up inside it.
    let quote = lines.iter().find(|l| l.2.contains("Pull quote")).unwrap();
    assert!(quote.0 > aside_box.0);
    // Body lines beside the float are shortened; below they are full.
    let body: Vec<_> = lines.iter().filter(|l| l.2.starts_with("wrap")).collect();
    assert!(body.iter().any(|l| {
        let (_, layout_lines) = (0, l);
        let _ = layout_lines;
        l.1 < quote.1 + 54.0
    }));
    let beside: Vec<_> = body
        .iter()
        .filter(|l| l.1 < aside_box.1 + aside_box.3)
        .collect();
    let below: Vec<_> = body
        .iter()
        .filter(|l| l.1 > aside_box.1 + aside_box.3 + 10.0)
        .collect();
    assert!(!beside.is_empty(), "lines beside the aside: {body:?}");
    assert!(!below.is_empty(), "lines below the aside");
    for l in &beside {
        assert!(
            l.0 < 45.0,
            "beside lines stay at the left edge for a right float: {l:?}"
        );
    }
    assert!(
        below.iter().any(|l| l.0 < 45.0),
        "below lines return to the full measure"
    );
}

#[test]
fn text_indent_applies_beside_float() {
    let words = "indent wrap words ".repeat(20);
    let html = format!("<html><body><p><img src=\"x.png\"/>{words}</p></body></html>");
    let (layout, _) = layout_html_with_image(
        &html,
        "p { margin: 0; text-indent: 30px; } img { float: left; margin: 0 10px 10px 0; }",
        &page_for_lines(12),
        (100, 54),
    );
    let lines = line_frags(&layout);
    // Band inset = 110. First line: 40 + 110 + 30 indent; second: 40 + 110.
    let first = &lines[0];
    let second = &lines[1];
    assert!(
        (first.0 - (40.0 + 110.0 + 30.0)).abs() < 0.5,
        "first line must carry the indent beside the float: {first:?}"
    );
    assert!(
        (second.0 - (40.0 + 110.0)).abs() < 0.5,
        "second line beside the float has no indent: {second:?}"
    );
}

#[test]
fn quotes_and_attr_content_items() {
    let html = "<html><body>\
                <p class=\"q\">Outer <span class=\"q\">inner</span> tail</p>\
                <p class=\"n\" data-note=\"N7\">noted</p>\
                </body></html>";
    let (layout, _) = layout_html(
        html,
        ".q::before { content: open-quote; } .q::after { content: close-quote; } \
         .n::before { content: attr(data-note) \". \"; }",
        &page_for_lines(10),
    );
    let texts = line_texts_in_order(&layout);
    let joined = texts.join(" ");
    assert!(
        joined.contains("\u{201C}Outer \u{2018}inner\u{2019} tail\u{201D}"),
        "nested quotes with depth: {joined:?}"
    );
    assert!(joined.contains("N7. noted"), "attr() content: {joined:?}");
}

#[test]
fn quotes_property_overrides_marks() {
    let html = "<html><body><p class=\"q\">guillemets</p></body></html>";
    let (layout, _) = layout_html(
        html,
        ".q { quotes: \"\u{AB}\" \"\u{BB}\"; } \
         .q::before { content: open-quote; } .q::after { content: close-quote; }",
        &page_for_lines(10),
    );
    let joined = line_texts_in_order(&layout).join(" ");
    assert!(
        joined.contains("\u{AB}guillemets\u{BB}"),
        "quotes property must supply the marks: {joined:?}"
    );
}

#[test]
fn decoration_stretches_on_justified_indented_first_line() {
    // The whole paragraph is underlined; the split-off first line gets
    // manually justified, and its underline must stretch with it.
    let words = "stretch these underlined words again and again ".repeat(6);
    let html = format!("<html><body><p><u>{words}</u></p></body></html>");
    let (layout, _) = layout_html(
        &html,
        "p { margin: 0; text-indent: 40px; text-align: justify; }",
        &page_for_lines(12),
    );
    let first = layout.pages[0]
        .fragments
        .iter()
        .find_map(|f| match &f.kind {
            FragmentKind::Line(l) => Some((f.rect, l.decorations.clone())),
            _ => None,
        })
        .expect("first line");
    let (rect, decorations) = first;
    assert!(!decorations.is_empty(), "underline present");
    let dec_right = decorations
        .iter()
        .map(|d| d.x + d.width)
        .fold(0.0f32, f32::max);
    assert!(
        (dec_right - rect.size.w).abs() < 1.5,
        "underline must span the justified line: dec_right={dec_right} line_w={}",
        rect.size.w
    );
}

// ---- Themes ----

fn layout_html_settings(
    html: &str,
    css: &str,
    page: &PageMetrics,
    settings: &ReadingSettings,
) -> ChapterLayout {
    let mut doc = chapbook_layout::dom::parse_xhtml(html.as_bytes(), "test.xhtml").unwrap();
    let css_sources = vec![css.to_string()];
    let mut engine = chapbook_layout::cascade::StyleEngine::new(page, settings);
    engine.set_author_sheets(&css_sources);
    engine.style_document(&mut doc);
    let mut fonts = fonts();
    chapbook_layout::paginate(
        &doc,
        &css_sources,
        page,
        &mut fonts,
        &mut chapbook_paint::ImageStore::default(),
    )
}

fn first_run_color(layout: &ChapterLayout, needle: &str) -> chapbook_core::Rgba {
    layout
        .pages
        .iter()
        .flat_map(|p| p.fragments.iter())
        .find_map(|f| match &f.kind {
            FragmentKind::Line(l) if l.text.contains(needle) => Some(l.runs[0].color),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no line containing {needle:?}"))
}

#[test]
fn sepia_recolors_defaults_dark_forces_everything() {
    use chapbook_core::Theme;
    let html = "<html><body><p>plain text</p><p class=\"red\">warm text</p></body></html>";
    let css = ".red { color: #c04030; }";
    let author_red = chapbook_core::Rgba::new(0xc0, 0x40, 0x30, 255);

    // Sepia is gentle: defaults recolor, author colors survive.
    let sepia = ReadingSettings {
        theme: Theme::Sepia,
        ..ReadingSettings::default()
    };
    let layout = layout_html_settings(html, css, &page_for_lines(10), &sepia);
    assert_eq!(first_run_color(&layout, "plain"), Theme::Sepia.foreground());
    assert_eq!(
        first_run_color(&layout, "warm"),
        author_red,
        "sepia must not override author-specified colors"
    );

    // Dark forces: readability beats publisher colors in night mode.
    let dark = ReadingSettings {
        theme: Theme::Dark,
        ..ReadingSettings::default()
    };
    let layout = layout_html_settings(html, css, &page_for_lines(10), &dark);
    assert_eq!(first_run_color(&layout, "plain"), Theme::Dark.foreground());
    assert_eq!(first_run_color(&layout, "warm"), Theme::Dark.foreground());

    // The identity theme really is identity: default black text.
    let light = layout_html_settings(html, css, &page_for_lines(10), &ReadingSettings::default());
    assert_eq!(first_run_color(&light, "plain"), Theme::Light.foreground());
    assert_eq!(first_run_color(&light, "warm"), author_red);
}

#[test]
fn dark_theme_flips_prefers_color_scheme() {
    use chapbook_core::Theme;
    let html =
        "<html><body><p class=\"day\">day only</p><p class=\"night\">night only</p></body></html>";
    let css = ".night { display: none; } \
               @media (prefers-color-scheme: dark) { \
                 .night { display: block; } .day { display: none; } }";
    let light = layout_html_settings(html, css, &page_for_lines(10), &ReadingSettings::default());
    let dark_settings = ReadingSettings {
        theme: Theme::Dark,
        ..ReadingSettings::default()
    };
    let dark = layout_html_settings(html, css, &page_for_lines(10), &dark_settings);
    let texts = |l: &ChapterLayout| line_texts_in_order(l).join(" ");
    assert!(texts(&light).contains("day only") && !texts(&light).contains("night only"));
    assert!(texts(&dark).contains("night only") && !texts(&dark).contains("day only"));
}

// ---- Selection geometry ----

#[test]
fn hit_test_and_range_rects_agree() {
    let html = "<html><body><p>alpha beta gamma delta epsilon zeta eta theta \
                iota kappa lambda mu</p></body></html>";
    let (layout, _) = layout_html(html, "p { margin: 0; }", &page_for_lines(10));
    let page = &layout.pages[0];
    let lines: Vec<&chapbook_paint::Fragment> = page
        .fragments
        .iter()
        .filter(|f| matches!(f.kind, FragmentKind::Line(_)))
        .collect();
    assert!(!lines.is_empty());
    let r = lines[0].rect;

    // A point mid-line hit-tests to an offset within the paragraph.
    let mid = chapbook_core::Point::new(r.origin.x + r.size.w / 2.0, r.origin.y + r.size.h / 2.0);
    let offset = page.offset_at(mid).expect("hit");
    // Points left/right of the line clamp to its ends.
    let left = page
        .offset_at(chapbook_core::Point::new(r.origin.x - 20.0, mid.y))
        .unwrap();
    let right = page
        .offset_at(chapbook_core::Point::new(
            r.origin.x + r.size.w + 20.0,
            mid.y,
        ))
        .unwrap();
    assert!(
        left < offset && offset < right,
        "{left} < {offset} < {right}"
    );
    // A point in the page margin above all text hits nothing.
    assert_eq!(page.offset_at(chapbook_core::Point::new(5.0, 5.0)), None);

    // Highlight rects for [left, right) cover the hit point.
    let rects = page.rects_for_range(left, right);
    assert!(!rects.is_empty());
    assert!(
        rects.iter().any(|hr| mid.x >= hr.origin.x
            && mid.x <= hr.origin.x + hr.size.w
            && mid.y >= hr.origin.y
            && mid.y <= hr.origin.y + hr.size.h),
        "selection rects must cover the selected point: {rects:?}"
    );
    // And an empty range yields nothing.
    assert!(page.rects_for_range(offset, offset).is_empty());
}

#[test]
fn selection_paints_under_text_in_display_list() {
    let html = "<html><body><p>select some of this text please</p></body></html>";
    let (layout, _) = layout_html(html, "p { margin: 0; }", &page_for_lines(10));
    let page = &layout.pages[0];
    let sel_color = chapbook_core::Rgba::new(80, 120, 200, 120);
    let dl = chapbook_paint::build_display_list(
        page,
        chapbook_core::Rgba::WHITE,
        &[chapbook_paint::Selection {
            start: 2,
            end: 12,
            color: sel_color,
        }],
    );
    let sel_idx = dl
        .ops
        .iter()
        .position(
            |op| matches!(op, chapbook_paint::DisplayOp::FillRect { color, .. } if *color == sel_color),
        )
        .expect("selection rect present");
    let glyph_idx = dl
        .ops
        .iter()
        .position(|op| matches!(op, chapbook_paint::DisplayOp::GlyphRun { .. }))
        .expect("glyphs present");
    assert!(sel_idx < glyph_idx, "selection paints under the text");
}

#[test]
fn multi_line_selection_covers_each_line() {
    let html = format!("<html><body>{}</body></html>", para_of_lines(3, "sel"));
    let (layout, _) = layout_html(&html, "p { margin: 0; }", &page_for_lines(10));
    let page = &layout.pages[0];
    // Select from within line 1 to within line 3.
    let all: Vec<u32> = page
        .fragments
        .iter()
        .filter_map(|f| match &f.kind {
            FragmentKind::Line(l) => Some(l.locator_start),
            _ => None,
        })
        .collect();
    assert_eq!(all.len(), 3);
    let rects = page.rects_for_range(all[0] + 2, all[2] + 2);
    assert_eq!(rects.len(), 3, "one rect per touched line: {rects:?}");
}
