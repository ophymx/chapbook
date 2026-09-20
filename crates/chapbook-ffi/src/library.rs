//! The shelf: browsing the library a session reads from.
//!
//! Everything before this crossed *one open book*. An app is more than a
//! reading view — it opens onto a shelf, and a host could not draw one:
//! `cb_session` knew the library existed and never let anyone look at it.
//!
//! # Two handles, and what each owns
//!
//! [`cb_library`] is the connection. It is `Send` and not `Sync` for the
//! same reason [`cb_session`] is — SQLite's connection is one thread's at
//! a time — and a host may hold one *while* a session holds its own,
//! because the database is WAL and concurrent readers are the ordinary
//! way to have two. That is what makes a shelf drawable while a book is
//! open.
//!
//! [`cb_shelf`] is one query's answer, held still. It could have been
//! state on the library handle, the way the page's text runs are state on
//! the session — and deliberately is not. A shelf UI holds a page of
//! results *while* the reader types in a search box, and an implicit
//! last-query slot would invalidate the results being drawn from under
//! the draw. It owns its rows outright and outlives the library handle if
//! a host closes that first.
//!
//! # Reading a shelf
//!
//! [`cb_shelf_book`] gives the plain-data half of a row in one copy, and
//! the strings come from accessors beside it — the same split as
//! [`cb_text_run`](crate::cb_text_run), and for the same reason: nothing
//! crosses owned, so a string is written into the caller's buffer rather
//! than handed over.
//!
//! # Zero is the unfiltered query
//!
//! Every narrowing field in [`cb_book_query`] treats zero or null as "do
//! not narrow", so `cb_book_query query = {0};` is the whole shelf,
//! newest first. A C caller should not have to fill in seven fields to
//! ask for everything.
//!
//! # Without the `library` feature
//!
//! The symbols are all here and every one of them declines, the way the
//! HTTP transport does without `opds`. A host asks
//! [`cb_capabilities`](crate::cb_capabilities) for `CB_CAP_LIBRARY`
//! rather than discovering the answer from a missing symbol at `dlopen`.

use std::ffi::c_char;

// Everything below the feature line is unreachable without it: the
// entry points still exist and decline, and their working bodies are
// `cfg`'d away entire.
#[cfg(feature = "library")]
use crate::abi::{slice_out, str_in, str_out};
#[cfg(feature = "library")]
use crate::error::clear_last_error;
use crate::error::{cb_status, fail, guard};
use crate::session::cb_session;

/// An open library. Opaque.
///
/// Movable between threads, never used from two at once — the same rule
/// as [`cb_session`], and for a stricter reason: the SQLite connection
/// underneath is not shareable at all.
pub struct cb_library {
    #[cfg(feature = "library")]
    pub(crate) inner: chapbook_reader::chapbook_library::Library,
}

/// One query's rows, held still until freed. Opaque.
pub struct cb_shelf {
    #[cfg(feature = "library")]
    pub(crate) books: Vec<chapbook_reader::chapbook_library::BookRecord>,
}

/// How far through a book the reader is.
///
/// `CB_STATE_ANY` is a query saying "do not narrow by state"; a row never
/// reports it.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum cb_reading_state {
    CB_STATE_ANY = 0,
    /// Never opened.
    CB_STATE_UNREAD = 1,
    /// Opened, not finished — what a "continue reading" row wants.
    CB_STATE_READING = 2,
    /// Reached the end at least once, whatever the position says now.
    CB_STATE_FINISHED = 3,
}

/// How a query orders its rows.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum cb_sort {
    /// Newest addition first: a stable listing, and the zero value.
    CB_SORT_ADDED = 0,
    /// The most recent thing that happened to this book, read or added —
    /// what a shelf shows first.
    CB_SORT_READ = 1,
    CB_SORT_TITLE = 2,
    /// First author, then title. A book with no author sorts last.
    CB_SORT_AUTHOR = 3,
    /// Series, then position within it. Books in no series sort last.
    CB_SORT_SERIES = 4,
}

/// What to list, and in what order. Zero-initialize for the whole shelf.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct cb_book_query {
    /// Free text over title, authors and series, or null for no text
    /// filter. Whole words matched by prefix, folded for case *and*
    /// accents — "bronte" finds Brontë. Text holding nothing searchable
    /// (punctuation alone) matches no book rather than every book.
    pub search: *const c_char,
    /// Only this series, matched exactly but case-folded, or null for
    /// any. The value comes from a row, not from typing.
    pub series: *const c_char,
    /// Only books in this collection; 0 for any.
    pub collection: i64,
    pub state: cb_reading_state,
    pub sort: cb_sort,
    /// How many rows to return; 0 for all of them.
    pub limit: usize,
    /// How many to skip — the other half of paging a long shelf.
    pub offset: usize,
}

/// One shelf row's plain data. The strings are beside it: see
/// [`cb_shelf_title`] and its neighbours.
///
/// Timestamps are Unix seconds, and 0 means *never* rather than 1970 —
/// no row here is from before the epoch.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct cb_book {
    /// The library id. Stable across re-imports and re-anchoring, and
    /// what every other call in this module takes.
    pub id: i64,
    pub added_at: i64,
    /// When the position was last written; 0 for a book never opened.
    pub last_read: i64,
    /// When the reader reached the end; 0 if they have not.
    pub finished_at: i64,
    /// How far through, 0.0..=1.0. Meaningful only when `has_progress`.
    ///
    /// Not what `state` is derived from, and not a substitute for it: a
    /// book skimmed to the last page reads 1.0 without being finished,
    /// and a finished book reopened reads near 0 without being unread.
    pub progress: f64,
    /// Where in its series. Meaningful only when `has_series_index`, and
    /// fractional on purpose — a novella between books two and three is
    /// conventionally 2.5.
    pub series_index: f64,
    pub state: cb_reading_state,
    /// Authors, read with [`cb_shelf_author`].
    pub author_count: usize,
    /// Collections, read with [`cb_shelf_collection_id`] and
    /// [`cb_shelf_collection_name`].
    pub collection_count: usize,
    pub has_progress: bool,
    pub has_series_index: bool,
    /// Whether [`cb_shelf_cover_path`] has anything to give. Not every
    /// CBZ or PDF carries a cover, and a shelf falls back to a title
    /// card.
    pub has_cover: bool,
}

/// One collection: a named set of books.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct cb_collection {
    pub id: i64,
    pub added_at: i64,
    /// How many books are in it. A list of shelf names that does not say
    /// how many books are on each is a list of words.
    pub books: usize,
}

/// The two configurations every entry point here has: with the `library`
/// feature it does the thing, and without it there is no shelf to do it
/// to.
///
/// Written once rather than twenty-five times. The `cfg`'d-out arm is
/// removed before type checking, which is why the body may name
/// `chapbook_library` freely even in a build that does not have it — and
/// why the unused arguments have to be named, since the other arm never
/// reads them.
macro_rules! with_library {
    (($($unused:ident),* $(,)?) $body:block) => {{
        #[cfg(feature = "library")]
        $body
        #[cfg(not(feature = "library"))]
        {
            $(let _ = $unused;)*
            fail(
                cb_status::CB_ERR_FORMAT_NOT_BUILT,
                "this build has no library, so there is no shelf to read",
            )
        }
    }};
}

#[cfg(feature = "library")]
macro_rules! library_mut {
    ($library:expr) => {
        // SAFETY: a handle from `cb_library_open`, not yet closed.
        match unsafe { $library.as_mut() } {
            Some(library) => library,
            None => return fail(cb_status::CB_ERR_NULL_ARGUMENT, "library is null"),
        }
    };
}

#[cfg(feature = "library")]
macro_rules! library_ref {
    ($library:expr) => {
        // SAFETY: a handle from `cb_library_open`, not yet closed.
        match unsafe { $library.as_ref() } {
            Some(library) => library,
            None => return fail(cb_status::CB_ERR_NULL_ARGUMENT, "library is null"),
        }
    };
}

/// A row of a shelf, or the range error naming what went wrong.
#[cfg(feature = "library")]
macro_rules! row {
    ($shelf:expr, $index:expr) => {{
        // SAFETY: a handle from `cb_library_query`, not yet freed.
        let shelf = match unsafe { $shelf.as_ref() } {
            Some(shelf) => shelf,
            None => return fail(cb_status::CB_ERR_NULL_ARGUMENT, "shelf is null"),
        };
        match shelf.books.get($index) {
            Some(book) => book,
            None => {
                return fail(
                    cb_status::CB_ERR_INVALID_ARGUMENT,
                    format!(
                        "book index {} out of range: the shelf holds {}",
                        $index,
                        shelf.books.len()
                    ),
                )
            }
        }
    }};
}

#[cfg(feature = "library")]
macro_rules! out {
    ($ptr:expr, $value:expr, $what:literal) => {{
        if $ptr.is_null() {
            return fail(
                cb_status::CB_ERR_NULL_ARGUMENT,
                concat!($what, " out-pointer is null"),
            );
        }
        // SAFETY: checked non-null just above.
        unsafe { *$ptr = $value };
    }};
}

// ---- Opening ----

/// Open (creating if needed) the library at `dir`.
///
/// Pass null for `dir` to use this platform's own location — the same one
/// [`cb_library_default_dir`] reports, and the same one a session opened
/// without [`cb_config_set_library_dir`](crate::cb_config_set_library_dir)
/// uses. On a platform with no such convention (Android, iOS, wasm) a null
/// `dir` is an error rather than a guess: a sandbox knows its own answer
/// and has to say it.
#[no_mangle]
pub unsafe extern "C" fn cb_library_open(
    dir: *const c_char,
    out: *mut *mut cb_library,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_library!((dir, out) {
            use chapbook_reader::chapbook_library::Library;

            clear_last_error();
            if out.is_null() {
                return fail(cb_status::CB_ERR_NULL_ARGUMENT, "out-pointer is null");
            }
            let path = if dir.is_null() {
                match Library::default_dir() {
                    Ok(path) => path,
                    Err(e) => return crate::error::from_error(&e),
                }
            } else {
                // SAFETY: the header's contract for a `const char*`.
                match unsafe { str_in(dir, "library directory") } {
                    Some(dir) => std::path::PathBuf::from(dir),
                    None => return cb_status::CB_ERR_INVALID_UTF8,
                }
            };
            match Library::open(&path) {
                Ok(inner) => {
                    let handle = Box::into_raw(Box::new(cb_library { inner }));
                    // SAFETY: checked non-null above.
                    unsafe { *out = handle };
                    cb_status::CB_OK
                }
                Err(e) => crate::error::from_error(&e),
            }
        })
    })
}

/// Close a library. Accepts null.
#[no_mangle]
pub unsafe extern "C" fn cb_library_close(library: *mut cb_library) {
    guard((), || {
        if !library.is_null() {
            // SAFETY: a handle from `cb_library_open`, closed once.
            drop(unsafe { Box::from_raw(library) });
        }
    })
}

/// Where this platform keeps a per-user library, as a path.
///
/// `CB_ERR_UNAVAILABLE` where there is no convention to follow — every
/// mobile and wasm target, and any Unix with no `HOME`. That is not a
/// gap: a sandboxed platform knows its own container and passes it to
/// [`cb_library_open`] or
/// [`cb_config_set_library_dir`](crate::cb_config_set_library_dir).
#[no_mangle]
pub unsafe extern "C" fn cb_library_default_dir(
    buf: *mut c_char,
    cap: usize,
    needed: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_library!((buf, cap, needed) {
            match chapbook_reader::chapbook_library::Library::default_dir() {
                // SAFETY: the header's contract for the buffer triple.
                Ok(path) => unsafe { str_out(&path.to_string_lossy(), buf, cap, needed) },
                Err(e) => crate::error::from_error(&e),
            }
        })
    })
}

// ---- Browsing ----

/// Run a query and hold its rows. Free the result with [`cb_shelf_free`].
///
/// An empty shelf is `CB_OK` with a length of zero, not an error: a
/// filter matching nothing is an answer.
#[no_mangle]
pub unsafe extern "C" fn cb_library_query(
    library: *const cb_library,
    query: *const cb_book_query,
    out: *mut *mut cb_shelf,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_library!((library, query, out) {
            use chapbook_reader::chapbook_library::{BookQuery, CollectionId};

            clear_last_error();
            let library = library_ref!(library);
            if out.is_null() {
                return fail(cb_status::CB_ERR_NULL_ARGUMENT, "out-pointer is null");
            }
            if query.is_null() {
                return fail(cb_status::CB_ERR_NULL_ARGUMENT, "query is null");
            }
            // SAFETY: checked non-null; the caller's struct outlives the call.
            let query = unsafe { *query };

            let search = if query.search.is_null() {
                None
            } else {
                // SAFETY: the header's contract for a `const char*`.
                match unsafe { str_in(query.search, "search text") } {
                    Some(text) => Some(text),
                    None => return cb_status::CB_ERR_INVALID_UTF8,
                }
            };
            let series = if query.series.is_null() {
                None
            } else {
                // SAFETY: the header's contract for a `const char*`.
                match unsafe { str_in(query.series, "series") } {
                    Some(text) => Some(text),
                    None => return cb_status::CB_ERR_INVALID_UTF8,
                }
            };

            let result = library.inner.query(&BookQuery {
                search,
                series,
                collection: (query.collection != 0).then_some(CollectionId(query.collection)),
                state: state_in(query.state),
                sort: sort_in(query.sort),
                // Zero is "all of them", so that a zero-initialized query
                // is the whole shelf rather than none of it.
                limit: (query.limit != 0).then_some(query.limit),
                offset: query.offset,
            });
            match result {
                Ok(books) => {
                    let handle = Box::into_raw(Box::new(cb_shelf { books }));
                    // SAFETY: checked non-null above.
                    unsafe { *out = handle };
                    cb_status::CB_OK
                }
                Err(e) => crate::error::from_error(&e),
            }
        })
    })
}

/// Release a shelf. Accepts null.
#[no_mangle]
pub unsafe extern "C" fn cb_shelf_free(shelf: *mut cb_shelf) {
    guard((), || {
        if !shelf.is_null() {
            // SAFETY: a handle from `cb_library_query`, freed once.
            drop(unsafe { Box::from_raw(shelf) });
        }
    })
}

/// How many rows the shelf holds.
#[no_mangle]
pub unsafe extern "C" fn cb_shelf_len(shelf: *const cb_shelf, len: *mut usize) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_library!((shelf, len) {
            // SAFETY: a handle from `cb_library_query`, not yet freed.
            let shelf = match unsafe { shelf.as_ref() } {
                Some(shelf) => shelf,
                None => return fail(cb_status::CB_ERR_NULL_ARGUMENT, "shelf is null"),
            };
            out!(len, shelf.books.len(), "len");
            cb_status::CB_OK
        })
    })
}

/// One row's plain data, by index. `CB_ERR_INVALID_ARGUMENT` past the end.
#[no_mangle]
pub unsafe extern "C" fn cb_shelf_book(
    shelf: *const cb_shelf,
    index: usize,
    book: *mut cb_book,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_library!((shelf, index, book) {
            let found = row!(shelf, index);
            out!(book, book_out(found), "book");
            cb_status::CB_OK
        })
    })
}

/// One row's title. Caller-allocates; see
/// [`cb_last_error_message`](crate::cb_last_error_message) for the
/// two-call idiom.
#[no_mangle]
pub unsafe extern "C" fn cb_shelf_title(
    shelf: *const cb_shelf,
    index: usize,
    buf: *mut c_char,
    cap: usize,
    needed: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_library!((shelf, index, buf, cap, needed) {
            let found = row!(shelf, index);
            // SAFETY: the header's contract for the buffer triple.
            unsafe { str_out(&found.title, buf, cap, needed) }
        })
    })
}

/// One row's author, by index within that row — the order the book lists
/// them, which is not alphabetical and is not arbitrary.
/// `CB_ERR_INVALID_ARGUMENT` past `author_count`.
#[no_mangle]
pub unsafe extern "C" fn cb_shelf_author(
    shelf: *const cb_shelf,
    index: usize,
    author: usize,
    buf: *mut c_char,
    cap: usize,
    needed: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_library!((shelf, index, author, buf, cap, needed) {
            let found = row!(shelf, index);
            let Some(name) = found.authors.get(author) else {
                return fail(
                    cb_status::CB_ERR_INVALID_ARGUMENT,
                    format!(
                        "author index {author} out of range: the book has {}",
                        found.authors.len()
                    ),
                );
            };
            // SAFETY: the header's contract for the buffer triple.
            unsafe { str_out(name, buf, cap, needed) }
        })
    })
}

/// One row's series. `CB_ERR_UNAVAILABLE` for a book in none, which is
/// most of them and is not a failure.
#[no_mangle]
pub unsafe extern "C" fn cb_shelf_series(
    shelf: *const cb_shelf,
    index: usize,
    buf: *mut c_char,
    cap: usize,
    needed: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_library!((shelf, index, buf, cap, needed) {
            let found = row!(shelf, index);
            let Some(series) = &found.series else {
                return fail(cb_status::CB_ERR_UNAVAILABLE, "this book names no series");
            };
            // SAFETY: the header's contract for the buffer triple.
            unsafe { str_out(series, buf, cap, needed) }
        })
    })
}

/// One row's language tag. `CB_ERR_UNAVAILABLE` if the book declares none.
#[no_mangle]
pub unsafe extern "C" fn cb_shelf_language(
    shelf: *const cb_shelf,
    index: usize,
    buf: *mut c_char,
    cap: usize,
    needed: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_library!((shelf, index, buf, cap, needed) {
            let found = row!(shelf, index);
            let Some(language) = &found.language else {
                return fail(cb_status::CB_ERR_UNAVAILABLE, "this book declares no language");
            };
            // SAFETY: the header's contract for the buffer triple.
            unsafe { str_out(language, buf, cap, needed) }
        })
    })
}

/// One row's publication identifier — an ISBN, a UUID, whatever the book
/// declared. `CB_ERR_UNAVAILABLE` if it declared none.
#[no_mangle]
pub unsafe extern "C" fn cb_shelf_identifier(
    shelf: *const cb_shelf,
    index: usize,
    buf: *mut c_char,
    cap: usize,
    needed: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_library!((shelf, index, buf, cap, needed) {
            let found = row!(shelf, index);
            let Some(identifier) = &found.identifier else {
                return fail(
                    cb_status::CB_ERR_UNAVAILABLE,
                    "this book declares no identifier",
                );
            };
            // SAFETY: the header's contract for the buffer triple.
            unsafe { str_out(identifier, buf, cap, needed) }
        })
    })
}

/// One row's edition fingerprint: the SHA-1 of the file's bytes, hex.
///
/// The key a host maps its own handle to — an Android `content://` grant,
/// an iOS security-scoped bookmark — because it is what identifies the
/// *file* across a reinstall, while the id identifies the reader's
/// history of it.
#[no_mangle]
pub unsafe extern "C" fn cb_shelf_fingerprint(
    shelf: *const cb_shelf,
    index: usize,
    buf: *mut c_char,
    cap: usize,
    needed: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_library!((shelf, index, buf, cap, needed) {
            let found = row!(shelf, index);
            // SAFETY: the header's contract for the buffer triple.
            unsafe { str_out(&found.fingerprint, buf, cap, needed) }
        })
    })
}

/// The library's own copy of the file.
///
/// Empty for an *adopted* book — one the library holds a record of and no
/// copy of, because the platform owns the file and the host owns the
/// means of reaching it again. An empty answer is `CB_OK`: the row is
/// fine and the host is the one that knows how to open it.
#[no_mangle]
pub unsafe extern "C" fn cb_shelf_file_path(
    shelf: *const cb_shelf,
    index: usize,
    buf: *mut c_char,
    cap: usize,
    needed: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_library!((shelf, index, buf, cap, needed) {
            let found = row!(shelf, index);
            // SAFETY: the header's contract for the buffer triple.
            unsafe { str_out(&found.file_path.to_string_lossy(), buf, cap, needed) }
        })
    })
}

/// The cover image on disk, kept at import so a shelf need not reopen
/// every book to draw one. `CB_ERR_UNAVAILABLE` when `has_cover` is
/// false.
#[no_mangle]
pub unsafe extern "C" fn cb_shelf_cover_path(
    shelf: *const cb_shelf,
    index: usize,
    buf: *mut c_char,
    cap: usize,
    needed: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_library!((shelf, index, buf, cap, needed) {
            let found = row!(shelf, index);
            let Some(cover) = &found.cover_path else {
                return fail(cb_status::CB_ERR_UNAVAILABLE, "this book has no cover");
            };
            // SAFETY: the header's contract for the buffer triple.
            unsafe { str_out(&cover.to_string_lossy(), buf, cap, needed) }
        })
    })
}

/// The id of one collection this row is in, by index within the row.
#[no_mangle]
pub unsafe extern "C" fn cb_shelf_collection_id(
    shelf: *const cb_shelf,
    index: usize,
    which: usize,
    id: *mut i64,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_library!((shelf, index, which, id) {
            let found = row!(shelf, index);
            let Some(collection) = found.collections.get(which) else {
                return fail(
                    cb_status::CB_ERR_INVALID_ARGUMENT,
                    format!(
                        "collection index {which} out of range: the book is in {}",
                        found.collections.len()
                    ),
                );
            };
            out!(id, collection.id.0, "id");
            cb_status::CB_OK
        })
    })
}

/// The name of one collection this row is in, by the same index.
#[no_mangle]
pub unsafe extern "C" fn cb_shelf_collection_name(
    shelf: *const cb_shelf,
    index: usize,
    which: usize,
    buf: *mut c_char,
    cap: usize,
    needed: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_library!((shelf, index, which, buf, cap, needed) {
            let found = row!(shelf, index);
            let Some(collection) = found.collections.get(which) else {
                return fail(
                    cb_status::CB_ERR_INVALID_ARGUMENT,
                    format!(
                        "collection index {which} out of range: the book is in {}",
                        found.collections.len()
                    ),
                );
            };
            // SAFETY: the header's contract for the buffer triple.
            unsafe { str_out(&collection.name, buf, cap, needed) }
        })
    })
}

// ---- Books ----

/// Take a book off the shelf.
///
/// Soft: the row keeps its id, its annotations and its position, so
/// adding the same file back is the same book with its marks intact.
#[no_mangle]
pub unsafe extern "C" fn cb_library_delete_book(library: *mut cb_library, book: i64) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_library!((library, book) {
            use chapbook_reader::chapbook_library::BookId;
            clear_last_error();
            let library = library_mut!(library);
            match library.inner.delete_book(BookId(book)) {
                Ok(()) => cb_status::CB_OK,
                Err(e) => crate::error::from_error(&e),
            }
        })
    })
}

/// Mark a book finished, or take the mark back.
///
/// A session records this itself when the reader reaches the end, so a
/// host needs it for the other direction: the "mark as read" a reader
/// taps on a book they finished elsewhere, and the undo. Marking twice
/// keeps the first timestamp.
#[no_mangle]
pub unsafe extern "C" fn cb_library_set_finished(
    library: *mut cb_library,
    book: i64,
    finished: bool,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_library!((library, book, finished) {
            use chapbook_reader::chapbook_library::BookId;
            clear_last_error();
            let library = library_mut!(library);
            match library.inner.set_finished(BookId(book), finished) {
                Ok(()) => cb_status::CB_OK,
                Err(e) => crate::error::from_error(&e),
            }
        })
    })
}

/// Put a file on the shelf, answering with the library row it became.
///
/// The format is decided by sniffing the bytes, so the name and
/// extension do not matter — which is what lets a host hand over
/// whatever its platform produced: a `content://` copy, a
/// `URLSession` temp file under a UUID, a file the reader picked out of
/// a document browser.
///
/// **The file is not consumed.** The library copies what it imports, and
/// this never deletes or moves the source: it belongs to whoever passed
/// it. Compare [`cb_catalog_download`](crate::cb_catalog_download),
/// which removes the staging file it made itself.
///
/// **Importing the same bytes twice is not an error.** Books are
/// identified by content fingerprint, so a second import answers with
/// the row the first one made rather than shelving a duplicate. That
/// matters for a background transfer: a job system that retries, or one
/// whose completion is delivered twice, does not need to coordinate with
/// this call to stay correct.
///
/// This shelves the file and nothing else. A book downloaded from a
/// catalog also has sync services to record, and those live in the
/// catalog entry rather than in the file — read them with
/// [`CB_ENTRY_PROGRESSION_URL`](crate::cb_entry_field::CB_ENTRY_PROGRESSION_URL)
/// and its neighbour before the transfer starts, then hand them to
/// [`cb_library_set_sync_targets`] once this has returned an id.
#[no_mangle]
pub unsafe extern "C" fn cb_library_import_file(
    library: *mut cb_library,
    path: *const c_char,
    book_id: *mut i64,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_library!((library, path, book_id) {
            clear_last_error();
            let library = library_mut!(library);
            // SAFETY: the header's contract for a `const char*`.
            let Some(path) = (unsafe { str_in(path, "path") }) else {
                return cb_status::CB_ERR_NULL_ARGUMENT;
            };
            let path = std::path::Path::new(path);
            let publication = match chapbook_reader::open_publication(path) {
                Ok(publication) => publication,
                Err(e) => return crate::error::from_error(&e),
            };
            match library.inner.import(path, publication.as_ref()) {
                Ok(id) => {
                    out!(book_id, id.0, "book_id");
                    cb_status::CB_OK
                }
                Err(e) => crate::error::from_error(&e),
            }
        })
    })
}

/// Record where a book syncs: its OPDS Progression endpoint and its Web
/// Annotation container, either or both, null to hold none.
///
/// This is where a book *learns* its services, and the only place it
/// can: in both protocols the URL is the publication's identity, so a
/// host that downloaded from a catalog records the two service links off
/// the entry it downloaded — a sideloaded book has no entry and so no
/// services. Both URLs are opaque and may embed a per-user key: never
/// log them, and key any credential by origin, not by the URL.
///
/// Calling again replaces both values; two nulls make the book local
/// again without touching what it still owes (a removed service simply
/// stops being asked).
#[no_mangle]
pub unsafe extern "C" fn cb_library_set_sync_targets(
    library: *mut cb_library,
    book: i64,
    progression_url: *const c_char,
    annotation_container: *const c_char,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_library!((library, book, progression_url, annotation_container) {
            use chapbook_reader::chapbook_library::BookId;
            clear_last_error();
            let library = library_mut!(library);
            // Null is "no service", not an error — unlike every other
            // string this module takes, absence is half the point here.
            let progression = if progression_url.is_null() {
                None
            } else {
                // SAFETY: the header's contract.
                match unsafe { crate::abi::str_in(progression_url, "progression_url") } {
                    Some(url) => Some(url),
                    None => return cb_status::CB_ERR_INVALID_UTF8,
                }
            };
            let container = if annotation_container.is_null() {
                None
            } else {
                // SAFETY: the header's contract.
                match unsafe { crate::abi::str_in(annotation_container, "annotation_container") } {
                    Some(url) => Some(url),
                    None => return cb_status::CB_ERR_INVALID_UTF8,
                }
            };
            match library.inner.set_sync_targets(BookId(book), progression, container) {
                Ok(()) => cb_status::CB_OK,
                Err(e) => crate::error::from_error(&e),
            }
        })
    })
}

/// The progression service this book syncs its position to.
/// `CB_ERR_UNAVAILABLE` when it has none — the ordinary state of a
/// sideloaded book, not an error worth surfacing.
#[no_mangle]
pub unsafe extern "C" fn cb_library_sync_progression_url(
    library: *const cb_library,
    book: i64,
    buf: *mut c_char,
    cap: usize,
    needed: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_library!((library, book, buf, cap, needed) {
            clear_last_error();
            sync_target(library, book, buf, cap, needed, |targets| {
                targets.progression_url
            })
        })
    })
}

/// The Web Annotation container this book syncs its marks with.
/// `CB_ERR_UNAVAILABLE` when it has none.
#[no_mangle]
pub unsafe extern "C" fn cb_library_sync_annotation_container(
    library: *const cb_library,
    book: i64,
    buf: *mut c_char,
    cap: usize,
    needed: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_library!((library, book, buf, cap, needed) {
            clear_last_error();
            sync_target(library, book, buf, cap, needed, |targets| {
                targets.annotation_container
            })
        })
    })
}

/// The one shape of both target getters: look up, pick a field, answer
/// absence honestly.
#[cfg(feature = "library")]
unsafe fn sync_target(
    library: *const cb_library,
    book: i64,
    buf: *mut c_char,
    cap: usize,
    needed: *mut usize,
    pick: impl FnOnce(chapbook_reader::chapbook_library::SyncTargets) -> Option<String>,
) -> cb_status {
    use chapbook_reader::chapbook_library::BookId;
    // SAFETY: a handle from `cb_library_open`, not yet closed.
    let Some(library) = (unsafe { library.as_ref() }) else {
        return fail(cb_status::CB_ERR_NULL_ARGUMENT, "library is null");
    };
    let targets = match library.inner.sync_targets(BookId(book)) {
        Ok(targets) => targets,
        Err(e) => return crate::error::from_error(&e),
    };
    let Some(url) = pick(targets) else {
        return fail(
            cb_status::CB_ERR_UNAVAILABLE,
            format!("book #{book} has no such service"),
        );
    };
    // SAFETY: the header's contract for the buffer triple.
    unsafe { str_out(&url, buf, cap, needed) }
}

/// The library row the open session is reading.
///
/// The join between the reading view and the shelf: a session *imports*
/// the book it opens, so this is how a host learns which row that became
/// — to record where it syncs to, or to find it again in a query.
///
/// `CB_ERR_UNAVAILABLE` for a book that never reached the library: an
/// OPDS page stream, or a session built without one.
#[no_mangle]
pub unsafe extern "C" fn cb_session_book_id(
    session: *const cb_session,
    book: *mut i64,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_library!((session, book) {
            // SAFETY: a handle from an open call, not yet closed.
            let session = match unsafe { session.as_ref() } {
                Some(session) => session,
                None => return fail(cb_status::CB_ERR_NULL_ARGUMENT, "session is null"),
            };
            let Some(id) = session.inner.book_id() else {
                return fail(
                    cb_status::CB_ERR_UNAVAILABLE,
                    "this session's book never reached the library",
                );
            };
            out!(book, id.0, "book");
            cb_status::CB_OK
        })
    })
}

// ---- Collections ----

/// Every collection, oldest first, into a caller-allocated array.
///
/// The same two-call idiom as the string accessors: ask with a zero
/// capacity to learn the count, allocate, ask again.
#[no_mangle]
pub unsafe extern "C" fn cb_library_collections(
    library: *const cb_library,
    buf: *mut cb_collection,
    cap: usize,
    needed: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_library!((library, buf, cap, needed) {
            clear_last_error();
            let library = library_ref!(library);
            let collections = match library.inner.collections() {
                Ok(collections) => collections,
                Err(e) => return crate::error::from_error(&e),
            };
            let items: Vec<cb_collection> = collections
                .iter()
                .map(|c| cb_collection {
                    id: c.id.0,
                    added_at: c.added_at,
                    books: c.books,
                })
                .collect();
            // SAFETY: the header's contract for the buffer triple.
            unsafe { slice_out(&items, buf, cap, needed) }
        })
    })
}

/// One collection's name, by id. `CB_ERR_UNAVAILABLE` if no live
/// collection has that id.
#[no_mangle]
pub unsafe extern "C" fn cb_library_collection_name(
    library: *const cb_library,
    collection: i64,
    buf: *mut c_char,
    cap: usize,
    needed: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_library!((library, collection, buf, cap, needed) {
            clear_last_error();
            let library = library_ref!(library);
            let collections = match library.inner.collections() {
                Ok(collections) => collections,
                Err(e) => return crate::error::from_error(&e),
            };
            let Some(found) = collections.iter().find(|c| c.id.0 == collection) else {
                return fail(
                    cb_status::CB_ERR_UNAVAILABLE,
                    format!("no collection #{collection}"),
                );
            };
            // SAFETY: the header's contract for the buffer triple.
            unsafe { str_out(&found.name, buf, cap, needed) }
        })
    })
}

/// Make a collection, or return the one that already has this name.
///
/// Idempotent on the name: a host putting a book on "Sci-Fi" should not
/// have to ask first whether "Sci-Fi" exists.
#[no_mangle]
pub unsafe extern "C" fn cb_library_create_collection(
    library: *mut cb_library,
    name: *const c_char,
    id: *mut i64,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_library!((library, name, id) {
            clear_last_error();
            let library = library_mut!(library);
            // SAFETY: the header's contract for a `const char*`.
            let Some(name) = (unsafe { str_in(name, "collection name") }) else {
                return cb_status::CB_ERR_INVALID_UTF8;
            };
            match library.inner.create_collection(name) {
                Ok(collection) => {
                    out!(id, collection.0, "id");
                    cb_status::CB_OK
                }
                Err(e) => crate::error::from_error(&e),
            }
        })
    })
}

/// Rename a collection.
#[no_mangle]
pub unsafe extern "C" fn cb_library_rename_collection(
    library: *mut cb_library,
    collection: i64,
    name: *const c_char,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_library!((library, collection, name) {
            use chapbook_reader::chapbook_library::CollectionId;
            clear_last_error();
            let library = library_mut!(library);
            // SAFETY: the header's contract for a `const char*`.
            let Some(name) = (unsafe { str_in(name, "collection name") }) else {
                return cb_status::CB_ERR_INVALID_UTF8;
            };
            match library
                .inner
                .rename_collection(CollectionId(collection), name)
            {
                Ok(()) => cb_status::CB_OK,
                Err(e) => crate::error::from_error(&e),
            }
        })
    })
}

/// Delete a collection. The books stay; only the grouping goes, and the
/// name becomes available again.
#[no_mangle]
pub unsafe extern "C" fn cb_library_delete_collection(
    library: *mut cb_library,
    collection: i64,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_library!((library, collection) {
            use chapbook_reader::chapbook_library::CollectionId;
            clear_last_error();
            let library = library_mut!(library);
            match library.inner.delete_collection(CollectionId(collection)) {
                Ok(()) => cb_status::CB_OK,
                Err(e) => crate::error::from_error(&e),
            }
        })
    })
}

/// Put a book in a collection. Doing it twice is not an error.
#[no_mangle]
pub unsafe extern "C" fn cb_library_add_to_collection(
    library: *mut cb_library,
    book: i64,
    collection: i64,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_library!((library, book, collection) {
            use chapbook_reader::chapbook_library::{BookId, CollectionId};
            clear_last_error();
            let library = library_mut!(library);
            match library
                .inner
                .add_to_collection(BookId(book), CollectionId(collection))
            {
                Ok(()) => cb_status::CB_OK,
                Err(e) => crate::error::from_error(&e),
            }
        })
    })
}

/// Take a book out of a collection. Doing it twice is not an error.
#[no_mangle]
pub unsafe extern "C" fn cb_library_remove_from_collection(
    library: *mut cb_library,
    book: i64,
    collection: i64,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_library!((library, book, collection) {
            use chapbook_reader::chapbook_library::{BookId, CollectionId};
            clear_last_error();
            let library = library_mut!(library);
            match library
                .inner
                .remove_from_collection(BookId(book), CollectionId(collection))
            {
                Ok(()) => cb_status::CB_OK,
                Err(e) => crate::error::from_error(&e),
            }
        })
    })
}

// ---- Conversions ----

#[cfg(feature = "library")]
fn state_in(state: cb_reading_state) -> Option<chapbook_reader::chapbook_library::ReadingState> {
    use chapbook_reader::chapbook_library::ReadingState;
    match state {
        cb_reading_state::CB_STATE_ANY => None,
        cb_reading_state::CB_STATE_UNREAD => Some(ReadingState::Unread),
        cb_reading_state::CB_STATE_READING => Some(ReadingState::Reading),
        cb_reading_state::CB_STATE_FINISHED => Some(ReadingState::Finished),
    }
}

#[cfg(feature = "library")]
fn state_out(state: chapbook_reader::chapbook_library::ReadingState) -> cb_reading_state {
    use chapbook_reader::chapbook_library::ReadingState;
    match state {
        ReadingState::Unread => cb_reading_state::CB_STATE_UNREAD,
        ReadingState::Reading => cb_reading_state::CB_STATE_READING,
        ReadingState::Finished => cb_reading_state::CB_STATE_FINISHED,
    }
}

#[cfg(feature = "library")]
fn sort_in(sort: cb_sort) -> chapbook_reader::chapbook_library::Sort {
    use chapbook_reader::chapbook_library::Sort;
    match sort {
        cb_sort::CB_SORT_ADDED => Sort::Added,
        cb_sort::CB_SORT_READ => Sort::Read,
        cb_sort::CB_SORT_TITLE => Sort::Title,
        cb_sort::CB_SORT_AUTHOR => Sort::Author,
        cb_sort::CB_SORT_SERIES => Sort::Series,
    }
}

#[cfg(feature = "library")]
fn book_out(book: &chapbook_reader::chapbook_library::BookRecord) -> cb_book {
    cb_book {
        id: book.id.0,
        added_at: book.added_at,
        last_read: book.last_read.unwrap_or(0),
        finished_at: book.finished_at.unwrap_or(0),
        progress: book.progress.unwrap_or(0.0),
        series_index: book.series_index.unwrap_or(0.0),
        state: state_out(book.state()),
        author_count: book.authors.len(),
        collection_count: book.collections.len(),
        has_progress: book.progress.is_some(),
        has_series_index: book.series_index.is_some(),
        has_cover: book.cover_path.is_some(),
    }
}
