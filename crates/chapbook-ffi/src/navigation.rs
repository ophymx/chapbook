//! Getting somewhere on purpose: the table of contents, in-book search,
//! and the locator a position actually is.
//!
//! Everything here was reachable in Rust and nowhere else, which meant a
//! non-Rust host could turn pages and follow links but could not offer a
//! contents menu, a search box, or a "resume exactly here" — the three
//! things a reader reaches for when they know where they want to be.
//!
//! **The contents cross flattened.** A `TocEntry` is a tree, and a tree
//! across C is a shape every host then has to rebuild; every consumer of
//! one is a list with indentation. So the entries arrive in reading
//! order with a depth, which is what a menu draws directly and what a
//! host can still rebuild a tree from. Entries that link nowhere are
//! kept — they are section headings, and dropping them would orphan
//! their children's indentation.
//!
//! **A locator is two numbers**, a spine index and a character offset in
//! that unit's locator text, and it is the durable position — the one
//! the library stores, annotations anchor to, and sync carries.
//! [`cb_session_position`](crate::cb_session_position) reports the
//! *view*: which page of the current pagination, which moves when the
//! font size does. A host saving a place, sharing a quote, or restoring
//! one wants this.
//!
//! **Search results are held by the session** between the call that runs
//! the search and the calls that read it, on the same borrowed-string
//! rule as the event and sync drains: a hit's context lives until the
//! next search or the session closes.

use std::ffi::c_char;

use crate::abi::{str_in, str_out};
use crate::error::{cb_status, clear_last_error, fail, guard};
use crate::session::cb_session;

/// One table-of-contents entry, flattened. Its label travels on
/// [`cb_session_toc_label`].
#[repr(C)]
pub struct cb_toc_entry {
    /// Nesting depth: 0 for a top-level entry, 1 for its children.
    pub depth: usize,
    /// The spine unit it points at. Meaningful only when `has_spine`;
    /// a heading that links nowhere has none.
    pub spine: usize,
    pub has_spine: bool,
    /// Whether the entry points inside its unit rather than at its
    /// start — a fragment. Nothing a host must act on; the jump handles
    /// it either way.
    pub has_fragment: bool,
}

/// One search hit. Its context travels on
/// [`cb_session_search_context`].
#[repr(C)]
pub struct cb_search_hit {
    /// The unit the match is in.
    pub spine: usize,
    /// Locator offset of the match's first character, and just past its
    /// last — the range to hand
    /// [`cb_session_select_range`](crate::cb_session_select_range) after
    /// jumping, which is how a hit gets painted on the page.
    pub start: u32,
    pub end: u32,
    /// Char range of the match within the context string, so a results
    /// list can embolden the matched words rather than the whole line.
    pub match_start: u32,
    pub match_end: u32,
}

/// What a flattened row carries beyond its label.
struct TocRow {
    depth: usize,
    spine: Option<usize>,
    fragment: bool,
    label: String,
    /// The path back to the entry the session jumps by.
    path: Vec<usize>,
}

/// Walk the tree into rows, remembering the path to each so a jump can
/// find the original entry without cloning it.
fn toc_rows(session: &cb_session) -> Vec<TocRow> {
    fn walk(
        entries: &[chapbook_reader::chapbook_core::TocEntry],
        depth: usize,
        path: &mut Vec<usize>,
        out: &mut Vec<TocRow>,
    ) {
        for (index, entry) in entries.iter().enumerate() {
            path.push(index);
            out.push(TocRow {
                depth,
                spine: entry.spine_index,
                fragment: entry.fragment.is_some(),
                label: entry.label.clone(),
                path: path.clone(),
            });
            walk(&entry.children, depth + 1, path, out);
            path.pop();
        }
    }
    let mut out = Vec::new();
    walk(session.inner.toc(), 0, &mut Vec::new(), &mut out);
    out
}

/// How many entries the contents hold, flattened. Zero for a book with
/// none, which is ordinary — a comic has no contents.
#[no_mangle]
pub unsafe extern "C" fn cb_session_toc_count(
    session: *const cb_session,
    count: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        clear_last_error();
        // SAFETY: a handle from an open call, not yet closed.
        let Some(session) = (unsafe { session.as_ref() }) else {
            return fail(cb_status::CB_ERR_NULL_ARGUMENT, "session is null");
        };
        if count.is_null() {
            return fail(cb_status::CB_ERR_NULL_ARGUMENT, "count out-pointer is null");
        }
        // SAFETY: checked non-null just above.
        unsafe { *count = toc_rows(session).len() };
        cb_status::CB_OK
    })
}

/// One entry's plain data, by index.
#[no_mangle]
pub unsafe extern "C" fn cb_session_toc_entry(
    session: *const cb_session,
    index: usize,
    out: *mut cb_toc_entry,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        clear_last_error();
        // SAFETY: a handle from an open call, not yet closed.
        let Some(session) = (unsafe { session.as_ref() }) else {
            return fail(cb_status::CB_ERR_NULL_ARGUMENT, "session is null");
        };
        if out.is_null() {
            return fail(cb_status::CB_ERR_NULL_ARGUMENT, "out is null");
        }
        let rows = toc_rows(session);
        let Some(row) = rows.get(index) else {
            return fail(
                cb_status::CB_ERR_INVALID_ARGUMENT,
                format!("toc index {index} out of {}", rows.len()),
            );
        };
        let filled = cb_toc_entry {
            depth: row.depth,
            spine: row.spine.unwrap_or(0),
            has_spine: row.spine.is_some(),
            has_fragment: row.fragment,
        };
        // SAFETY: checked non-null above.
        unsafe { *out = filled };
        cb_status::CB_OK
    })
}

/// An entry's label — what a contents menu shows.
#[no_mangle]
pub unsafe extern "C" fn cb_session_toc_label(
    session: *const cb_session,
    index: usize,
    buf: *mut c_char,
    cap: usize,
    needed: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        clear_last_error();
        // SAFETY: a handle from an open call, not yet closed.
        let Some(session) = (unsafe { session.as_ref() }) else {
            return fail(cb_status::CB_ERR_NULL_ARGUMENT, "session is null");
        };
        let rows = toc_rows(session);
        let Some(row) = rows.get(index) else {
            return fail(
                cb_status::CB_ERR_INVALID_ARGUMENT,
                format!("toc index {index} out of {}", rows.len()),
            );
        };
        // SAFETY: the header's contract for the buffer triple.
        unsafe { str_out(&row.label, buf, cap, needed) }
    })
}

/// Jump to a contents entry by index. `*moved` is false for an entry
/// that links nowhere — a section heading — which is not an error and is
/// why a host may show them all.
///
/// The jump pushes the return position for the `Back` action, like a
/// followed link.
#[no_mangle]
pub unsafe extern "C" fn cb_session_goto_toc(
    session: *mut cb_session,
    index: usize,
    moved: *mut bool,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        clear_last_error();
        // SAFETY: a handle from an open call, not yet closed.
        let Some(session) = (unsafe { session.as_mut() }) else {
            return fail(cb_status::CB_ERR_NULL_ARGUMENT, "session is null");
        };
        if moved.is_null() {
            return fail(cb_status::CB_ERR_NULL_ARGUMENT, "moved out-pointer is null");
        }
        let path = {
            let rows = toc_rows(session);
            let Some(row) = rows.get(index) else {
                return fail(
                    cb_status::CB_ERR_INVALID_ARGUMENT,
                    format!("toc index {index} out of {}", rows.len()),
                );
            };
            row.path.clone()
        };
        // Walk the path back to the entry itself: `goto_toc` takes the
        // entry, and cloning one to hand it over would clone its whole
        // subtree.
        let mut entries: &[chapbook_reader::chapbook_core::TocEntry] = session.inner.toc();
        let mut found = None;
        for (step, child) in path.iter().enumerate() {
            let Some(entry) = entries.get(*child) else {
                break;
            };
            if step + 1 == path.len() {
                found = Some(entry.clone());
            } else {
                entries = &entry.children;
            }
        }
        let Some(entry) = found else {
            return fail(cb_status::CB_ERR_UNAVAILABLE, "the entry went away");
        };
        let did = session.inner.goto_toc(&entry);
        // SAFETY: checked non-null above.
        unsafe { *moved = did };
        cb_status::CB_OK
    })
}

// ---- Search ----

/// Search the whole book, keeping at most `limit` hits (0 for a sane
/// cap). `*count` is how many were found.
///
/// Blocking and potentially slow: it lays out nothing, but it reads and
/// folds every unit's text, so a shell with a responsive search box runs
/// it off its UI thread or walks units itself with
/// [`cb_session_search_unit`].
///
/// The hits are held until the next search or the session's close; read
/// them with [`cb_session_search_hit`] and
/// [`cb_session_search_context`].
#[no_mangle]
pub unsafe extern "C" fn cb_session_search(
    session: *mut cb_session,
    query: *const c_char,
    limit: usize,
    count: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        clear_last_error();
        // SAFETY: a handle from an open call, not yet closed.
        let Some(session) = (unsafe { session.as_mut() }) else {
            return fail(cb_status::CB_ERR_NULL_ARGUMENT, "session is null");
        };
        // SAFETY: the header's contract.
        let Some(query) = (unsafe { str_in(query, "query") }) else {
            return cb_status::CB_ERR_NULL_ARGUMENT;
        };
        if count.is_null() {
            return fail(cb_status::CB_ERR_NULL_ARGUMENT, "count out-pointer is null");
        }
        let limit = if limit == 0 { 500 } else { limit };
        session.hits = session.inner.search(query, limit);
        // SAFETY: checked non-null above.
        unsafe { *count = session.hits.len() };
        cb_status::CB_OK
    })
}

/// Search one unit — the worker-drivable half, for a shell that wants
/// results as they arrive rather than after the whole book. Replaces
/// whatever the last search left, same as [`cb_session_search`].
#[no_mangle]
pub unsafe extern "C" fn cb_session_search_unit(
    session: *mut cb_session,
    spine: usize,
    query: *const c_char,
    count: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        clear_last_error();
        // SAFETY: a handle from an open call, not yet closed.
        let Some(session) = (unsafe { session.as_mut() }) else {
            return fail(cb_status::CB_ERR_NULL_ARGUMENT, "session is null");
        };
        // SAFETY: the header's contract.
        let Some(query) = (unsafe { str_in(query, "query") }) else {
            return cb_status::CB_ERR_NULL_ARGUMENT;
        };
        if count.is_null() {
            return fail(cb_status::CB_ERR_NULL_ARGUMENT, "count out-pointer is null");
        }
        if spine >= session.inner.spine_len() {
            return fail(
                cb_status::CB_ERR_INVALID_ARGUMENT,
                format!("spine index {spine} out of {}", session.inner.spine_len()),
            );
        }
        session.hits = session.inner.search_unit(spine, query);
        // SAFETY: checked non-null above.
        unsafe { *count = session.hits.len() };
        cb_status::CB_OK
    })
}

/// One hit's plain data, by index into the last search's results.
#[no_mangle]
pub unsafe extern "C" fn cb_session_search_hit(
    session: *const cb_session,
    index: usize,
    out: *mut cb_search_hit,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        clear_last_error();
        // SAFETY: a handle from an open call, not yet closed.
        let Some(session) = (unsafe { session.as_ref() }) else {
            return fail(cb_status::CB_ERR_NULL_ARGUMENT, "session is null");
        };
        if out.is_null() {
            return fail(cb_status::CB_ERR_NULL_ARGUMENT, "out is null");
        }
        let Some(hit) = session.hits.get(index) else {
            return fail(
                cb_status::CB_ERR_INVALID_ARGUMENT,
                format!("hit index {index} out of {}", session.hits.len()),
            );
        };
        let filled = cb_search_hit {
            spine: hit.locator.spine_index,
            start: hit.locator.char_offset,
            end: hit.end,
            match_start: hit.match_range.0,
            match_end: hit.match_range.1,
        };
        // SAFETY: checked non-null above.
        unsafe { *out = filled };
        cb_status::CB_OK
    })
}

/// A hit's context: the match with a little text either side,
/// whitespace collapsed, for a results list.
#[no_mangle]
pub unsafe extern "C" fn cb_session_search_context(
    session: *const cb_session,
    index: usize,
    buf: *mut c_char,
    cap: usize,
    needed: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        clear_last_error();
        // SAFETY: a handle from an open call, not yet closed.
        let Some(session) = (unsafe { session.as_ref() }) else {
            return fail(cb_status::CB_ERR_NULL_ARGUMENT, "session is null");
        };
        let Some(hit) = session.hits.get(index) else {
            return fail(
                cb_status::CB_ERR_INVALID_ARGUMENT,
                format!("hit index {index} out of {}", session.hits.len()),
            );
        };
        // SAFETY: the header's contract for the buffer triple.
        unsafe { str_out(&hit.context, buf, cap, needed) }
    })
}

// ---- The locator: where the reader actually is ----

/// The reader's position as a locator — a spine index and a character
/// offset into that unit's text.
///
/// This is the *durable* position, the one the library stores, marks
/// anchor to and sync carries, and it does not move when the font size
/// does. [`cb_session_position`](crate::cb_session_position) reports the
/// view — which page of the current pagination — and that one does.
#[no_mangle]
pub unsafe extern "C" fn cb_session_locator(
    session: *const cb_session,
    spine: *mut usize,
    offset: *mut u32,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        clear_last_error();
        // SAFETY: a handle from an open call, not yet closed.
        let Some(session) = (unsafe { session.as_ref() }) else {
            return fail(cb_status::CB_ERR_NULL_ARGUMENT, "session is null");
        };
        if spine.is_null() || offset.is_null() {
            return fail(cb_status::CB_ERR_NULL_ARGUMENT, "an out-pointer is null");
        }
        let locator = session.inner.locator();
        // SAFETY: both checked non-null just above.
        unsafe {
            *spine = locator.spine_index;
            *offset = locator.char_offset;
        }
        cb_status::CB_OK
    })
}

/// Jump to a locator — a place saved earlier, a hit from a search, a
/// position another device reached. `*moved` is false for a spine index
/// the book does not have; an offset past the unit's text lands at its
/// end rather than failing.
///
/// The jump pushes the return position for the `Back` action.
#[no_mangle]
pub unsafe extern "C" fn cb_session_goto(
    session: *mut cb_session,
    spine: usize,
    offset: u32,
    moved: *mut bool,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        clear_last_error();
        // SAFETY: a handle from an open call, not yet closed.
        let Some(session) = (unsafe { session.as_mut() }) else {
            return fail(cb_status::CB_ERR_NULL_ARGUMENT, "session is null");
        };
        if moved.is_null() {
            return fail(cb_status::CB_ERR_NULL_ARGUMENT, "moved out-pointer is null");
        }
        let did = session
            .inner
            .goto(chapbook_reader::chapbook_core::Locator::new(spine, offset));
        // SAFETY: checked non-null above.
        unsafe { *moved = did };
        cb_status::CB_OK
    })
}

/// Jump to an element id within a unit — a footnote, a cross-reference,
/// a contents fragment. A fragment the unit does not carry lands at the
/// unit's start rather than failing; `*moved` is false only for a spine
/// index the book does not have.
#[no_mangle]
pub unsafe extern "C" fn cb_session_goto_anchor(
    session: *mut cb_session,
    spine: usize,
    fragment: *const c_char,
    moved: *mut bool,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        clear_last_error();
        // SAFETY: a handle from an open call, not yet closed.
        let Some(session) = (unsafe { session.as_mut() }) else {
            return fail(cb_status::CB_ERR_NULL_ARGUMENT, "session is null");
        };
        // SAFETY: the header's contract.
        let Some(fragment) = (unsafe { str_in(fragment, "fragment") }) else {
            return cb_status::CB_ERR_NULL_ARGUMENT;
        };
        if moved.is_null() {
            return fail(cb_status::CB_ERR_NULL_ARGUMENT, "moved out-pointer is null");
        }
        let did = session.inner.goto_anchor(spine, fragment);
        // SAFETY: checked non-null above.
        unsafe { *moved = did };
        cb_status::CB_OK
    })
}

/// Whether the `Back` action has anywhere to return to — what greys out
/// a back button. The action itself is
/// [`cb_session_apply`](crate::cb_session_apply) with `CB_ACTION_BACK`;
/// this is the question a host cannot otherwise ask.
#[no_mangle]
pub unsafe extern "C" fn cb_session_can_go_back(
    session: *const cb_session,
    can: *mut bool,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        clear_last_error();
        // SAFETY: a handle from an open call, not yet closed.
        let Some(session) = (unsafe { session.as_ref() }) else {
            return fail(cb_status::CB_ERR_NULL_ARGUMENT, "session is null");
        };
        if can.is_null() {
            return fail(cb_status::CB_ERR_NULL_ARGUMENT, "can out-pointer is null");
        }
        // SAFETY: checked non-null just above.
        unsafe { *can = session.inner.can_go_back() };
        cb_status::CB_OK
    })
}
