//! CLI subcommands, one milestone each. Output formats are deterministic —
//! the snapshot tests in `tests/` capture them verbatim.

use std::path::Path;

use chapbook_core::{BookKind, PageMetrics, Publication, ReadingSettings, Result, TocEntry};
use chapbook_epub::Book;
use chapbook_layout::{cascade, dom};
use chapbook_reader::open_publication;

/// The fixture corpus's fonts: vendored faces only, never host fonts, so
/// every stage this CLI dumps is byte-identical on any machine.
///
/// Crimson Text answers all five CSS generics and covers the Latin corpus.
/// The second directory holds the Hebrew and Arabic faces, reached only
/// through per-script fallback — a Latin page never sees them, which is
/// why adding them moved no existing golden. It is separate because
/// `fixtures/fonts` is scanned recursively and pinned at four faces by a
/// test, and is what every other fixture falls back to.
///
/// Without the mapping the bidi fixture would still lay out, still
/// paginate and still render. It would render as tofu.
fn fixture_fonts() -> chapbook_core::FontSource {
    use chapbook_core::{Faces, FallbackFamilies, Fallbacks, FontSource, ScriptTag};

    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
    let mut source = FontSource::embedded(fixtures.join("fonts"), "Crimson Text");
    source.faces.push(Faces::Dir(fixtures.join("fonts-bidi")));
    source.fallback = Fallbacks::Explicit(FallbackFamilies {
        common: Vec::new(),
        per_script: vec![
            (
                ScriptTag::new("Hebr").expect("Hebr is a script tag"),
                vec!["Noto Sans Hebrew".into()],
            ),
            (
                ScriptTag::new("Arab").expect("Arab is a script tag"),
                vec!["Noto Naskh Arabic".into()],
            ),
        ],
        forbidden: Vec::new(),
    });
    source
}

pub fn meta(book_path: &Path) -> Result<String> {
    let book = open_publication(book_path)?;
    // EPUBs report their fixed-layout status; the trait surface doesn't
    // carry it (comics are inherently fixed pages).
    let layout_note = match book.kind() {
        BookKind::Epub if Book::open(book_path)?.is_fixed_layout() => "fixed (unsupported)",
        BookKind::Epub => "reflowable",
        _ => "pages (image book)",
    };
    let md = book.metadata();
    let mut out = String::new();
    push_field(&mut out, "title", md.title.as_deref());
    for author in &md.authors {
        push_field(&mut out, "author", Some(author));
    }
    push_field(&mut out, "language", md.language.as_deref());
    push_field(&mut out, "identifier", md.identifier.as_deref());
    push_field(&mut out, "version", Some(&md.format_version));
    push_field(&mut out, "layout", Some(layout_note));
    out.push_str(&format!("spine:      {} items\n", book.spine().len()));
    Ok(out)
}

/// Any publication's toc, not just an EPUB's: a PDF's outline and a CBZ's
/// ComicInfo bookmarks land here too.
pub fn toc(book_path: &Path) -> Result<String> {
    let book = open_publication(book_path)?;
    let mut out = String::new();
    fn walk(entries: &[TocEntry], depth: usize, out: &mut String) {
        for e in entries {
            let target = match (&e.href, &e.fragment) {
                (Some(h), Some(f)) => format!("{h}#{f}"),
                (Some(h), None) => h.clone(),
                _ => "-".to_string(),
            };
            out.push_str(&format!(
                "{}{}  [{}]\n",
                "  ".repeat(depth),
                e.label,
                target
            ));
            walk(&e.children, depth + 1, out);
        }
    }
    walk(book.toc(), 0, &mut out);
    Ok(out)
}

pub fn text(epub: &Path, spine: Option<usize>) -> Result<String> {
    let book = Book::open(epub)?;
    let indices: Vec<usize> = match spine {
        Some(i) => vec![i],
        None => (0..book.spine().len()).collect(),
    };
    let mut out = String::new();
    for i in indices {
        let href = book.spine_item(i)?.href.clone();
        let bytes = book.unit_bytes(i)?;
        let doc = dom::parse_xhtml(&bytes, &href)?;
        if spine.is_none() {
            out.push_str(&format!("==== spine {i} ({href}) ====\n"));
        }
        out.push_str(&dom::extract_text(&doc));
    }
    Ok(out)
}

pub fn styles(epub: &Path, spine: usize) -> Result<String> {
    let book = Book::open(epub)?;
    let href = book.spine_item(spine)?.href.clone();
    let (doc, _css, notes) = styled_chapter(&book, spine, &href, &ReadingSettings::default())?;
    Ok(notes + &cascade::dump_computed_styles(&doc))
}

/// Register @font-face fonts into `fonts` and decode the chapter's images.
fn load_chapter_assets(
    book: &Book,
    chapter_href: &str,
    doc: &dom::Document,
    css_pairs: &[(String, String)],
    fonts: &mut cosmic_text::FontSystem,
) -> chapbook_paint::ImageStore {
    for face in chapbook_layout::extract_font_faces(css_pairs) {
        for src in &face.sources {
            if let Ok(res) = book.resource(&face.base, src) {
                if chapbook_layout::register_font(fonts, &face.family, res.data) {
                    break;
                }
            }
        }
    }
    chapbook_layout::collect_images(doc, Some(fonts), |href| {
        book.resource(chapter_href, href).ok().map(|r| r.data)
    })
}

pub fn layout(epub: &Path, spine: usize) -> Result<String> {
    let book = Book::open(epub)?;
    let href = book.spine_item(spine)?.href.clone();
    let (doc, css, notes) = styled_chapter(&book, spine, &href, &ReadingSettings::default())?;

    let (mut fonts, _) = chapbook_layout::build_font_system(&fixture_fonts())?;
    let images = load_chapter_assets(&book, &href, &doc, &css, &mut fonts);
    let sheets: Vec<String> = css.iter().map(|(text, _)| text.clone()).collect();
    let layout =
        chapbook_layout::paginate(&doc, &sheets, &PageMetrics::default(), &mut fonts, &images);

    let mut out = notes;
    out.push_str(&format!("pages: {}\n", layout.pages.len()));
    let mut anchors: Vec<_> = layout.anchors.iter().collect();
    anchors.sort();
    for (id, page) in anchors {
        out.push_str(&format!("anchor #{id} -> page {page}\n"));
    }
    for (i, page) in layout.pages.iter().enumerate() {
        out.push_str(&format!(
            "-- page {i} (locator start {})\n",
            layout.char_map[i]
        ));
        for fragment in &page.fragments {
            let r = &fragment.rect;
            match &fragment.kind {
                chapbook_paint::FragmentKind::Line(line) => {
                    out.push_str(&format!(
                        "  [{:7.1},{:7.1} {:6.1}x{:5.1}] base={:5.1} loc={:<5} {:?}\n",
                        r.origin.x,
                        r.origin.y,
                        r.size.w,
                        r.size.h,
                        line.baseline,
                        line.locator_start,
                        line.text,
                    ));
                }
                other => out.push_str(&format!(
                    "  [{:7.1},{:7.1} {:6.1}x{:5.1}] {other:?}\n",
                    r.origin.x, r.origin.y, r.size.w, r.size.h
                )),
            }
        }
    }
    Ok(out)
}

pub fn render(
    epub: &Path,
    spine: usize,
    page: usize,
    out: &Path,
    theme: chapbook_core::Theme,
) -> Result<String> {
    let publication = open_publication(epub)?;
    if !matches!(publication.kind(), BookKind::Epub) {
        return render_image_book(publication.as_ref(), spine, out, theme);
    }
    drop(publication);
    let book = Book::open(epub)?;
    let href = book.spine_item(spine)?.href.clone();
    let settings = ReadingSettings {
        theme,
        ..ReadingSettings::default()
    };
    let (doc, css, _notes) = styled_chapter(&book, spine, &href, &settings)?;

    let (mut fonts, _) = chapbook_layout::build_font_system(&fixture_fonts())?;
    let images = load_chapter_assets(&book, &href, &doc, &css, &mut fonts);
    let sheets: Vec<String> = css.iter().map(|(text, _)| text.clone()).collect();
    let metrics = PageMetrics::default();
    let layout = chapbook_layout::paginate(&doc, &sheets, &metrics, &mut fonts, &images);

    let page_data = layout.pages.get(page).ok_or_else(|| {
        chapbook_core::ChapbookError::Layout(format!(
            "page {page} out of range (chapter has {})",
            layout.pages.len()
        ))
    })?;

    let dl = chapbook_paint::build_display_list(page_data, theme.background(), &[]);
    let scale = metrics.dpi_scale;
    let mut pixmap = chapbook_render_tinyskia::tiny_skia::Pixmap::new(
        (dl.size.w * scale) as u32,
        (dl.size.h * scale) as u32,
    )
    .ok_or_else(|| chapbook_core::ChapbookError::Layout("empty page size".into()))?;
    let mut renderer = chapbook_render_tinyskia::Renderer::new();
    renderer.render(&dl, &mut fonts, &images, scale, &mut pixmap.as_mut());
    pixmap
        .save_png(out)
        .map_err(|e| chapbook_core::ChapbookError::Io(std::io::Error::other(e)))?;
    Ok(format!(
        "rendered spine {spine} page {page}/{} ({}x{}) to {}\n",
        layout.pages.len() - 1,
        pixmap.width(),
        pixmap.height(),
        out.display()
    ))
}

/// A chapter's stylesheets as `(css text, container path of the sheet)`.
type CssSheets = Vec<(String, String)>;

/// Parse + cascade one chapter: shared plumbing for `styles` and `layout`.
fn styled_chapter(
    book: &Book,
    spine: usize,
    href: &str,
    settings: &ReadingSettings,
) -> Result<(dom::Document, CssSheets, String)> {
    let bytes = book.unit_bytes(spine)?;
    let mut doc = dom::parse_xhtml(&bytes, href)?;

    // Author stylesheets in document order as (text, container path of the
    // declaring sheet): <style> contents inline (base = the chapter),
    // <link rel=stylesheet> resolved against the chapter (base = the
    // stylesheet itself, for @font-face url() resolution). A missing sheet
    // degrades to "no publisher styles from that link", noted in the output
    // so goldens surface it.
    let mut css: Vec<(String, String)> = Vec::new();
    let mut notes = String::new();
    for source in doc.stylesheet_sources() {
        match source {
            dom::StylesheetSource::Inline(text) => {
                css.push((text, href.to_string()));
            }
            dom::StylesheetSource::External(rel) => match book.resource(href, &rel) {
                Ok(res) => css.push((
                    String::from_utf8_lossy(&res.data).into_owned(),
                    chapbook_epub::resolve_href(href, &rel),
                )),
                Err(_) => notes.push_str(&format!("!! stylesheet not found: {rel}\n")),
            },
        }
    }

    let sheets: Vec<String> = css.iter().map(|(text, _)| text.clone()).collect();
    let mut engine = cascade::StyleEngine::new(&PageMetrics::default(), settings);
    engine.set_author_sheets(&sheets);
    engine.style_document(&mut doc);
    Ok((doc, css, notes))
}

fn push_field(out: &mut String, name: &str, value: Option<&str>) {
    if let Some(v) = value {
        out.push_str(&format!("{:<11} {v}\n", format!("{name}:")));
    }
}

fn opds_client(url: &str) -> chapbook_opds::OpdsClient {
    use chapbook_core::{CredentialLookup, CredentialStore, Freshness};

    let mut client = chapbook_opds::OpdsClient::with_ureq();
    // The dev CLI's store is the environment; a viewer on a platform with
    // real secret storage injects a different one and this code is the
    // same shape. Keyed by origin so a catalog URL's secret-bearing path
    // never becomes part of a key.
    let store = chapbook_core::EnvCredentials;
    if let Some(key) = chapbook_core::CredentialKey::http_origin(url) {
        if let CredentialLookup::Found(credential) = store.get(&key, Freshness::Cached) {
            client.set_authorization(credential.authorization);
        }
    }
    client
}

fn describe_opds_error(e: chapbook_opds::OpdsError) -> chapbook_core::ChapbookError {
    if let chapbook_opds::OpdsError::AuthRequired(Some(doc)) = &e {
        let flows: Vec<&str> = doc.authentication.iter().map(|f| f.kind.as_str()).collect();
        return chapbook_core::ChapbookError::Opds(format!(
            "authentication required by \"{}\" (flows: {}) — set {} / {}",
            doc.title,
            flows.join(", "),
            chapbook_core::EnvCredentials::USER_VAR,
            chapbook_core::EnvCredentials::PASSWORD_VAR,
        ));
    }
    chapbook_opds::to_chapbook_error(e)
}

fn dump_feed(feed: &chapbook_opds::Feed) -> String {
    let mut out = format!("{} [{:?}]\n", feed.title, feed.version);
    if let Some(total) = feed.totals.total_results {
        out.push_str(&format!("total: {total}"));
        if let Some(per) = feed.totals.items_per_page {
            out.push_str(&format!(" ({per}/page)"));
        }
        out.push('\n');
    }
    for (group, facets) in feed.facet_groups() {
        let names: Vec<String> = facets
            .iter()
            .map(|f| {
                let name = f.title.clone().unwrap_or_default();
                if f.active_facet {
                    format!("[{name}]")
                } else {
                    name
                }
            })
            .collect();
        out.push_str(&format!("facets/{group}: {}\n", names.join(" ")));
    }
    fn list_entry(out: &mut String, entry: &chapbook_opds::Entry) {
        out.push_str(&format!("- {}", entry.title));
        if !entry.authors.is_empty() {
            out.push_str(&format!(" — {}", entry.authors.join(", ")));
        }
        out.push('\n');
        for acq in entry.acquisitions() {
            let kind = acq
                .rel
                .iter()
                .find_map(|r| r.rsplit('/').next())
                .unwrap_or("acquisition");
            let mt = acq
                .media_type
                .as_ref()
                .map(|t| t.essence.clone())
                .unwrap_or_default();
            out.push_str(&format!("    {kind} {mt}: {}\n", acq.href));
        }
        if let Some(nav) = entry.navigation() {
            out.push_str(&format!("    -> {}\n", nav.href));
        }
        if let Some(stream) = entry.pse_stream() {
            out.push_str(&format!(
                "    pages: {} (stream){}\n",
                stream.pse_count.unwrap_or(0),
                stream
                    .pse_last_read
                    .map(|p| format!(" last-read {p}"))
                    .unwrap_or_default()
            ));
        }
    }
    for entry in &feed.entries {
        list_entry(&mut out, entry);
    }
    for group in &feed.groups {
        out.push_str(&format!("group: {}\n", group.title));
        for entry in &group.entries {
            list_entry(&mut out, entry);
        }
    }
    if let Some(next) = feed.next() {
        out.push_str(&format!("next: {}\n", next.href));
    }
    if let Some(previous) = feed.previous() {
        out.push_str(&format!("previous: {}\n", previous.href));
    }
    out
}

pub fn opds_ls(url: &str) -> Result<String> {
    let feed = opds_client(url).fetch(url).map_err(describe_opds_error)?;
    Ok(dump_feed(&feed))
}

pub fn opds_search(url: &str, query: &str) -> Result<String> {
    let client = opds_client(url);
    let feed = client.fetch(url).map_err(describe_opds_error)?;
    let results = client
        .search(&feed, url, query)
        .map_err(describe_opds_error)?;
    Ok(dump_feed(&results))
}

pub fn opds_get(url: &str, out: &Path) -> Result<String> {
    opds_client(url)
        .download(url, out)
        .map_err(describe_opds_error)?;
    let size = std::fs::metadata(out).map(|m| m.len()).unwrap_or(0);
    Ok(format!("downloaded {} ({size} bytes)\n", out.display()))
}

/// Import any publication. The library was built format-agnostic — it
/// takes a `BookMetadata` and keeps the source extension so the format can
/// be sniffed on reopen — but this entry point opened everything as an
/// EPUB, so importing a comic died inside the zip reader.
pub fn lib_import(book_path: &Path) -> Result<String> {
    let book = open_publication(book_path)?;
    let mut lib = open_library()?;
    let id = lib.import(book_path, book.as_ref())?;
    let record = lib.book(id)?.expect("just imported");
    Ok(format!(
        "imported #{} \"{}\" ({} authors, {} spine items)\n",
        id.0,
        record.title,
        record.authors.len(),
        book.spine().len()
    ))
}

/// The shelf, narrowed however the caller asked.
///
/// The default sort is `read` rather than `added`: someone running
/// `lib ls` is looking for what they were reading, which is the same
/// thing a shelf shows first.
pub fn lib_ls(
    search: Option<&str>,
    collection: Option<&str>,
    series: Option<&str>,
    state: Option<chapbook_library::ReadingState>,
    sort: chapbook_library::Sort,
    limit: Option<usize>,
) -> Result<String> {
    let lib = open_library()?;
    let collection = collection
        .map(|name| named_collection(&lib, name))
        .transpose()?;
    let books = lib.query(&chapbook_library::BookQuery {
        search,
        collection,
        series,
        state,
        sort,
        limit,
        offset: 0,
    })?;
    if books.is_empty() {
        // Distinguish "nothing here" from "nothing matched": the second
        // is a filter to relax, and telling a reader to import a book
        // when they have fifty is unhelpful.
        let narrowed =
            search.is_some() || collection.is_some() || series.is_some() || state.is_some();
        return Ok(if narrowed {
            "nothing on the shelf matches\n".into()
        } else {
            "library is empty — chapbook lib import <book>\n".into()
        });
    }
    let mut out = String::new();
    for book in books {
        out.push_str(&format!("#{:<4} {}", book.id.0, book.title));
        if !book.authors.is_empty() {
            out.push_str(&format!(" — {}", book.authors.join(", ")));
        }
        if let Some(series) = &book.series {
            out.push_str(&format!("  [{series}"));
            if let Some(index) = book.series_index {
                out.push_str(&format!(" #{}", trim_index(index)));
            }
            out.push(']');
        }
        out.push_str(&format!("  {}", describe_state(&book)));
        if !book.collections.is_empty() {
            let names: Vec<&str> = book.collections.iter().map(|c| c.name.as_str()).collect();
            out.push_str(&format!("  {{{}}}", names.join(", ")));
        }
        if book.cover_path.is_some() {
            out.push_str("  (cover)");
        }
        out.push('\n');
    }
    Ok(out)
}

/// Everything the library holds about one book, including the parts
/// `ls` has no room for.
pub fn lib_show(id: i64) -> Result<String> {
    let lib = open_library()?;
    let id = chapbook_library::BookId(id);
    let book = require_book(&lib, id)?;

    let mut out = String::new();
    out.push_str(&format!("#{}  {}\n", book.id.0, book.title));
    if !book.authors.is_empty() {
        out.push_str(&format!("  authors      {}\n", book.authors.join(", ")));
    }
    if let Some(series) = &book.series {
        let position = book
            .series_index
            .map(|i| format!(" #{}", trim_index(i)))
            .unwrap_or_default();
        out.push_str(&format!("  series       {series}{position}\n"));
    }
    if let Some(language) = &book.language {
        out.push_str(&format!("  language     {language}\n"));
    }
    if let Some(identifier) = &book.identifier {
        out.push_str(&format!("  identifier   {identifier}\n"));
    }
    out.push_str(&format!("  state        {}\n", describe_state(&book)));
    out.push_str(&format!("  fingerprint  {}\n", book.fingerprint));
    // Empty for an adopted book: the platform owns the file and the
    // shell owns the means of reaching it again.
    out.push_str(&format!(
        "  file         {}\n",
        if book.file_path.as_os_str().is_empty() {
            "(adopted — the shell holds the handle)".to_string()
        } else {
            book.file_path.display().to_string()
        }
    ));
    if let Some(cover) = &book.cover_path {
        out.push_str(&format!("  cover        {}\n", cover.display()));
    }
    if !book.collections.is_empty() {
        let names: Vec<&str> = book.collections.iter().map(|c| c.name.as_str()).collect();
        out.push_str(&format!("  collections  {}\n", names.join(", ")));
    }
    out.push_str(&format!("  annotations  {}\n", lib.annotations(id)?.len()));

    // Never the URLs themselves: a service URL may embed a per-user key,
    // and this prints to a terminal that scrolls into a bug report.
    let targets = lib.sync_targets(id)?;
    let services = [
        ("position", targets.progression_url.is_some()),
        ("annotations", targets.annotation_container.is_some()),
    ]
    .iter()
    .filter(|(_, present)| *present)
    .map(|(name, _)| *name)
    .collect::<Vec<_>>();
    out.push_str(&format!(
        "  syncs        {}\n",
        if services.is_empty() {
            "nothing (sideloaded)".to_string()
        } else {
            services.join(", ")
        }
    ));
    Ok(out)
}

pub fn lib_rm(id: i64) -> Result<String> {
    let mut lib = open_library()?;
    let id = chapbook_library::BookId(id);
    let record = require_book(&lib, id)?;
    lib.delete_book(id)?;
    Ok(format!(
        "removed #{} \"{}\" (annotations kept: a re-import finds them again)\n",
        id.0, record.title
    ))
}

pub fn lib_finish(id: i64, finished: bool) -> Result<String> {
    let mut lib = open_library()?;
    let id = chapbook_library::BookId(id);
    let record = require_book(&lib, id)?;
    lib.set_finished(id, finished)?;
    Ok(format!(
        "#{} \"{}\" is {}\n",
        id.0,
        record.title,
        if finished { "finished" } else { "unfinished" }
    ))
}

pub fn lib_series() -> Result<String> {
    let lib = open_library()?;
    let series = lib.series()?;
    if series.is_empty() {
        return Ok("no book on the shelf names a series\n".into());
    }
    let mut out = String::new();
    for (name, count) in series {
        out.push_str(&format!("{count:>4}  {name}\n"));
    }
    Ok(out)
}

pub fn lib_collections() -> Result<String> {
    let lib = open_library()?;
    let collections = lib.collections()?;
    if collections.is_empty() {
        return Ok("no collections — chapbook lib collection new <name>\n".into());
    }
    let mut out = String::new();
    for collection in collections {
        out.push_str(&format!("{:>4}  {}\n", collection.books, collection.name));
    }
    Ok(out)
}

pub fn lib_collection_new(name: &str) -> Result<String> {
    let mut lib = open_library()?;
    let id = lib.create_collection(name)?;
    Ok(format!("collection #{} \"{name}\"\n", id.0))
}

pub fn lib_collection_rename(name: &str, new_name: &str) -> Result<String> {
    let mut lib = open_library()?;
    let id = named_collection(&lib, name)?;
    lib.rename_collection(id, new_name)?;
    Ok(format!("\"{name}\" is now \"{new_name}\"\n"))
}

pub fn lib_collection_rm(name: &str) -> Result<String> {
    let mut lib = open_library()?;
    let id = named_collection(&lib, name)?;
    lib.delete_collection(id)?;
    Ok(format!("removed \"{name}\" (the books stayed)\n"))
}

/// Creating the collection if it does not exist: `create_collection` is
/// idempotent on the name, and asking someone to declare a shelf before
/// putting a book on it is a step with no question behind it.
pub fn lib_collection_add(name: &str, id: i64) -> Result<String> {
    let mut lib = open_library()?;
    let book = chapbook_library::BookId(id);
    let record = require_book(&lib, book)?;
    let collection = lib.create_collection(name)?;
    lib.add_to_collection(book, collection)?;
    Ok(format!("#{id} \"{}\" is in \"{name}\"\n", record.title))
}

pub fn lib_collection_remove(name: &str, id: i64) -> Result<String> {
    let mut lib = open_library()?;
    let book = chapbook_library::BookId(id);
    let record = require_book(&lib, book)?;
    let collection = named_collection(&lib, name)?;
    lib.remove_from_collection(book, collection)?;
    Ok(format!("#{id} \"{}\" is out of \"{name}\"\n", record.title))
}

fn open_library() -> Result<chapbook_library::Library> {
    chapbook_library::Library::open(&chapbook_library::Library::default_dir()?)
}

fn require_book(
    lib: &chapbook_library::Library,
    id: chapbook_library::BookId,
) -> Result<chapbook_library::BookRecord> {
    lib.book(id)?.ok_or_else(|| {
        chapbook_core::ChapbookError::Library(format!("no book #{} in the library", id.0))
    })
}

/// Collections are addressed by name here rather than by id: a person
/// typing at a terminal knows the name they gave it, and the id is not
/// printed anywhere they would have looked.
fn named_collection(
    lib: &chapbook_library::Library,
    name: &str,
) -> Result<chapbook_library::CollectionId> {
    lib.collections()?
        .into_iter()
        .find(|c| c.name.eq_ignore_ascii_case(name))
        .map(|c| c.id)
        .ok_or_else(|| {
            chapbook_core::ChapbookError::Library(format!(
                "no collection \"{name}\" — chapbook lib collection ls"
            ))
        })
}

/// "reading 42%", "finished", "unread" — the state with the progress
/// that qualifies it, where there is one.
fn describe_state(book: &chapbook_library::BookRecord) -> String {
    let percent = book
        .progress
        .map(|p| format!(" {:.0}%", p * 100.0))
        .unwrap_or_default();
    match book.state() {
        chapbook_library::ReadingState::Unread => "unread".to_string(),
        chapbook_library::ReadingState::Reading => format!("reading{percent}"),
        // The progress of a finished book is where the reader is now,
        // which may be the beginning again — so it is not shown beside
        // a word it would contradict.
        chapbook_library::ReadingState::Finished => "finished".to_string(),
    }
}

/// `2` rather than `2.0`, but `2.5` intact: series positions are whole
/// numbers except when they are not.
fn trim_index(index: f64) -> String {
    if index.fract() == 0.0 {
        format!("{index:.0}")
    } else {
        format!("{index}")
    }
}

/// Convert between reading positions and EPUB CFIs. Encode with
/// `--spine N --offset M`; decode with `--cfi "epubcfi(...)"`. Resolution
/// needs only the parsed document — no styling.
pub fn cfi(
    epub: &Path,
    spine: Option<usize>,
    offset: Option<u32>,
    cfi_str: Option<&str>,
) -> Result<String> {
    let book = Book::open(epub)?;
    let chapter_doc = |spine: usize| -> Result<dom::Document> {
        let href = book.spine_item(spine)?.href.clone();
        let bytes = book.unit_bytes(spine)?;
        let doc = dom::parse_xhtml(&bytes, &href)?;
        Ok(doc)
    };
    match (spine, offset, cfi_str) {
        (Some(spine), Some(offset), None) => {
            let doc = chapter_doc(spine)?;
            let cfi = dom::cfi_for_offset(&doc, spine, offset)
                .ok_or_else(|| chapbook_core::ChapbookError::Cfi("chapter has no text".into()))?;
            Ok(format!("{cfi}\n"))
        }
        (None, None, Some(cfi_str)) => {
            let cfi = chapbook_core::Cfi::parse(cfi_str)?;
            let spine = cfi.spine_index().ok_or_else(|| {
                chapbook_core::ChapbookError::Cfi(
                    "package part does not address a spine item".into(),
                )
            })?;
            let doc = chapter_doc(spine)?;
            let offset = dom::offset_for_cfi(&doc, &cfi).ok_or_else(|| {
                chapbook_core::ChapbookError::Cfi("CFI does not resolve in this chapter".into())
            })?;
            let text = dom::locator_text(&doc);
            let chars: Vec<char> = text.chars().collect();
            let start = (offset as usize).saturating_sub(30);
            let end = (offset as usize + 30).min(chars.len());
            let excerpt: String = chars[start..end].iter().collect();
            Ok(format!(
                "spine {spine} offset {offset}\n…{}…\n",
                excerpt.replace(['\n', '\r'], " ")
            ))
        }
        _ => Err(chapbook_core::ChapbookError::Cfi(
            "pass either --spine N --offset M (encode) or --cfi CFI (decode)".into(),
        )),
    }
}

/// Render one image-book page (CBZ or PDF): decode the image,
/// scale-to-fit page model, no dom/stylo/shaping anywhere in the path.
fn render_image_book(
    book: &dyn Publication,
    spine: usize,
    out: &Path,
    theme: chapbook_core::Theme,
) -> Result<String> {
    let bytes = book.unit_bytes(spine)?;
    let decoded = image::load_from_memory(&bytes)
        .map_err(|e| chapbook_core::ChapbookError::BookMalformed(format!("page image: {e}")))?
        .to_rgba8();
    let (w, h) = decoded.dimensions();
    let mut images = chapbook_paint::ImageStore::default();
    images.insert(1, w, h, decoded.into_raw());

    let metrics = PageMetrics::default();
    let page = chapbook_paint::image_page(&metrics, w, h, 1);
    let dl = chapbook_paint::build_display_list(&page, theme.background(), &[]);
    let scale = metrics.dpi_scale;
    let mut pixmap = chapbook_render_tinyskia::tiny_skia::Pixmap::new(
        (dl.size.w * scale) as u32,
        (dl.size.h * scale) as u32,
    )
    .ok_or_else(|| chapbook_core::ChapbookError::Layout("empty page size".into()))?;
    let (mut fonts, _) = chapbook_layout::build_font_system(&fixture_fonts())?;
    let mut renderer = chapbook_render_tinyskia::Renderer::new();
    renderer.render(&dl, &mut fonts, &images, scale, &mut pixmap.as_mut());
    pixmap
        .save_png(out)
        .map_err(|e| chapbook_core::ChapbookError::Io(std::io::Error::other(e)))?;
    Ok(format!(
        "rendered page {spine}/{} ({}x{}) to {}\n",
        book.spine().len(),
        pixmap.width(),
        pixmap.height(),
        out.display()
    ))
}

// ---- lib sync ----

/// Reconcile one book, or every book with a service, against what its
/// catalog already holds.
///
/// The engine holds no schedule and this is the whole of the CLI's: a
/// person typed the word. That is the honest shape for a terminal, and it
/// is why nothing here retries — a failed book says so and the next
/// invocation tries again.
///
/// Books are walked one at a time rather than through `sync_all`, because
/// the `Authorization` is per origin and a shelf can hold books from two
/// catalogs. `sync_all` shares one credential across the batch, which is
/// right for a shell with one account and wrong for a terminal pointed at
/// whatever is running locally.
pub fn lib_sync(id: Option<i64>) -> Result<String> {
    let dir = chapbook_library::Library::default_dir()?;
    let library = chapbook_library::Library::open(&dir)?;

    let books = match id {
        Some(id) => {
            let id = chapbook_library::BookId(id);
            require_book(&library, id)?;
            vec![id]
        }
        None => library.books_with_sync_targets()?,
    };

    // An empty shelf and a shelf where nothing syncs are different
    // problems, and telling someone to sync when no book has a service
    // sends them to the wrong one.
    if books.is_empty() {
        return Ok("no book in the library has a service to sync with\n\
             a book learns one from the catalog entry it was downloaded from\n"
            .to_string());
    }

    let mut engine = chapbook_sync::SyncEngine::new(
        library,
        std::sync::Arc::new(chapbook_opds::UreqHttp::new()),
        device(&dir)?,
    );

    let mut out = String::new();
    let (mut synced, mut failed, mut sideloaded) = (0usize, 0usize, 0usize);
    for book in books {
        let title = engine
            .library()
            .book(book)?
            .map(|record| record.title)
            .unwrap_or_else(|| "(gone)".to_string());
        let targets = engine.library().sync_targets(book)?;
        authorize(&mut engine, &targets);

        out.push_str(&format!("#{} \"{}\"\n", book.0, title));
        match engine.sync_book(book) {
            Ok(report) => {
                synced += 1;
                out.push_str(&format!(
                    "  {:<12} {}\n",
                    "position",
                    describe_position(&report.position)
                ));
                out.push_str(&format!(
                    "  {:<12} {}\n",
                    "marks",
                    describe_marks(&report.annotations)
                ));
            }
            // Having no service is a fact about the book, not a failure of
            // this run — a sideloaded book is the ordinary case, and naming
            // it "failed" sends someone looking for a broken network. Same
            // words `lib show` uses, so the two agree.
            Err(chapbook_sync::SyncError::NotSyncable(_)) => {
                sideloaded += 1;
                out.push_str(&format!("  {:<12} nothing (sideloaded)\n", "syncs"));
            }
            Err(e) => {
                failed += 1;
                out.push_str(&format!("  {:<12} {e}\n", "failed"));
            }
        }
    }

    let mut tally = vec![format!("{synced} synced")];
    if failed > 0 {
        tally.push(format!("{failed} failed"));
    }
    if sideloaded > 0 {
        tally.push(format!("{sideloaded} with no service"));
    }
    out.push_str(&format!("\n{}\n", tally.join(", ")));
    Ok(out)
}

/// The `Authorization` for the origin this book's services live on.
///
/// Same store and same shape as `opds_client`: the environment here, a
/// platform keychain in a real shell. Keyed by origin so a service URL's
/// per-user path never becomes part of a key — which is also why the URL
/// itself is never printed.
fn authorize(engine: &mut chapbook_sync::SyncEngine, targets: &chapbook_library::SyncTargets) {
    use chapbook_core::{CredentialLookup, CredentialStore, Freshness};

    let Some(url) = targets
        .progression_url
        .as_deref()
        .or(targets.annotation_container.as_deref())
    else {
        return;
    };
    if let Some(key) = chapbook_core::CredentialKey::http_origin(url) {
        if let CredentialLookup::Found(credential) =
            chapbook_core::EnvCredentials.get(&key, Freshness::Cached)
        {
            engine.set_authorization(credential.authorization);
        }
    }
}

fn describe_position(report: &chapbook_sync::PositionReport) -> String {
    use chapbook_sync::PositionReport::*;
    match report {
        Idle => "idle (nothing moved on either side)".to_string(),
        Pushed => "pushed".to_string(),
        Pulled => "pulled".to_string(),
        // Not a failure: the service holds something newer, and saying so
        // is the difference between "try again" and "you lost a page".
        Refused(why) => format!("refused, the service is ahead ({why})"),
        Conflict => "conflict — both moved, nothing overwritten".to_string(),
        Failed(why) => format!("failed: {why}"),
    }
}

fn describe_marks(report: &chapbook_sync::AnnotationReport) -> String {
    let mut parts = Vec::new();
    for (count, name) in [
        (report.created, "created"),
        (report.updated, "updated"),
        (report.deleted, "deleted"),
        (report.adopted, "adopted"),
        (report.refreshed, "refreshed"),
        (report.withdrawn, "withdrawn"),
        (report.conflicts, "in conflict"),
    ] {
        if count > 0 {
            parts.push(format!("{count} {name}"));
        }
    }
    if parts.is_empty() {
        parts.push("idle".to_string());
    }
    let mut line = parts.join(", ");
    // Said apart from the counts above rather than added to them: a
    // settled conflict is one of those writes, not another one — and it
    // may have left a second mark over the same words, which is the part
    // worth a reader's attention.
    if report.merged > 0 {
        line.push_str(&format!(" ({} settled a conflict)", report.merged));
    }
    // Said out loud, because it changes what the rest of the line means:
    // no deletion was inferred, so a mark another device removed may still
    // be sitting here.
    if report.truncated {
        line.push_str(" — container longer than one sync reads");
    }
    if let Some(why) = &report.failed {
        line.push_str(&format!(", container failed: {why}"));
    }
    line
}

/// The device this CLI is, minted once and kept beside the library.
///
/// `Device` is host-owned identity — chapbook-sync neither generates nor
/// persists one — and a fresh id per run would turn "last read on …" into
/// a list of strangers. It lives in the library directory rather than a
/// config dir so it travels with the library it names.
fn device(dir: &Path) -> Result<chapbook_opds::progression::Device> {
    let path = dir.join("device");
    let id = match std::fs::read_to_string(&path) {
        Ok(existing) if !existing.trim().is_empty() => existing.trim().to_string(),
        _ => {
            let id = mint_device_id();
            std::fs::write(&path, &id).map_err(|e| {
                chapbook_core::ChapbookError::Library(format!(
                    "cannot write the device id to {}: {e}",
                    path.display()
                ))
            })?;
            id
        }
    };
    Ok(chapbook_opds::progression::Device {
        id,
        name: "chapbook-cli".to_string(),
    })
}

/// 16 bytes from the OS, shaped as a v4 `urn:uuid:`.
///
/// Read straight from `/dev/urandom` rather than through a crate: this is
/// the only random number the CLI ever wants, and a dependency for it
/// would travel into every build that links the workspace.
fn mint_device_id() -> String {
    let mut bytes = [0u8; 16];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut bytes))
        .is_err()
    {
        // No `/dev` to read: hash what varies instead. Thinner entropy
        // than a UUID deserves, but it is minted once and written down,
        // and refusing to sync over it would serve nobody.
        let seed = format!("{:?}-{}", std::time::SystemTime::now(), std::process::id());
        let hex = chapbook_library::Library::fingerprint_of_bytes(seed.as_bytes());
        for (slot, pair) in bytes.iter_mut().zip(hex.as_bytes().chunks(2)) {
            *slot = std::str::from_utf8(pair)
                .ok()
                .and_then(|pair| u8::from_str_radix(pair, 16).ok())
                .unwrap_or(0);
        }
    }
    // Version 4, variant 1: a `urn:uuid:` a service reads is entitled to
    // parse as one.
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "urn:uuid:{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// Add books from a catalog: download, import, and record where they sync.
///
/// The missing half of `lib sync`. A book learns its services from the
/// catalog entry it came from and from nowhere else — in both protocols
/// the URL *is* the publication's identity — so a book that arrives any
/// other way has nothing to reconcile against. `lib import` takes a path
/// and cannot know any of that; this is the door that can.
///
/// Matched on a title substring rather than an id, because `opds ls`
/// prints titles and an id nobody has seen is not an address a person can
/// use.
pub fn lib_add(feed_url: &str, matching: &str) -> Result<String> {
    let client = opds_client(feed_url);
    let feed = client.fetch(feed_url).map_err(describe_opds_error)?;

    let needle = matching.to_lowercase();
    let matched: Vec<&chapbook_opds::Entry> = feed
        .entries
        .iter()
        .filter(|entry| entry.title.to_lowercase().contains(&needle))
        .collect();
    if matched.is_empty() {
        return Err(chapbook_core::ChapbookError::Opds(format!(
            "no entry in this feed has \"{matching}\" in its title ({} were offered)",
            feed.entries.len()
        )));
    }

    // Staging, and nothing more: the library copies what it imports into
    // its own `books/`, so a download kept anywhere else is a second copy
    // of every book that was ever added.
    let staging = std::env::temp_dir().join(format!("chapbook-add-{}", std::process::id()));
    std::fs::create_dir_all(&staging).map_err(|e| {
        chapbook_core::ChapbookError::Library(format!("cannot make {}: {e}", staging.display()))
    })?;

    let mut lib = open_library()?;
    let mut out = String::new();
    for entry in matched {
        let Some(acquisition) = entry.links.iter().find(|link| {
            link.rel
                .iter()
                .any(|rel| rel.starts_with(chapbook_opds::REL_ACQ_PREFIX))
        }) else {
            out.push_str(&format!("\"{}\" has nothing to download\n", entry.title));
            continue;
        };

        let file = staging.join(format!("{}.epub", file_stem_for(&entry.id)));
        client
            .download(
                &chapbook_opds::resolve_url(feed_url, &acquisition.href),
                &file,
            )
            .map_err(describe_opds_error)?;

        let publication = open_publication(&file)?;
        let id = lib.import(&file, publication.as_ref())?;
        drop(publication);
        let _ = std::fs::remove_file(&file);

        // The parser already resolved these against the request URL — that
        // is the crate's invariant, and a catalog does serve relative hrefs
        // (progression links come back root-relative). Resolving again is a
        // no-op on an absolute URL and is here so a service URL can never
        // reach `set_sync_targets` as a path.
        let (progression, container) = chapbook_sync::targets_of(entry);
        let progression = progression.map(|href| chapbook_opds::resolve_url(feed_url, &href));
        let container = container.map(|href| chapbook_opds::resolve_url(feed_url, &href));
        lib.set_sync_targets(id, progression.as_deref(), container.as_deref())?;

        let services = [
            ("position", progression.is_some()),
            ("annotations", container.is_some()),
        ]
        .iter()
        .filter(|(_, present)| *present)
        .map(|(name, _)| *name)
        .collect::<Vec<_>>();
        out.push_str(&format!(
            "added #{} \"{}\" — syncs {}\n",
            id.0,
            entry.title,
            if services.is_empty() {
                "nothing (the catalog advertised no service)".to_string()
            } else {
                services.join(", ")
            }
        ));
    }
    let _ = std::fs::remove_dir_all(&staging);
    Ok(out)
}

/// A filename for a catalog id, which is opaque and may hold anything —
/// comic-server ids carry slashes and dots.
fn file_stem_for(id: &str) -> String {
    let cleaned: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('-');
    if trimmed.is_empty() {
        "book".to_string()
    } else {
        trimmed.to_string()
    }
}
