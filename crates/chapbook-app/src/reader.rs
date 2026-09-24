//! Reader policy: the decisions a reading screen makes that are not
//! about drawing.
//!
//! Every function here takes the session it acts on rather than owning
//! it, because a front end already has a place a session lives — a view
//! model that survives a rotation, a slot a page area draws from — and
//! this crate has no business inventing another. What it does own is the
//! *answers*: where the reader is as one value, how much of a phone's
//! memory a page cache may take, how a search walks a book without
//! blocking a draw, what turning a selection into a highlight involves.
//! Each of these was written twice for two phones and once for a desk
//! before it was written here.

use chapbook_core::{BookKind, Locator, TocEntry};
use chapbook_reader::{SearchHit, Session};

/// Where the reader is, for the chrome, as one value read after a draw —
/// which is when a position is authoritative, because a restored
/// position lands on the first frame rather than at open.
#[derive(Debug, Clone, PartialEq)]
pub struct Place {
    pub title: String,
    pub spine: usize,
    pub spine_len: usize,
    pub page: usize,
    pub page_count: usize,
    /// Whole-book progress, 0..=1, spine-weighted: each unit a
    /// `1/spine_len` slice, the page's place within it added — where this
    /// page *begins*, like the position it is read from, so a book opens
    /// at 0. The one exception is the last page of the last unit, which
    /// is the whole book: 1.0, so the percent readout says 100 exactly
    /// when `pages_left` says 0 with nothing after. Coarser than the
    /// character-weighted progression the library stores — the shelf's
    /// bar may disagree by a little — but every term is already in hand,
    /// so the bar needs no engine call of its own.
    pub book_fraction: f64,
    /// Whether the engine's Back has anywhere to go — after a link, a
    /// contents jump, a mark. What decides whether a *Return* is drawn.
    pub can_go_back: bool,
}

impl Place {
    /// Read the place off a session. Takes `&mut` because the page count
    /// lays the current unit out if nothing has yet.
    pub fn of(session: &mut Session) -> Place {
        let position = session.position();
        let spine_len = session.spine_len();
        let page_count = session.page_count();
        let within = if page_count > 0 {
            position.page as f64 / page_count as f64
        } else {
            0.0
        };
        let last_page = position.spine + 1 == spine_len && position.page + 1 == page_count;
        let book_fraction = if last_page {
            1.0
        } else if spine_len > 0 {
            ((position.spine as f64 + within) / spine_len as f64).clamp(0.0, 1.0)
        } else {
            0.0
        };
        Place {
            title: session.title().to_string(),
            spine: position.spine,
            spine_len,
            page: position.page,
            page_count,
            book_fraction,
            can_go_back: session.can_go_back(),
        }
    }

    /// Pages after this one in the current unit.
    pub fn pages_left(&self) -> usize {
        self.page_count.saturating_sub(self.page + 1)
    }

    /// The readout beside the progress bar, as the reader chose to see
    /// it. Numbers, not words: the front end has the strings table.
    pub fn readout(&self, label: ProgressLabel) -> Readout {
        match label {
            ProgressLabel::Percent => Readout::Percent((self.book_fraction * 100.0).round() as u8),
            ProgressLabel::PagesLeft => Readout::PagesLeft(self.pages_left()),
            ProgressLabel::ChapterPage => Readout::ChapterPage {
                unit: self.spine + 1,
                units: self.spine_len,
                page: self.page + 1,
                pages: self.page_count,
            },
        }
    }
}

/// What the progress readout says, beside the whole-book bar. The
/// reader's choice, kept as a preference by [`App`](crate::App).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProgressLabel {
    /// "34%" of the whole book.
    #[default]
    Percent,
    /// "6 left in chapter" — how many pages remain in this unit.
    PagesLeft,
    /// "unit 6/20 · page 2/11" — the raw indices.
    ChapterPage,
}

impl ProgressLabel {
    /// The stored form. Stable: it is what a preference row holds.
    pub fn as_str(self) -> &'static str {
        match self {
            ProgressLabel::Percent => "percent",
            ProgressLabel::PagesLeft => "pages_left",
            ProgressLabel::ChapterPage => "chapter_page",
        }
    }

    pub fn parse(value: &str) -> Option<ProgressLabel> {
        Some(match value {
            "percent" => ProgressLabel::Percent,
            "pages_left" => ProgressLabel::PagesLeft,
            "chapter_page" => ProgressLabel::ChapterPage,
            _ => return None,
        })
    }

    /// Every choice, in the order a settings sheet lists them.
    pub const ALL: [ProgressLabel; 3] = [
        ProgressLabel::Percent,
        ProgressLabel::PagesLeft,
        ProgressLabel::ChapterPage,
    ];
}

/// The numbers a progress readout shows, per [`ProgressLabel`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readout {
    Percent(u8),
    PagesLeft(usize),
    ChapterPage {
        /// One-based, as shown.
        unit: usize,
        units: usize,
        page: usize,
        pages: usize,
    },
}

// ---- Memory ----

/// The least a page cache is worth having: below this a page-back rereads
/// and re-lays out the chapter every time.
pub const MIN_CACHE_BUDGET: usize = 16 << 20;
/// The most a page cache is worth: past this a phone is holding chapters
/// nobody will page back to.
pub const MAX_CACHE_BUDGET: usize = 192 << 20;
/// Where halving stops: the page on screen and a little either side.
pub const MIN_CACHE_BUDGET_UNDER_PRESSURE: usize = 4 << 20;

/// How much of what the platform says this process may use goes to the
/// page cache: a quarter, between the two bounds above.
///
/// The number is the platform's to supply — `ActivityManager.memoryClass`
/// on Android, `os_proc_available_memory` on iOS, a fixed figure on a
/// device with no such callback — and the policy is the same for all of
/// them. The engine's own default is a desktop's and too generous for
/// any of those.
pub fn cache_budget_for(available_bytes: u64) -> usize {
    let quarter = (available_bytes / 4).min(usize::MAX as u64) as usize;
    quarter.clamp(MIN_CACHE_BUDGET, MAX_CACHE_BUDGET)
}

/// What a memory warning does: halve the budget, which evicts at once,
/// and release the caches. Halving rather than releasing alone keeps the
/// next warning from finding the same cache again; the floor keeps the
/// page on screen. Returns the budget now in force.
pub fn after_memory_warning(session: &mut Session) -> usize {
    let budget = (session.cache_budget() / 2).max(MIN_CACHE_BUDGET_UNDER_PRESSURE);
    session.set_cache_budget(budget);
    session.release_caches();
    budget
}

// ---- Opening ----

/// What is read once at open and never changes: the book's shape, its
/// contents flattened for a menu, and every typeface the session's fonts
/// can offer a picker. Read on the thread that opened, before the
/// hand-over to the thread that draws, because both cost a little.
#[derive(Debug, Clone)]
pub struct Reading {
    pub kind: BookKind,
    /// Each entry with its nesting depth, in reading order.
    pub contents: Vec<(usize, TocEntry)>,
    pub font_families: Vec<String>,
}

impl Reading {
    pub fn of(session: &Session) -> Reading {
        Reading {
            kind: session.kind(),
            contents: crate::flatten_toc(session.toc()),
            font_families: session.font_families(),
        }
    }
}

// ---- Search ----

/// Where a search stops accumulating: past this a results list is a
/// scroll nobody finishes, and the reader refines the query.
pub const SEARCH_CAP: usize = 200;

/// A search walked one unit at a time, on the thread that owns the
/// session, so the page stays responsive between steps.
///
/// The blocking whole-book `Session::search` would want a worker, and a
/// worker would touch the session while the page draws — the one rule
/// the engine has. So the walk is a cursor: the front end calls
/// [`step`](SearchWalk::step) from its own loop, yielding however its
/// platform yields between units, and reads the hits so far after each.
#[derive(Debug)]
pub struct SearchWalk {
    query: String,
    next: usize,
    hits: Vec<SearchHit>,
    done: bool,
}

impl SearchWalk {
    /// Begin a search, or `None` for a query that is only whitespace —
    /// which clears rather than searches.
    pub fn new(query: &str) -> Option<SearchWalk> {
        let query = query.trim();
        (!query.is_empty()).then(|| SearchWalk {
            query: query.to_string(),
            next: 0,
            hits: Vec::new(),
            done: false,
        })
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    /// Search the next unit. Returns whether there is more to do: `false`
    /// once the last unit is searched or the cap is reached, and on every
    /// call after.
    pub fn step(&mut self, session: &mut Session) -> bool {
        if self.done {
            return false;
        }
        let units = session.spine_len();
        if self.next >= units {
            self.done = true;
            return false;
        }
        self.hits
            .extend(session.search_unit(self.next, &self.query));
        self.next += 1;
        if self.hits.len() >= SEARCH_CAP || self.next >= units {
            self.done = true;
        }
        !self.done
    }

    /// Every hit found so far, in reading order.
    pub fn hits(&self) -> &[SearchHit] {
        &self.hits
    }

    pub fn is_done(&self) -> bool {
        self.done
    }
}

/// Jump to a hit and leave it selected, so the eye finds it — what a
/// results row does when tapped. Returns whether the position moved.
pub fn show_hit(session: &mut Session, hit: &SearchHit) -> bool {
    let moved = session.goto(Locator {
        spine_index: hit.locator.spine_index,
        char_offset: hit.locator.char_offset,
    });
    session.select_range(hit.locator.char_offset, hit.end);
    moved
}

// ---- Marks ----

/// The selection becomes a highlight, and the selection goes. `None`
/// when nothing was selected.
pub fn highlight_selection(session: &mut Session) -> Option<i64> {
    let id = session.add_highlight()?;
    session.selection_clear();
    Some(id)
}

/// The selection becomes a note, and the selection goes. `None` when
/// nothing was selected.
pub fn note_on_selection(session: &mut Session, body: &str) -> Option<i64> {
    let id = session.add_note(body)?;
    session.selection_clear();
    Some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quarter_of_the_platforms_number_within_bounds() {
        assert_eq!(cache_budget_for(256 << 20), 64 << 20);
        assert_eq!(cache_budget_for(16 << 20), MIN_CACHE_BUDGET, "floored");
        assert_eq!(cache_budget_for(0), MIN_CACHE_BUDGET);
        assert_eq!(cache_budget_for(4 << 30), MAX_CACHE_BUDGET, "capped");
        assert_eq!(cache_budget_for(u64::MAX), MAX_CACHE_BUDGET);
    }

    #[test]
    fn the_readout_is_numbers_the_front_end_words() {
        let place = Place {
            title: String::new(),
            spine: 5,
            spine_len: 20,
            page: 1,
            page_count: 11,
            book_fraction: 0.3409,
            can_go_back: false,
        };
        assert_eq!(place.readout(ProgressLabel::Percent), Readout::Percent(34));
        assert_eq!(
            place.readout(ProgressLabel::PagesLeft),
            Readout::PagesLeft(9)
        );
        assert_eq!(
            place.readout(ProgressLabel::ChapterPage),
            Readout::ChapterPage {
                unit: 6,
                units: 20,
                page: 2,
                pages: 11
            }
        );
        let last = Place { page: 10, ..place };
        assert_eq!(last.pages_left(), 0);
    }

    #[test]
    fn the_label_round_trips_its_stored_form() {
        for label in ProgressLabel::ALL {
            assert_eq!(ProgressLabel::parse(label.as_str()), Some(label));
        }
        assert_eq!(ProgressLabel::parse("nonsense"), None);
        assert_eq!(ProgressLabel::default(), ProgressLabel::Percent);
    }

    #[test]
    fn a_blank_query_is_not_a_search() {
        assert!(SearchWalk::new("   ").is_none());
        assert_eq!(SearchWalk::new(" word ").unwrap().query(), "word");
    }
}
