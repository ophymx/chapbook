//! The JNI surface. Names match `com.ophymx.chapbook.Native`.
//!
//! Rungs 1 through 4 of this binding were written against a session that
//! reached for its fonts, its library directory and its diagnostics behind
//! the caller's back, and the workarounds were the point: each one was a
//! defect report. They are gone now. Every capability this file needs
//! arrives through `SessionConfig` or `Source`, so what is left reads like
//! what the C ABI will wrap rather than like a list of complaints.

use std::ffi::c_void;

use chapbook_reader::chapbook_core::{
    Action, ActionOutcome, EdgeSizes, FontSource, Key, KeyMap, PageMetrics, ReadingDirection,
    Rotation, Size, Source, TapZones,
};
use chapbook_reader::chapbook_core::{ReadingSettings, Theme};
use chapbook_reader::{Session, SessionConfig, SessionEvent, SettingsScope};
use jni::objects::{JClass, JObject, JString};
use jni::sys::{jboolean, jfloat, jfloatArray, jint, jintArray, jlong, jlongArray, jstring};
use jni::JNIEnv;

// ---- Handles ----

/// What a `jlong` handle actually points at: the session, plus the input
/// policy that belongs to this shell rather than to the engine.
///
/// The tap zones live here because they are configuration — how wide the
/// bands are, what the middle one does — and because the one field a shell
/// must *not* configure, the reading direction, comes off the book. Keeping
/// them beside the session is what lets `tapAction` be a single call that
/// cannot be given the wrong direction by accident.
struct Shell {
    session: Session,
    zones: TapZones,
    keys: KeyMap,
    /// Session events drained from the engine and not yet handed to
    /// Kotlin, plus the message belonging to the last one handed over —
    /// `nextEvent` packs the numbers into a `jlong` and `eventMessage`
    /// answers for the string half, because a JNI call per field is the
    /// expensive shape and an object per event is the verbose one.
    events: std::collections::VecDeque<SessionEvent>,
    event_message: Option<String>,
    /// What the last search found, held so the per-index readers have
    /// something to read.
    hits: Vec<chapbook_reader::SearchHit>,
}

/// A session, as a `jlong` Java holds onto. Null is the failure value, so
/// the Kotlin side never sees a Rust error type.
fn into_handle(session: Session) -> jlong {
    // The direction is the book's; everything else is the default policy
    // until Kotlin says otherwise.
    let zones = TapZones::new(session.reading_direction());
    let shell = Shell {
        session,
        zones,
        keys: KeyMap::default(),
        events: std::collections::VecDeque::new(),
        event_message: None,
        hits: Vec::new(),
    };
    Box::into_raw(Box::new(shell)) as jlong
}

/// # Safety
/// `handle` must have come from [`into_handle`] and not yet been closed.
unsafe fn shell<'a>(handle: jlong) -> Option<&'a mut Shell> {
    (handle as *mut Shell).as_mut()
}

/// # Safety
/// `handle` must have come from [`into_handle`] and not yet been closed.
unsafe fn session<'a>(handle: jlong) -> Option<&'a mut Session> {
    shell(handle).map(|s| &mut s.session)
}

/// How this shell configures a session, in one place because the demo opens
/// sessions three ways — a path, a file descriptor, and one per conformance
/// check — and all three want the same answers.
///
/// [`FontSource::android_system`] is the whole font story: `/system/fonts`
/// for the faces, the families Android actually ships for the generics, and
/// Noto for fallback. Rung 3 had to do all three by hand through
/// `paint_resources`, which is an accessor for rasterizing a display list
/// and was never meant to configure anything.
fn config(library_dir: Option<String>) -> SessionConfig {
    let config = SessionConfig::new(FontSource::android_system());
    match library_dir {
        Some(dir) => config.with_library_dir(dir),
        None => config,
    }
}

/// A `String` from a `JString`, or `None` if the JVM would not give one up.
fn string_in(env: &mut JNIEnv, value: &JString) -> Option<String> {
    env.get_string(value).ok().map(Into::into)
}

fn string_out(env: &JNIEnv, value: &str) -> jstring {
    match env.new_string(value) {
        Ok(s) => s.into_raw(),
        Err(_) => JObject::null().into_raw(),
    }
}

// ---- Diagnostics ----

/// Point the engine's `log` records at logcat.
///
/// Call before opening anything. Until a backend is installed the engine is
/// silent by design — and on Android that used to mean the failures only a
/// device hits were the ones nobody could see, which is what
/// `chapbook_core::diagnostics` was built for. `android_logger` is the
/// stock backend; nothing about it is chapbook's business beyond this call.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_initLogging(
    _env: JNIEnv,
    _class: JClass,
    verbose: jboolean,
) {
    let level = if verbose != 0 {
        log::LevelFilter::Debug
    } else {
        log::LevelFilter::Info
    };
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(level)
            .with_tag("chapbook"),
    );
    log::info!("logging to logcat at {level}");
}

// ---- Lifecycle ----

/// Opens a book from a filesystem path.
///
/// `library_dir` is `context.getFilesDir()`, handed over as an argument.
/// Rung 2 wrote it into the process environment with `setenv` because
/// `Session::open` read `CHAPBOOK_LIBRARY_DIR` on its own; that was a
/// stopgap on a sandboxed platform whose one true answer is not reachable
/// through an environment variable at all.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_open(
    mut env: JNIEnv,
    _class: JClass,
    path: JString,
    library_dir: JString,
) -> jlong {
    let Some(path) = string_in(&mut env, &path) else {
        return 0;
    };
    let library_dir = string_in(&mut env, &library_dir);
    match Session::open_with(std::path::PathBuf::from(&path), config(library_dir)) {
        Ok(session) => into_handle(session),
        Err(e) => {
            log::error!("could not open {path}: {e}");
            0
        }
    }
}

/// Opens a book from a file descriptor — rung 5, and the only rung that
/// resembles what a real Android app does.
///
/// The storage access framework hands back a `content://` URI with no path
/// and, very often, no extension either: `ParcelFileDescriptor` is all
/// there is. `fd` must therefore be *detached* on the Kotlin side, because
/// the `File` built here owns it and closes it when the session drops.
///
/// `Format::Guess` is not a concession, it is the better answer — the EPUB
/// `mimetype` entry and the `%PDF` header are in the bytes, and a name that
/// was never going to arrive cannot be trusted anyway.
///
/// **A handle reaches the library by content.** The stream is hashed on
/// open and the book adopted under the same edition fingerprint a path
/// import gets — recorded, not copied — so position, annotations and
/// per-book settings persist. What the engine cannot do is reopen the
/// file: holding a persistable URI grant and re-resolving it next launch
/// is the app's half of custody, per `docs/PLATFORM.md`.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_openFd(
    mut env: JNIEnv,
    _class: JClass,
    fd: jint,
    library_dir: JString,
) -> jlong {
    if fd < 0 {
        log::error!("openFd got no descriptor");
        return 0;
    }
    let library_dir = string_in(&mut env, &library_dir);
    // SAFETY: Kotlin called `ParcelFileDescriptor.detachFd()`, which gives
    // up ownership; nothing else will read or close it.
    let file = unsafe {
        use std::os::fd::FromRawFd;
        std::fs::File::from_raw_fd(fd)
    };
    match Session::open_with(Source::reader(file), config(library_dir)) {
        Ok(session) => into_handle(session),
        Err(e) => {
            log::error!("could not open the descriptor: {e}");
            0
        }
    }
}

#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_close(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) {
    if handle != 0 {
        // SAFETY: the handle came from `into_handle` and Kotlin promises
        // one close per open.
        drop(unsafe { Box::from_raw(handle as *mut Shell) });
    }
}

/// What realizing the font source actually produced: faces loaded, and any
/// generic family pointed at a name no loaded face carries.
///
/// Zero faces means every page paginates blank, which takes navigation,
/// search and the table of contents with it — so this is still the first
/// thing worth putting on screen. It is now a fact the session reports
/// rather than a number read back out of the font database.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_fontReport(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jstring {
    let text = match unsafe { session(handle) } {
        None => "no session".to_string(),
        Some(s) => {
            let report = s.font_report();
            if report.is_clean() {
                format!("{} faces", report.faces)
            } else {
                let unresolved: Vec<String> = report
                    .unresolved_generics
                    .iter()
                    .map(|(generic, family)| format!("{generic}={family}"))
                    .collect();
                format!(
                    "{} faces, unresolved: {}",
                    report.faces,
                    unresolved.join(" ")
                )
            }
        }
    };
    string_out(&env, &text)
}

/// Let go of everything reconstructible, and save the position while there
/// is still a process to save it from.
///
/// This is `onStop`, which is the last callback Android guarantees. Rung 3
/// had nothing to call here.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_suspendSession(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) {
    if let Some(s) = unsafe { session(handle) } {
        s.suspend();
    }
}

/// `onTrimMemory`, which Android calls with a level and no argument about
/// what to do with it. Drops cached layouts and decoded images; the current
/// page is rebuilt on the next draw.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_releaseCaches(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) {
    if let Some(s) = unsafe { session(handle) } {
        s.release_caches();
    }
}

/// Bytes the session's caches are holding. The status line shows it beside
/// the budget, because a number nobody can see is a budget nobody trusts.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_cacheBytes(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jlong {
    match unsafe { session(handle) } {
        Some(s) => s.cache_bytes() as jlong,
        None => -1,
    }
}

#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_cacheBudget(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jlong {
    match unsafe { session(handle) } {
        Some(s) => s.cache_budget() as jlong,
        None => -1,
    }
}

// ---- Layout and navigation ----

#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_setMetrics(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    width: jfloat,
    height: jfloat,
    margin: jfloat,
    scale: jfloat,
) {
    if let Some(s) = unsafe { session(handle) } {
        s.set_metrics(PageMetrics {
            size: Size {
                w: width,
                h: height,
            },
            margins: EdgeSizes::uniform(margin),
            dpi_scale: scale,
            rotation: Rotation::None,
        });
    }
}

/// Returns whether the position moved — the answer `docs/SHELLS.md` insists
/// a shell use rather than comparing page numbers across the turn.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_nextPage(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jboolean {
    unsafe { session(handle) }.is_some_and(Session::next_page) as jboolean
}

#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_prevPage(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jboolean {
    unsafe { session(handle) }.is_some_and(Session::prev_page) as jboolean
}

/// Cycle the reading theme.
///
/// In the demo this is the middle tap zone, and it is there for a reason:
/// sepia is the only thing on screen whose red and blue channels differ, so
/// it is the one test that can tell premultiplied RGBA from BGRA. Black
/// text on white paper looks identical either way.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_cycleTheme(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) {
    if let Some(s) = unsafe { session(handle) } {
        s.cycle_theme();
    }
}

/// `(spine, page)` packed into one `jlong`, because they are one value and
/// handing them over separately invites exactly the bug the conformance
/// harness exists to catch.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_position(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jlong {
    match unsafe { session(handle) } {
        Some(s) => {
            let p = s.position();
            ((p.spine as jlong) << 32) | (p.page as jlong & 0xffff_ffff)
        }
        None => -1,
    }
}

#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_title(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jstring {
    let title = match unsafe { session(handle) } {
        Some(s) => s.title().to_string(),
        None => String::new(),
    };
    string_out(&env, &title)
}

// ---- The text surface ----
//
// The current page's text with geometry — what `AccessibilityNodeInfo`,
// TTS word highlighting (`UtteranceProgressListener.onRangeStart`), and a
// dictionary popup consume. Values cross packed — ranges in a `jlong`
// like `position`, geometry flattened into primitive arrays — because a
// TTS engine reads the whole word table once per page, and a JNI call per
// word is the expensive shape. Locator offsets ride `jint`/`jlong` halves
// as raw `u32` bits; real books sit far below 2^31 characters a unit.

fn float_array_out(env: &JNIEnv, values: &[jfloat]) -> jfloatArray {
    let Ok(array) = env.new_float_array(values.len() as i32) else {
        return JObject::null().into_raw();
    };
    if env.set_float_array_region(&array, 0, values).is_err() {
        return JObject::null().into_raw();
    }
    array.into_raw()
}

fn int_array_out(env: &JNIEnv, values: &[jint]) -> jintArray {
    let Ok(array) = env.new_int_array(values.len() as i32) else {
        return JObject::null().into_raw();
    };
    if env.set_int_array_region(&array, 0, values).is_err() {
        return JObject::null().into_raw();
    }
    array.into_raw()
}

fn pack_range(start: u32, end: u32) -> jlong {
    ((start as jlong) << 32) | (end as jlong & 0xffff_ffff)
}

/// How many text runs the current page holds; `-1` until it is laid out,
/// `0` for a laid-out page with nothing to speak (a comic) — different
/// answers on purpose.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_pageTextRunCount(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jint {
    match unsafe { session(handle) }.and_then(|s| s.page_text_runs()) {
        Some(runs) => runs.len() as jint,
        None => -1,
    }
}

/// One run's locator range packed like `position`: `start << 32 | end`.
/// `-1` for a bad handle or index.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_pageTextRunRange(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    index: jint,
) -> jlong {
    unsafe { session(handle) }
        .and_then(|s| s.page_text_runs())
        .and_then(|runs| runs.get(index as usize).cloned())
        .map(|run| pack_range(run.locator_start, run.locator_end))
        .unwrap_or(-1)
}

/// One run's page-space rect as `[x, y, w, h]`; empty for a bad handle or
/// index.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_pageTextRunRect(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    index: jint,
) -> jfloatArray {
    let rect = unsafe { session(handle) }
        .and_then(|s| s.page_text_runs())
        .and_then(|runs| runs.get(index as usize).map(|run| run.rect));
    match rect {
        Some(r) => float_array_out(&env, &[r.origin.x, r.origin.y, r.size.w, r.size.h]),
        None => float_array_out(&env, &[]),
    }
}

/// One run's text; `""` for a bad handle or index.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_pageTextRunText(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    index: jint,
) -> jstring {
    let text = unsafe { session(handle) }
        .and_then(|s| s.page_text_runs())
        .and_then(|runs| runs.get(index as usize).map(|run| run.text.clone()))
        .unwrap_or_default();
    string_out(&env, &text)
}

/// The page as one speakable string — hand it to TTS whole, then map its
/// progress reports back through [`pageWords`]. `""` until laid out.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_speakableText(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jstring {
    let text = unsafe { session(handle) }
        .and_then(|s| s.speakable_page())
        .map(|page| page.text)
        .unwrap_or_default();
    string_out(&env, &text)
}

/// The whole word table in one crossing: four ints per word —
/// `textStart, textEnd, locatorStart, locatorEnd` — char offsets into
/// [`speakableText`] and locator offsets respectively. Empty until laid
/// out (use [`pageTextRunCount`] to tell "not laid out" from "no words").
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_pageWords(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jintArray {
    let words: Vec<jint> = unsafe { session(handle) }
        .and_then(|s| s.speakable_page())
        .map(|page| {
            page.words
                .iter()
                .flat_map(|w| {
                    [
                        w.text_start as jint,
                        w.text_end as jint,
                        w.locator_start as jint,
                        w.locator_end as jint,
                    ]
                })
                .collect()
        })
        .unwrap_or_default();
    int_array_out(&env, &words)
}

/// The word under a point in panel coordinates, packed `start << 32 |
/// end` — dictionary lookup's question. `-1` off text, on whitespace, or
/// on bare punctuation.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_wordAt(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    x: jfloat,
    y: jfloat,
) -> jlong {
    unsafe { session(handle) }
        .and_then(|s| s.word_at(x, y))
        .map(|(start, end)| pack_range(start, end))
        .unwrap_or(-1)
}

/// Page-space rects covering a locator range on the current page, four
/// floats per rect — the geometry a TTS word highlight paints. Empty when
/// nothing is there.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_rangeRects(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    start: jint,
    end: jint,
) -> jfloatArray {
    let flat: Vec<jfloat> = unsafe { session(handle) }
        .map(|s| s.range_rects(start as u32, end as u32))
        .unwrap_or_default()
        .into_iter()
        .flat_map(|r| [r.origin.x, r.origin.y, r.size.w, r.size.h])
        .collect();
    float_array_out(&env, &flat)
}

// ---- Input ----
//
// Actions cross this boundary as their `Action::name()` strings rather
// than as ordinals. `Action` is `#[non_exhaustive]` and Kotlin has no way
// to notice a reordering, so a token that describes itself is worth the
// allocation — which is charged per keypress, at human rates. It also
// means the demo's status line gets `"next-page"` for free, and that
// `Action::name`/`from_name` — built for exactly this and until now
// consumed by nothing — are actually exercised.
//
// Kotlin never has to *interpret* an action: it hands back whatever it was
// given and reads the outcome. `Unhandled` is how it learns that one was
// its own to deal with, which is why no enum has to be mirrored.

/// Which edge this book reads from: `"ltr"` or `"rtl"`.
///
/// The book declares it and the tap zones already use it — this is here so
/// a shell can *show* that it did, which is the only way a reader can tell
/// a correctly-flipped RTL book from a bug.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_readingDirection(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jstring {
    let direction = match unsafe { session(handle) } {
        Some(s) => s.reading_direction(),
        None => ReadingDirection::Ltr,
    };
    string_out(
        &env,
        match direction {
            ReadingDirection::Ltr => "ltr",
            ReadingDirection::Rtl => "rtl",
        },
    )
}

/// Reconfigure the tap bands. `middle` is an action name, or `""` for a
/// band that does nothing.
///
/// The reading direction is deliberately not a parameter. It is the book's
/// and is re-read here, so a shell cannot flip a book by configuring it.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_setTapZones(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    prev_fraction: jfloat,
    next_fraction: jfloat,
    middle: JString,
) {
    let middle = string_in(&mut env, &middle).unwrap_or_default();
    let Some(shell) = (unsafe { shell(handle) }) else {
        return;
    };
    if !middle.is_empty() && Action::from_name(&middle).is_none() {
        log::warn!("no action named {middle}; the middle band will do nothing");
    }
    shell.zones = TapZones {
        prev_fraction,
        next_fraction,
        middle: Action::from_name(&middle),
        direction: shell.session.reading_direction(),
    };
}

/// What a tap at a point means, or `""` for nothing.
///
/// `x` and `y` are **logical units** — a view's pixels divided by its
/// density, the same space `setMetrics` is given — and they are *panel*
/// coordinates, so a rotated panel is undone on this side and a shell
/// never applies the inverse itself.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_tapAction(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    x: jfloat,
    y: jfloat,
) -> jstring {
    let name = unsafe { shell(handle) }
        .and_then(|s| {
            let metrics = s.session.metrics()?;
            s.zones.action_at(x, y, &metrics)
        })
        .map_or("", Action::name);
    string_out(&env, name)
}

/// What a key means, or `""` for one this reader does not bind.
///
/// `key_code` is an `android.view.KeyEvent.KEYCODE_*` value. Those are
/// translated here rather than in Kotlin because they are the stable half:
/// Android can never renumber them without breaking every app on the
/// platform, whereas an ordinal invented in this workspace can move in any
/// commit. So the mapping that is safe to hardcode is the one hardcoded.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_actionForKeyCode(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    key_code: jint,
) -> jstring {
    let name = key_of(key_code)
        .and_then(|key| unsafe { shell(handle) }.and_then(|s| s.keys.action(key)))
        .map_or("", Action::name);
    string_out(&env, name)
}

/// `android.view.KeyEvent.KEYCODE_*` to the engine's vocabulary.
///
/// Volume up and down are the interesting pair. Android readers
/// conventionally borrow them for page turns, and borrowing them is what
/// makes `applyAction`'s outcome load-bearing: a shell that does not tell
/// the platform it took the press gets the system volume slider drawn over
/// the book.
fn key_of(key_code: jint) -> Option<Key> {
    // From android.view.KeyEvent. Frozen by the platform's own
    // compatibility promise, which is why they are safe as literals.
    const KEYCODE_DPAD_UP: jint = 19;
    const KEYCODE_DPAD_DOWN: jint = 20;
    const KEYCODE_DPAD_LEFT: jint = 21;
    const KEYCODE_DPAD_RIGHT: jint = 22;
    const KEYCODE_VOLUME_UP: jint = 24;
    const KEYCODE_VOLUME_DOWN: jint = 25;
    const KEYCODE_SPACE: jint = 62;
    const KEYCODE_DEL: jint = 67;
    const KEYCODE_PAGE_UP: jint = 92;
    const KEYCODE_PAGE_DOWN: jint = 93;

    match key_code {
        KEYCODE_DPAD_UP => Some(Key::ArrowUp),
        KEYCODE_DPAD_DOWN => Some(Key::ArrowDown),
        KEYCODE_DPAD_LEFT => Some(Key::ArrowLeft),
        KEYCODE_DPAD_RIGHT => Some(Key::ArrowRight),
        KEYCODE_VOLUME_UP => Some(Key::VolumeUp),
        KEYCODE_VOLUME_DOWN => Some(Key::VolumeDown),
        KEYCODE_SPACE => Some(Key::Space),
        KEYCODE_DEL => Some(Key::Backspace),
        KEYCODE_PAGE_UP => Some(Key::PageUp),
        KEYCODE_PAGE_DOWN => Some(Key::PageDown),
        _ => None,
    }
}

/// Apply an action by name. Returns the outcome: `0` changed, `1`
/// unchanged, `2` not the engine's — and `-1` for a dead handle or a name
/// this build does not know.
///
/// Two answers, because a shell needs both. `0` says repaint. `0` or `1`
/// says tell Android you consumed the event; `2` says let it through, and
/// `2` is what an empty back stack returns so the system Back can leave
/// the reader without this side tracking history to know when to stop.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_applyAction(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    action: JString,
) -> jint {
    let Some(name) = string_in(&mut env, &action) else {
        return -1;
    };
    let Some(action) = Action::from_name(&name) else {
        log::warn!("no action named {name}");
        return -1;
    };
    match unsafe { session(handle) }.map(|s| s.apply(action)) {
        Some(ActionOutcome::Changed) => 0,
        Some(ActionOutcome::Unchanged) => 1,
        Some(ActionOutcome::Unhandled) => 2,
        None => -1,
    }
}

// ---- Pixels ----

// libjnigraphics, which is on the NDK's stable list. This is the real
// `render_into` destination: memory Android owns, that we draw into.
#[repr(C)]
struct AndroidBitmapInfo {
    width: u32,
    height: u32,
    stride: u32,
    format: i32,
    flags: u32,
}

const ANDROID_BITMAP_FORMAT_RGBA_8888: i32 = 1;

// The `link` attribute is not decoration. Without it the crate builds
// clean, the `.so` is produced, and every AndroidBitmap symbol is left
// UND with no DT_NEEDED entry naming libjnigraphics — so the failure
// arrives at `System.loadLibrary`, as a crash in an app that compiled.
#[link(name = "jnigraphics")]
extern "C" {
    fn AndroidBitmap_getInfo(
        env: *mut jni::sys::JNIEnv,
        bitmap: jni::sys::jobject,
        info: *mut AndroidBitmapInfo,
    ) -> i32;
    fn AndroidBitmap_lockPixels(
        env: *mut jni::sys::JNIEnv,
        bitmap: jni::sys::jobject,
        pixels: *mut *mut c_void,
    ) -> i32;
    fn AndroidBitmap_unlockPixels(env: *mut jni::sys::JNIEnv, bitmap: jni::sys::jobject) -> i32;
}

/// The device-pixel size the bitmap must be, packed `(width << 32) | height`.
///
/// The host allocates the surface, so the host has to be told how big it
/// is. `-1` until metrics are set.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_renderSize(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jlong {
    match unsafe { session(handle) }.and_then(|s| s.render_size()) {
        Some((w, h)) => ((w as jlong) << 32) | (h as jlong & 0xffff_ffff),
        None => -1,
    }
}

/// Render the current page into an `ARGB_8888` bitmap.
///
/// Rung 3 rendered to a freshly allocated `Pixmap` and then memcpy'd it row
/// by row, once per page turn, because that was the only way in. It is now
/// what `render_into` was built for: `AndroidBitmap_lockPixels` hands back
/// the bitmap's own backing store, the engine rasterizes straight into it,
/// and for an unrotated page whose stride is exactly `width * 4` — which is
/// what Android gives — there is no intermediate and no copy at all.
///
/// The premultiplied-RGBA claim held up on a device: tiny-skia's output and
/// `ARGB_8888`'s memory layout are the same bytes, so nothing converts at
/// either end. Sepia is the check, since black on white cannot tell RGBA
/// from BGRA.
///
/// Returns 0 on success, or a negative code: -1 no session, -2 nothing to
/// render, -3 the bitmap is the wrong format or size, -4 the lock failed.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_renderInto(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    bitmap: JObject,
) -> jint {
    let Some(session) = (unsafe { session(handle) }) else {
        return -1;
    };
    let Some((want_w, want_h)) = session.render_size() else {
        return -2;
    };

    let raw_env = env.get_raw();
    let raw_bitmap = bitmap.as_raw();
    let mut info = AndroidBitmapInfo {
        width: 0,
        height: 0,
        stride: 0,
        format: 0,
        flags: 0,
    };
    // SAFETY: `env` and `bitmap` are live for the duration of the call, and
    // `info` is a valid out-pointer.
    if unsafe { AndroidBitmap_getInfo(raw_env, raw_bitmap, &mut info) } != 0 {
        return -3;
    }
    // Checked here rather than left to `render_into`'s `false` so that a
    // caller who sized its bitmap from something other than `renderSize`
    // gets told which thing was wrong.
    if info.format != ANDROID_BITMAP_FORMAT_RGBA_8888
        || info.width != want_w
        || info.height != want_h
        || (info.stride as usize) < (want_w as usize) * 4
    {
        return -3;
    }

    let mut pixels: *mut c_void = std::ptr::null_mut();
    // SAFETY: as above; the pointer is written only on success.
    if unsafe { AndroidBitmap_lockPixels(raw_env, raw_bitmap, &mut pixels) } != 0
        || pixels.is_null()
    {
        return -4;
    }

    let len = info.stride as usize * info.height as usize;
    // SAFETY: Android just reported this mapping's stride and height, and
    // the lock keeps it alive and unmoved until `unlockPixels` below.
    // Nothing else aliases it while it is locked.
    let dst = unsafe { std::slice::from_raw_parts_mut(pixels as *mut u8, len) };
    let drew = session.render_into(dst, info.width, info.height, info.stride as usize);

    // SAFETY: paired with the successful lock above.
    unsafe { AndroidBitmap_unlockPixels(raw_env, raw_bitmap) };
    if drew {
        0
    } else {
        -2
    }
}

// ---- Proving it ----

/// Run the shell conformance harness against this book and hand back its
/// report. This is the rung that tests the *binding* rather than the build:
/// the harness already watches the seam from outside, so driving it through
/// JNI asks whether the seam survived the trip.
///
/// It takes a path rather than an open handle because the harness opens its
/// own sessions — one per check, since "position survives a restart" cannot
/// be asked of a session that never stopped. That also means it needs the
/// library directory: a position has nowhere to survive to without one.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_conformance(
    mut env: JNIEnv,
    _class: JClass,
    path: JString,
    library_dir: JString,
) -> jstring {
    let Some(path) = string_in(&mut env, &path) else {
        return string_out(&env, "bad path");
    };
    let library_dir = string_in(&mut env, &library_dir);
    let open = || Session::open_with(std::path::PathBuf::from(&path), config(library_dir.clone()));
    // The harness wants an infallible factory. A book that will not open is
    // not a conformance failure, it is a different question, so say so
    // rather than reporting eleven mysterious ones.
    if let Err(e) = open() {
        log::error!("conformance could not open {path}: {e}");
        return string_out(&env, "could not open the book");
    }
    let report = chapbook_reader::conformance::Harness::new(|| {
        open().expect("the book opened a moment ago")
    })
    .run()
    .to_string();
    string_out(&env, &report)
}

// ---- The shelf ----
//
// Two more handles, matching the C ABI's shape rather than inventing a
// second one: a library connection, and one query's rows held still.
// The rows are their own handle for the reason the C ABI's are — a shelf
// UI holds a page of results while the reader types in a search box, and
// a last-query slot on the library would invalidate what is being drawn.
//
// Nothing here constructs a Java object. Every field crosses as a
// primitive or a `String`, indexed, exactly like the text surface above:
// building a Kotlin data class from Rust means naming its constructor
// signature in a string, which is a link error nothing checks.

/// A library, as a `jlong`. 0 is the failure value.
fn library_handle(library: chapbook_reader::chapbook_library::Library) -> jlong {
    Box::into_raw(Box::new(library)) as jlong
}

/// # Safety
/// `handle` must have come from [`library_handle`] and not yet been closed.
unsafe fn library<'a>(handle: jlong) -> Option<&'a mut chapbook_reader::chapbook_library::Library> {
    (handle as *mut chapbook_reader::chapbook_library::Library).as_mut()
}

/// # Safety
/// `handle` must have come from `libraryQuery` and not yet been freed.
unsafe fn shelf<'a>(
    handle: jlong,
) -> Option<&'a Vec<chapbook_reader::chapbook_library::BookRecord>> {
    (handle as *const Vec<chapbook_reader::chapbook_library::BookRecord>).as_ref()
}

/// # Safety
/// As [`shelf`], plus an index the caller got from `shelfLen`.
unsafe fn row<'a>(
    handle: jlong,
    index: jint,
) -> Option<&'a chapbook_reader::chapbook_library::BookRecord> {
    if index < 0 {
        return None;
    }
    unsafe { shelf(handle) }.and_then(|books| books.get(index as usize))
}

/// Open (creating if needed) the library at `dir`; an empty `dir` means
/// this platform's default, which on Android is an error — the app knows
/// its own container and has to say it. 0 if it could not be opened.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_libraryOpen(
    mut env: JNIEnv,
    _class: JClass,
    dir: JString,
) -> jlong {
    use chapbook_reader::chapbook_library::Library;
    let dir = string_in(&mut env, &dir).filter(|d| !d.is_empty());
    let path = match dir {
        Some(dir) => std::path::PathBuf::from(dir),
        None => match Library::default_dir() {
            Ok(path) => path,
            Err(e) => {
                log::error!("no default library location: {e}");
                return 0;
            }
        },
    };
    match Library::open(&path) {
        Ok(library) => library_handle(library),
        Err(e) => {
            log::error!("could not open the library at {}: {e}", path.display());
            0
        }
    }
}

/// Close a library. Tolerates 0, so a failed open needs no special case.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_libraryClose(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) {
    if handle != 0 {
        // SAFETY: a handle from `libraryOpen`, closed once.
        drop(unsafe { Box::from_raw(handle as *mut chapbook_reader::chapbook_library::Library) });
    }
}

/// Run a query and hold its rows; 0 if it failed. Free with `shelfFree`.
///
/// The arguments are the query flattened, because a struct crossing JNI
/// is a Java class this file would have to name by signature. Empty
/// strings and zeros mean "do not narrow", so the all-defaults call is
/// the whole shelf — the same property the C ABI's zero-initialized
/// struct has.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "system" fn Java_com_ophymx_chapbook_Native_libraryQuery(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    search: JString,
    series: JString,
    collection: jlong,
    state: jint,
    sort: jint,
    limit: jint,
    offset: jint,
) -> jlong {
    use chapbook_reader::chapbook_library::{BookQuery, CollectionId, ReadingState, Sort};

    let Some(library) = (unsafe { library(handle) }) else {
        return 0;
    };
    let search = string_in(&mut env, &search).filter(|s| !s.is_empty());
    let series = string_in(&mut env, &series).filter(|s| !s.is_empty());
    let state = match state {
        1 => Some(ReadingState::Unread),
        2 => Some(ReadingState::Reading),
        3 => Some(ReadingState::Finished),
        _ => None,
    };
    let sort = match sort {
        1 => Sort::Read,
        2 => Sort::Title,
        3 => Sort::Author,
        4 => Sort::Series,
        _ => Sort::Added,
    };
    let query = BookQuery {
        search: search.as_deref(),
        series: series.as_deref(),
        collection: (collection != 0).then_some(CollectionId(collection)),
        state,
        sort,
        limit: (limit > 0).then_some(limit as usize),
        offset: offset.max(0) as usize,
    };
    match library.query(&query) {
        Ok(books) => Box::into_raw(Box::new(books)) as jlong,
        Err(e) => {
            log::error!("shelf query failed: {e}");
            0
        }
    }
}

/// Release a shelf. Tolerates 0.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_shelfFree(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) {
    if handle != 0 {
        // SAFETY: a handle from `libraryQuery`, freed once.
        drop(unsafe {
            Box::from_raw(handle as *mut Vec<chapbook_reader::chapbook_library::BookRecord>)
        });
    }
}

/// How many rows; `-1` for a bad handle, so "empty" and "broken" are
/// different answers.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_shelfLen(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jint {
    match unsafe { shelf(handle) } {
        Some(books) => books.len() as jint,
        None => -1,
    }
}

/// One row's numbers, in one crossing: `[id, addedAt, lastRead,
/// finishedAt, state, authorCount, collectionCount]`. Timestamps are Unix
/// seconds with 0 meaning never; `state` is 1 unread, 2 reading, 3
/// finished. Empty for a bad handle or index.
///
/// Packed rather than one call per field because a shelf of a hundred
/// books would otherwise make seven hundred JNI crossings to draw once.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_shelfBook(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    index: jint,
) -> jlongArray {
    use chapbook_reader::chapbook_library::ReadingState;
    let Some(book) = (unsafe { row(handle, index) }) else {
        return long_array_out(&env, &[]);
    };
    let state = match book.state() {
        ReadingState::Unread => 1,
        ReadingState::Reading => 2,
        ReadingState::Finished => 3,
    };
    long_array_out(
        &env,
        &[
            book.id.0,
            book.added_at,
            book.last_read.unwrap_or(0),
            book.finished_at.unwrap_or(0),
            state,
            book.authors.len() as jlong,
            book.collections.len() as jlong,
        ],
    )
}

/// One row's fractions: `[progress, seriesIndex]`, each `-1` when the
/// book has none. Empty for a bad handle or index.
///
/// A negative sentinel rather than NaN: NaN survives the crossing but
/// `-1.0` is what a Kotlin `takeIf` reads cleanly, and neither a
/// progress nor a series position is ever negative.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_shelfBookFractions(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    index: jint,
) -> jfloatArray {
    let Some(book) = (unsafe { row(handle, index) }) else {
        return float_array_out(&env, &[]);
    };
    float_array_out(
        &env,
        &[
            book.progress.unwrap_or(-1.0) as jfloat,
            book.series_index.unwrap_or(-1.0) as jfloat,
        ],
    )
}

/// One row's title; `""` for a bad handle or index.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_shelfTitle(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    index: jint,
) -> jstring {
    let title = unsafe { row(handle, index) }
        .map(|book| book.title.clone())
        .unwrap_or_default();
    string_out(&env, &title)
}

/// One row's author, by index within the row — the order the book lists
/// them. `""` past the end.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_shelfAuthor(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    index: jint,
    author: jint,
) -> jstring {
    let name = unsafe { row(handle, index) }
        .filter(|_| author >= 0)
        .and_then(|book| book.authors.get(author as usize).cloned())
        .unwrap_or_default();
    string_out(&env, &name)
}

/// One row's series; `""` for a book in none, which is most of them.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_shelfSeries(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    index: jint,
) -> jstring {
    let series = unsafe { row(handle, index) }
        .and_then(|book| book.series.clone())
        .unwrap_or_default();
    string_out(&env, &series)
}

/// One row's edition fingerprint — the SHA-1 of the file's bytes, hex.
///
/// The key an app maps its own `content://` grant to: it identifies the
/// *file* across a reinstall, while the id identifies the reader's
/// history of it.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_shelfFingerprint(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    index: jint,
) -> jstring {
    let fingerprint = unsafe { row(handle, index) }
        .map(|book| book.fingerprint.clone())
        .unwrap_or_default();
    string_out(&env, &fingerprint)
}

/// The library's own copy of the file, or `""` for an adopted book — one
/// the library holds a record of and no copy of, because the platform
/// owns the file and the app owns the grant that reaches it.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_shelfFilePath(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    index: jint,
) -> jstring {
    let path = unsafe { row(handle, index) }
        .map(|book| book.file_path.to_string_lossy().into_owned())
        .unwrap_or_default();
    string_out(&env, &path)
}

/// The cover kept at import, so a shelf need not reopen every book to
/// draw one. `""` when the book had none.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_shelfCoverPath(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    index: jint,
) -> jstring {
    let path = unsafe { row(handle, index) }
        .and_then(|book| book.cover_path.as_ref())
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default();
    string_out(&env, &path)
}

/// The id of one collection this row is in; 0 past the end.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_shelfCollectionId(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    index: jint,
    which: jint,
) -> jlong {
    unsafe { row(handle, index) }
        .filter(|_| which >= 0)
        .and_then(|book| book.collections.get(which as usize))
        .map(|collection| collection.id.0)
        .unwrap_or(0)
}

/// The name of one collection this row is in; `""` past the end.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_shelfCollectionName(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    index: jint,
    which: jint,
) -> jstring {
    let name = unsafe { row(handle, index) }
        .filter(|_| which >= 0)
        .and_then(|book| book.collections.get(which as usize))
        .map(|collection| collection.name.clone())
        .unwrap_or_default();
    string_out(&env, &name)
}

/// Every collection as `[id, bookCount, id, bookCount, ...]`, oldest
/// first. Names come from `libraryCollectionName`, since a `String[]`
/// and a `long[]` cannot cross as one array.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_libraryCollections(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jlongArray {
    let Some(library) = (unsafe { library(handle) }) else {
        return long_array_out(&env, &[]);
    };
    match library.collections() {
        Ok(collections) => {
            let flat: Vec<jlong> = collections
                .iter()
                .flat_map(|c| [c.id.0, c.books as jlong])
                .collect();
            long_array_out(&env, &flat)
        }
        Err(e) => {
            log::error!("could not list collections: {e}");
            long_array_out(&env, &[])
        }
    }
}

/// One collection's name by id; `""` if no live collection has it.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_libraryCollectionName(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    collection: jlong,
) -> jstring {
    let name = unsafe { library(handle) }
        .and_then(|library| library.collections().ok())
        .and_then(|collections| {
            collections
                .into_iter()
                .find(|c| c.id.0 == collection)
                .map(|c| c.name)
        })
        .unwrap_or_default();
    string_out(&env, &name)
}

/// Make a collection, or return the one that already has this name; 0 on
/// failure. Idempotent, so an app need not ask first.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_libraryCreateCollection(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    name: JString,
) -> jlong {
    let Some(name) = string_in(&mut env, &name) else {
        return 0;
    };
    let Some(library) = (unsafe { library(handle) }) else {
        return 0;
    };
    match library.create_collection(&name) {
        Ok(collection) => collection.0,
        Err(e) => {
            log::error!("could not create a collection: {e}");
            0
        }
    }
}

/// Rename a collection. `false` if it did not happen.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_libraryRenameCollection(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    collection: jlong,
    name: JString,
) -> jboolean {
    use chapbook_reader::chapbook_library::CollectionId;
    let Some(name) = string_in(&mut env, &name) else {
        return 0;
    };
    let Some(library) = (unsafe { library(handle) }) else {
        return 0;
    };
    ok(library.rename_collection(CollectionId(collection), &name))
}

/// Delete a collection. The books stay; only the grouping goes.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_libraryDeleteCollection(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    collection: jlong,
) -> jboolean {
    use chapbook_reader::chapbook_library::CollectionId;
    let Some(library) = (unsafe { library(handle) }) else {
        return 0;
    };
    ok(library.delete_collection(CollectionId(collection)))
}

/// Put a book in a collection. Doing it twice is not a failure.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_libraryAddToCollection(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    book: jlong,
    collection: jlong,
) -> jboolean {
    use chapbook_reader::chapbook_library::{BookId, CollectionId};
    let Some(library) = (unsafe { library(handle) }) else {
        return 0;
    };
    ok(library.add_to_collection(BookId(book), CollectionId(collection)))
}

/// Take a book out of a collection. Doing it twice is not a failure.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_libraryRemoveFromCollection(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    book: jlong,
    collection: jlong,
) -> jboolean {
    use chapbook_reader::chapbook_library::{BookId, CollectionId};
    let Some(library) = (unsafe { library(handle) }) else {
        return 0;
    };
    ok(library.remove_from_collection(BookId(book), CollectionId(collection)))
}

/// Take a book off the shelf. Soft: the row keeps its id, its position
/// and its annotations, so adding the same file back is the same book.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_libraryDeleteBook(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    book: jlong,
) -> jboolean {
    use chapbook_reader::chapbook_library::BookId;
    let Some(library) = (unsafe { library(handle) }) else {
        return 0;
    };
    ok(library.delete_book(BookId(book)))
}

/// Mark a book finished, or take the mark back.
///
/// A session records this itself on reaching the end, so this is for the
/// other direction: the "mark as read" a reader taps for a book they
/// finished elsewhere, and the undo.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_librarySetFinished(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    book: jlong,
    finished: jboolean,
) -> jboolean {
    use chapbook_reader::chapbook_library::BookId;
    let Some(library) = (unsafe { library(handle) }) else {
        return 0;
    };
    ok(library.set_finished(BookId(book), finished != 0))
}

/// The library row this session's book was imported into; 0 for a book
/// that never reached the library (an OPDS stream, or a session opened
/// without a library directory).
///
/// The join between the reading view and the shelf: the session did the
/// importing, so only it knows which row that became.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_sessionBookId(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jlong {
    unsafe { session(handle) }
        .and_then(|session| session.book_id())
        .map(|id| id.0)
        .unwrap_or(0)
}

fn long_array_out(env: &JNIEnv, values: &[jlong]) -> jlongArray {
    let Ok(array) = env.new_long_array(values.len() as i32) else {
        return JObject::null().into_raw();
    };
    if env.set_long_array_region(&array, 0, values).is_err() {
        return JObject::null().into_raw();
    }
    array.into_raw()
}

/// A library write's outcome as a `jboolean`, with the reason logged
/// rather than thrown: none of these failures is one a shelf can act on,
/// and logcat is where an Android defect gets read.
fn ok(result: chapbook_reader::chapbook_core::Result<()>) -> jboolean {
    match result {
        Ok(()) => 1,
        Err(e) => {
            log::error!("library write failed: {e}");
            0
        }
    }
}

// ---- Background loads ----
//
// The half that was missing while this binding shipped `cbz` and `pdf`:
// image books decode on the loader thread, and without a waker and a
// poll nothing ever carried the decoded pages to a redraw — a comic
// opened to its placeholder and stayed there. EPUBs lay out
// synchronously, which is why the emulator runs never noticed.

/// Install the wake callback: a `java.lang.Runnable` run once per landed
/// load, **on the loader thread**. It must only get back to the main
/// thread — `View.post`, a `Handler` — and poke [`pollLoaded`]; touching
/// a view from inside it is the bug the reference shells' wakers all
/// exist to prevent. Null clears it.
///
/// [`pollLoaded`]: Java_com_ophymx_chapbook_Native_pollLoaded
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_setWaker(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    waker: JObject,
) {
    let Some(s) = (unsafe { session(handle) }) else {
        return;
    };
    if waker.is_null() {
        s.set_waker(|| {});
        return;
    }
    let (Ok(vm), Ok(waker)) = (env.get_java_vm(), env.new_global_ref(waker)) else {
        return;
    };
    s.set_waker(move || {
        // The loader thread fires many wakes over its life: attach it as a
        // daemon once and let the JVM detach it when the thread exits,
        // rather than paying an attach/detach round trip per wake.
        if let Ok(mut env) = vm.attach_current_thread_permanently() {
            let _ = env.call_method(waker.as_obj(), "run", "()V", &[]);
        }
    });
}

/// Take delivery of anything the loader finished. Returns whether the
/// *visible* page changed, and therefore whether a repaint is worth
/// doing — prefetch landings answer false on purpose.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_pollLoaded(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jboolean {
    unsafe { session(handle) }.is_some_and(|s| s.poll_loaded()) as jboolean
}

/// Whether any unit is still being loaded — for a shell that wants a
/// spinner; one that just repaints on wake does not need it.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_hasPendingLoads(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jboolean {
    unsafe { session(handle) }.is_some_and(|s| s.has_pending_loads()) as jboolean
}

// ---- Session events ----

/// The next session event, oldest first, packed:
/// `(kind << 56) | (spine << 28) | page`, or -1 when there is none.
/// Kinds: 0 unit loaded, 1 unit failed (its message waits in
/// [`eventMessage`]), 2 position changed, 3 book finished.
///
/// [`eventMessage`]: Java_com_ophymx_chapbook_Native_eventMessage
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_nextEvent(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jlong {
    let Some(shell) = (unsafe { shell(handle) }) else {
        return -1;
    };
    if shell.events.is_empty() {
        shell.events.extend(shell.session.drain_events());
    }
    let Some(event) = shell.events.pop_front() else {
        return -1;
    };
    shell.event_message = None;
    let (kind, spine, page): (jlong, usize, usize) = match event {
        SessionEvent::UnitLoaded { spine } => (0, spine, 0),
        SessionEvent::UnitFailed { spine, message } => {
            shell.event_message = Some(message);
            (1, spine, 0)
        }
        SessionEvent::PositionChanged { spine, page } => (2, spine, page),
        SessionEvent::BookFinished => (3, 0, 0),
    };
    (kind << 56) | ((spine as jlong & 0x0fff_ffff) << 28) | (page as jlong & 0x0fff_ffff)
}

/// The message belonging to the event [`nextEvent`] just returned — a
/// unit failure's reason, for a person to read. Null for every other
/// kind. Replaced by the next call.
///
/// [`nextEvent`]: Java_com_ophymx_chapbook_Native_nextEvent
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_eventMessage(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jstring {
    match unsafe { shell(handle) }.and_then(|s| s.event_message.take()) {
        Some(message) => string_out(&env, &message),
        None => JObject::null().into_raw(),
    }
}

// ---- The rest of the reading model ----

/// How many spine units the book has — the denominator of "ch 2/8".
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_spineLen(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jint {
    unsafe { session(handle) }.map_or(-1, |s| s.spine_len() as jint)
}

/// How many pages the current unit laid out to; 0 until metrics arrive.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_pageCount(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jint {
    unsafe { session(handle) }.map_or(-1, |s| s.page_count() as jint)
}

/// What kind of book: 0 EPUB, 1 comic, 2 PDF — the difference between a
/// title bar saying "ch" and one saying "pg".
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_bookKind(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jint {
    use chapbook_reader::chapbook_core::BookKind;
    unsafe { session(handle) }.map_or(-1, |s| match s.kind() {
        BookKind::Epub => 0,
        BookKind::Comic => 1,
        BookKind::Pdf => 2,
    })
}

// ---- Settings ----
//
// The C ABI's lesson, kept: a flat setter cannot carry the font family,
// so the family travels on its own calls and the flat setter preserves
// whatever family is in force rather than clearing it.

/// The current settings, flattened:
/// `[base_font_px, line_height, justify, publisher_styles, theme]`,
/// booleans as 0/1 and the theme as 0 light, 1 sepia, 2 dark.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_settings(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jfloatArray {
    let values: Vec<jfloat> = unsafe { session(handle) }
        .map(|s| {
            let settings = s.settings();
            vec![
                settings.base_font_px,
                settings.line_height,
                settings.justify as u8 as jfloat,
                settings.publisher_styles as u8 as jfloat,
                match settings.theme {
                    Theme::Light => 0.0,
                    Theme::Sepia => 1.0,
                    Theme::Dark => 2.0,
                },
            ]
        })
        .unwrap_or_default();
    float_array_out(&env, &values)
}

/// Replace the scalar settings, preserving the font family. `thisBook`
/// scopes the change to the open book instead of the reader default;
/// both persist through the library when the session has one.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "system" fn Java_com_ophymx_chapbook_Native_setSettings(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    base_font_px: jfloat,
    line_height: jfloat,
    justify: jboolean,
    publisher_styles: jboolean,
    theme: jint,
    this_book: jboolean,
) {
    let Some(s) = (unsafe { session(handle) }) else {
        return;
    };
    let settings = ReadingSettings {
        base_font_px,
        line_height,
        justify: justify != 0,
        publisher_styles: publisher_styles != 0,
        theme: match theme {
            1 => Theme::Sepia,
            2 => Theme::Dark,
            _ => Theme::Light,
        },
        ..s.settings().clone()
    };
    let scope = if this_book != 0 {
        SettingsScope::ThisBook
    } else {
        SettingsScope::Global
    };
    s.set_settings(settings, scope);
}

/// The reader's chosen typeface, or null for the publisher's.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_fontFamily(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jstring {
    match unsafe { session(handle) }.and_then(|s| s.settings().font_family.clone()) {
        Some(family) => string_out(&env, &family),
        None => JObject::null().into_raw(),
    }
}

/// Choose a typeface by family name — one of [`fontFamilies`]' answers —
/// or null to give the publisher's back.
///
/// [`fontFamilies`]: Java_com_ophymx_chapbook_Native_fontFamilies
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_setFontFamily(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    family: JString,
    this_book: jboolean,
) {
    let Some(s) = (unsafe { session(handle) }) else {
        return;
    };
    let family = if family.is_null() {
        None
    } else {
        string_in(&mut env, &family)
    };
    let scope = if this_book != 0 {
        SettingsScope::ThisBook
    } else {
        SettingsScope::Global
    };
    s.set_font_family(family, scope);
}

/// Every family the session's font database offers — what a picker lists.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_fontFamilies(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jni::sys::jobjectArray {
    let families = unsafe { session(handle) }
        .map(|s| s.font_families())
        .unwrap_or_default();
    let Ok(class) = env.find_class("java/lang/String") else {
        return JObject::null().into_raw();
    };
    let Ok(array) = env.new_object_array(families.len() as jint, class, JObject::null()) else {
        return JObject::null().into_raw();
    };
    for (index, family) in families.iter().enumerate() {
        let Ok(value) = env.new_string(family) else {
            continue;
        };
        let _ = env.set_object_array_element(&array, index as jint, value);
    }
    array.into_raw()
}

// ---- Sync ----
//
// The engine and worker are chapbook-sync's; what this section adds is
// the transport and the drain, both spelled for a platform that owns its
// networking. The binding deliberately bundles no TLS: a Rust stack here
// would trust webpki-roots and ignore the user's CAs, enterprise roots
// and network security config, so the transport is a Kotlin object over
// the platform's own HTTP — and it must write, because reconciling marks
// means POST, PUT and DELETE. Credentials are the transport's business
// too: attach `Authorization` per request from whatever store the app
// keeps, which is why no credential type crosses here at all.

/// What a sync handle points at: the worker, plus the drain state the
/// flattened `syncNext`/`syncDetail` pair reads.
struct SyncHandle {
    worker: chapbook_sync::SyncWorker,
    pending: std::collections::VecDeque<chapbook_sync::SyncEvent>,
    detail: Option<String>,
    marks_error: Option<String>,
}

/// # Safety
/// `handle` must have come from `syncOpen` and not yet been closed.
unsafe fn sync_handle<'a>(handle: jlong) -> Option<&'a mut SyncHandle> {
    (handle as *mut SyncHandle).as_mut()
}

/// The Kotlin transport, held as a global ref and driven from the sync
/// worker's thread.
struct KtTransport {
    vm: jni::JavaVM,
    transport: jni::objects::GlobalRef,
}

use chapbook_sync::{HttpError, HttpRequest, HttpResponse};

impl KtTransport {
    /// Attach the worker thread (as a daemon, once — it lives for the
    /// worker's life) and run one call, translating a thrown exception
    /// into the transport failure it is.
    fn with_env<T>(
        &self,
        f: impl FnOnce(&mut JNIEnv) -> Result<T, jni::errors::Error>,
    ) -> Result<T, HttpError> {
        let mut env = self
            .vm
            .attach_current_thread_permanently()
            .map_err(|e| HttpError::new(format!("cannot attach to the JVM: {e}")))?;
        let result = f(&mut env);
        if env.exception_check().unwrap_or(false) {
            let thrown = env.exception_occurred().ok();
            env.exception_clear().ok();
            let message = thrown
                .and_then(|exc| {
                    let value = env
                        .call_method(&exc, "toString", "()Ljava/lang/String;", &[])
                        .ok()?
                        .l()
                        .ok()?;
                    env.get_string(&jni::objects::JString::from(value))
                        .ok()
                        .map(String::from)
                })
                .unwrap_or_else(|| "the transport threw".to_string());
            return Err(HttpError::new(message));
        }
        result.map_err(|e| HttpError::new(format!("transport call failed: {e}")))
    }
}

/// Request headers as Kotlin sees them: one flat array, names and values
/// interleaved.
fn kt_headers<'l>(
    env: &mut JNIEnv<'l>,
    headers: &[(String, String)],
) -> Result<jni::objects::JObjectArray<'l>, jni::errors::Error> {
    let class = env.find_class("java/lang/String")?;
    let array = env.new_object_array((headers.len() * 2) as jint, class, JObject::null())?;
    for (index, (name, value)) in headers.iter().enumerate() {
        let name = env.new_string(name)?;
        env.set_object_array_element(&array, (index * 2) as jint, name)?;
        let value = env.new_string(value)?;
        env.set_object_array_element(&array, (index * 2 + 1) as jint, value)?;
    }
    Ok(array)
}

/// A `SyncResponse` read back into the engine's shape.
fn kt_response(env: &mut JNIEnv, response: JObject) -> Result<HttpResponse, jni::errors::Error> {
    let status = env.get_field(&response, "status", "I")?.i()? as u16;
    let content_type = {
        let value = env
            .get_field(&response, "contentType", "Ljava/lang/String;")?
            .l()?;
        if value.is_null() {
            None
        } else {
            Some(String::from(
                env.get_string(&jni::objects::JString::from(value))?,
            ))
        }
    };
    let mut headers = Vec::new();
    let raw = env
        .get_field(&response, "headers", "[Ljava/lang/String;")?
        .l()?;
    if !raw.is_null() {
        let raw = jni::objects::JObjectArray::from(raw);
        let len = env.get_array_length(&raw)?;
        let mut pair = 0;
        while pair + 1 < len {
            let name = env.get_object_array_element(&raw, pair)?;
            let value = env.get_object_array_element(&raw, pair + 1)?;
            if !name.is_null() && !value.is_null() {
                headers.push((
                    String::from(env.get_string(&jni::objects::JString::from(name))?),
                    String::from(env.get_string(&jni::objects::JString::from(value))?),
                ));
            }
            pair += 2;
        }
    }
    let body = {
        let raw = env.get_field(&response, "body", "[B")?.l()?;
        if raw.is_null() {
            Vec::new()
        } else {
            env.convert_byte_array(jni::objects::JByteArray::from(raw))?
        }
    };
    Ok(HttpResponse {
        status,
        content_type,
        headers,
        body: Box::new(std::io::Cursor::new(body)),
    })
}

impl chapbook_sync::HttpClient for KtTransport {
    fn get(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        self.with_env(|env| {
            let url = env.new_string(&request.url)?;
            let headers = kt_headers(env, &request.headers)?;
            let response = env
                .call_method(
                    self.transport.as_obj(),
                    "get",
                    "(Ljava/lang/String;[Ljava/lang/String;)Lcom/ophymx/chapbook/SyncResponse;",
                    &[(&url).into(), (&headers).into()],
                )?
                .l()?;
            kt_response(env, response)
        })
    }

    fn send(
        &self,
        method: chapbook_sync::HttpMethod,
        request: HttpRequest,
        body: Option<Vec<u8>>,
    ) -> Result<HttpResponse, HttpError> {
        self.with_env(|env| {
            let verb = env.new_string(method.as_str())?;
            let url = env.new_string(&request.url)?;
            let headers = kt_headers(env, &request.headers)?;
            let body = env.byte_array_from_slice(&body.unwrap_or_default())?;
            let response = env
                .call_method(
                    self.transport.as_obj(),
                    "send",
                    "(Ljava/lang/String;Ljava/lang/String;[Ljava/lang/String;[B)\
                     Lcom/ophymx/chapbook/SyncResponse;",
                    &[
                        (&verb).into(),
                        (&url).into(),
                        (&headers).into(),
                        (&body).into(),
                    ],
                )?
                .l()?;
            kt_response(env, response)
        })
    }
}

/// Start a sync worker over a library. The transport is required — see
/// the section comment for why nothing is bundled. `waker` is a
/// `Runnable` fired on the worker thread once per queued report, or null
/// to poll. `deviceId` is minted once by the app and reused forever;
/// `deviceName` is for people.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_syncOpen(
    mut env: JNIEnv,
    _class: JClass,
    library_dir: JString,
    device_id: JString,
    device_name: JString,
    transport: JObject,
    waker: JObject,
) -> jlong {
    use chapbook_reader::chapbook_library::Library;
    let (Some(dir), Some(id), Some(name)) = (
        string_in(&mut env, &library_dir),
        string_in(&mut env, &device_id),
        string_in(&mut env, &device_name),
    ) else {
        return 0;
    };
    if transport.is_null() {
        log::error!("syncOpen needs a transport; this binding bundles none on purpose");
        return 0;
    }
    let (Ok(vm), Ok(transport)) = (env.get_java_vm(), env.new_global_ref(transport)) else {
        return 0;
    };
    let library = match Library::open(std::path::Path::new(&dir)) {
        Ok(library) => library,
        Err(e) => {
            log::error!("sync cannot open the library at {dir}: {e}");
            return 0;
        }
    };
    let engine = chapbook_sync::SyncEngine::new(
        library,
        std::sync::Arc::new(KtTransport { vm, transport }),
        chapbook_sync::Device { id, name },
    );
    let waker: std::sync::Arc<dyn Fn() + Send + Sync> = if waker.is_null() {
        std::sync::Arc::new(|| {})
    } else {
        let (Ok(vm), Ok(waker)) = (env.get_java_vm(), env.new_global_ref(waker)) else {
            return 0;
        };
        std::sync::Arc::new(move || {
            if let Ok(mut env) = vm.attach_current_thread_permanently() {
                let _ = env.call_method(waker.as_obj(), "run", "()V", &[]);
            }
        })
    };
    Box::into_raw(Box::new(SyncHandle {
        worker: chapbook_sync::SyncWorker::spawn(engine, waker),
        pending: std::collections::VecDeque::new(),
        detail: None,
        marks_error: None,
    })) as jlong
}

/// Ask for every book with a service. Returns whether the worker took it.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_syncRequestAll(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jboolean {
    unsafe { sync_handle(handle) }
        .is_some_and(|s| s.worker.request(chapbook_sync::SyncCommand::All)) as jboolean
}

/// Ask for one book.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_syncRequestBook(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    book: jlong,
) -> jboolean {
    use chapbook_reader::chapbook_library::BookId;
    unsafe { sync_handle(handle) }.is_some_and(|s| {
        s.worker
            .request(chapbook_sync::SyncCommand::Book(BookId(book)))
    }) as jboolean
}

/// The next report, flattened: `[kind, book, position, created, updated,
/// deleted, adopted, refreshed, merged, conflicts, books, withdrawn,
/// truncated]`, or an empty array when none waits. The last two ride at
/// the end so the indices before them never moved: `withdrawn` is
/// another device's deletions arriving, `truncated` is 1 when the
/// container had more pages than one pass reads — no deletion was
/// inferred from a listing that stopped early. Kinds: 0 book, 1 book failed, 2 finished;
/// positions: 0 idle, 1 pushed, 2 pulled, 3 refused, 4 conflict,
/// 5 failed. The strings ride [`syncDetail`] and [`syncMarksError`].
///
/// [`syncDetail`]: Java_com_ophymx_chapbook_Native_syncDetail
/// [`syncMarksError`]: Java_com_ophymx_chapbook_Native_syncMarksError
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_syncNext(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jlongArray {
    use chapbook_sync::{PositionReport, SyncEvent};
    let Some(sync) = (unsafe { sync_handle(handle) }) else {
        return long_array_out(&env, &[]);
    };
    if sync.pending.is_empty() {
        sync.pending.extend(sync.worker.drain());
    }
    let Some(event) = sync.pending.pop_front() else {
        return long_array_out(&env, &[]);
    };
    sync.detail = None;
    sync.marks_error = None;
    let mut values = [0 as jlong; 13];
    match event {
        SyncEvent::Book(report) => {
            values[0] = 0;
            values[1] = report.book.0;
            values[2] = match report.position {
                PositionReport::Idle => 0,
                PositionReport::Pushed => 1,
                PositionReport::Pulled => 2,
                PositionReport::Refused(why) => {
                    sync.detail = Some(why);
                    3
                }
                PositionReport::Conflict => 4,
                PositionReport::Failed(why) => {
                    sync.detail = Some(why);
                    5
                }
            };
            let marks = report.annotations;
            values[3] = marks.created as jlong;
            values[4] = marks.updated as jlong;
            values[5] = marks.deleted as jlong;
            values[6] = marks.adopted as jlong;
            values[7] = marks.refreshed as jlong;
            values[8] = marks.merged as jlong;
            values[9] = marks.conflicts as jlong;
            values[11] = marks.withdrawn as jlong;
            values[12] = marks.truncated as jlong;
            sync.marks_error = marks.failed;
        }
        SyncEvent::Failed { book, reason } => {
            values[0] = 1;
            values[1] = book.0;
            sync.detail = Some(reason);
        }
        SyncEvent::Finished { books } => {
            values[0] = 2;
            values[10] = books as jlong;
        }
    }
    long_array_out(&env, &values)
}

/// The refusal or failure belonging to the report [`syncNext`] just
/// returned, or null. Replaced by the next call.
///
/// [`syncNext`]: Java_com_ophymx_chapbook_Native_syncNext
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_syncDetail(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jstring {
    match unsafe { sync_handle(handle) }.and_then(|s| s.detail.take()) {
        Some(detail) => string_out(&env, &detail),
        None => JObject::null().into_raw(),
    }
}

/// The container-unreachable message belonging to the last report, or
/// null.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_syncMarksError(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jstring {
    match unsafe { sync_handle(handle) }.and_then(|s| s.marks_error.take()) {
        Some(message) => string_out(&env, &message),
        None => JObject::null().into_raw(),
    }
}

/// Close the worker. Blocks for the book in flight — the thread is
/// joined, so a returned close means nothing still touches the library
/// or calls the transport.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_syncClose(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) {
    if handle != 0 {
        // SAFETY: a handle from `syncOpen`, closed once.
        drop(unsafe { Box::from_raw(handle as *mut SyncHandle) });
    }
}

/// Record where a book syncs — the services off the catalog entry it was
/// downloaded from. Null holds none; two nulls make it local again.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_librarySetSyncTargets(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    book: jlong,
    progression_url: JString,
    annotation_container: JString,
) -> jboolean {
    use chapbook_reader::chapbook_library::BookId;
    let Some(library) = (unsafe { library(handle) }) else {
        return 0;
    };
    let progression = if progression_url.is_null() {
        None
    } else {
        string_in(&mut env, &progression_url)
    };
    let container = if annotation_container.is_null() {
        None
    } else {
        string_in(&mut env, &annotation_container)
    };
    ok(library.set_sync_targets(BookId(book), progression.as_deref(), container.as_deref()))
}

/// The progression service this book syncs its position to, or null.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_librarySyncProgressionUrl(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    book: jlong,
) -> jstring {
    use chapbook_reader::chapbook_library::BookId;
    let url = unsafe { library(handle) }
        .and_then(|l| l.sync_targets(BookId(book)).ok())
        .and_then(|t| t.progression_url);
    match url {
        Some(url) => string_out(&env, &url),
        None => JObject::null().into_raw(),
    }
}

/// The annotation container this book syncs its marks with, or null.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_librarySyncAnnotationContainer(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    book: jlong,
) -> jstring {
    use chapbook_reader::chapbook_library::BookId;
    let container = unsafe { library(handle) }
        .and_then(|l| l.sync_targets(BookId(book)).ok())
        .and_then(|t| t.annotation_container);
    match container {
        Some(container) => string_out(&env, &container),
        None => JObject::null().into_raw(),
    }
}

// ---- Selection, links and marks ----
//
// The gesture-shaped surface, and on Android the one that matters most:
// long-press selects a word, handles adjust by exact range, and the
// selection becomes a highlight the library keeps. Coordinates are
// logical units, the same space `tapAction` reads; locator offsets ride
// `jint` as raw `u32` bits, as the word table already does.

/// Anchor a selection at a point. Returns whether text was there.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_selectionBegin(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    x: jfloat,
    y: jfloat,
) -> jboolean {
    unsafe { session(handle) }.is_some_and(|s| s.selection_begin(x, y)) as jboolean
}

/// Extend the selection to a point — press-drag, or a moving handle.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_selectionDrag(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    x: jfloat,
    y: jfloat,
) {
    if let Some(s) = unsafe { session(handle) } {
        s.selection_drag(x, y);
    }
}

/// Select the word under a point — what a long press means on glass.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_selectWordAt(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    x: jfloat,
    y: jfloat,
) -> jboolean {
    unsafe { session(handle) }.is_some_and(|s| s.select_word_at(x, y)) as jboolean
}

/// Select an exact locator range — a search hit, an adjusted handle.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_selectRange(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    start: jint,
    end: jint,
) {
    if let Some(s) = unsafe { session(handle) } {
        s.select_range(start as u32, end as u32);
    }
}

/// Drop the selection.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_selectionClear(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) {
    if let Some(s) = unsafe { session(handle) } {
        s.selection_clear();
    }
}

/// The selection as `(start << 32) | end`, or -1 when there is none.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_selectedRange(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jlong {
    unsafe { session(handle) }
        .and_then(|s| s.selected_range())
        .map_or(-1, |(start, end)| {
            ((start as jlong) << 32) | (end as jlong & 0xffff_ffff)
        })
}

/// The selected text, collapsed the way a clipboard wants it, or null.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_selectedText(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jstring {
    match unsafe { session(handle) }.and_then(|s| s.selected_text()) {
        Some(text) => string_out(&env, &text),
        None => JObject::null().into_raw(),
    }
}

/// The link under a point, or null — checked before starting a
/// selection, so a press on a link follows it.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_linkAt(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    x: jfloat,
    y: jfloat,
) -> jstring {
    match unsafe { session(handle) }.and_then(|s| s.link_at(x, y)) {
        Some(href) => string_out(&env, &href),
        None => JObject::null().into_raw(),
    }
}

/// Follow an href. Returns whether the reader moved; an external
/// `http(s)` link answers false and is the shell's to open.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_followLink(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    href: JString,
) -> jboolean {
    let Some(href) = string_in(&mut env, &href) else {
        return 0;
    };
    unsafe { session(handle) }.is_some_and(|s| s.follow_link(&href)) as jboolean
}

/// The selection becomes a stored highlight. Returns its id, or 0 with
/// nothing selected (or no library to remember it).
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_addHighlight(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jlong {
    unsafe { session(handle) }
        .and_then(|s| s.add_highlight())
        .unwrap_or(0)
}

/// The selection becomes a note carrying `body`. Returns its id, or 0.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_addNote(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    body: JString,
) -> jlong {
    let Some(body) = string_in(&mut env, &body) else {
        return 0;
    };
    unsafe { session(handle) }
        .and_then(|s| s.add_note(&body))
        .unwrap_or(0)
}

/// Bookmark the current position. Returns its id, or 0.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_addBookmark(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jlong {
    unsafe { session(handle) }
        .and_then(|s| s.add_bookmark())
        .unwrap_or(0)
}

/// The stored highlight under a point, or 0 — what a tap on marked text
/// asks before the recolor-or-remove menu opens.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_highlightAt(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    x: jfloat,
    y: jfloat,
) -> jlong {
    unsafe { session(handle) }
        .and_then(|s| s.highlight_at(x, y))
        .unwrap_or(0)
}

/// Recolor a highlight — `"#rrggbb"`/`"#rrggbbaa"`, null for the theme's.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_setHighlightColor(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    id: jlong,
    color: JString,
) {
    let Some(s) = (unsafe { session(handle) }) else {
        return;
    };
    let color = if color.is_null() {
        None
    } else {
        string_in(&mut env, &color)
    };
    s.set_highlight_color(id, color.as_deref());
}

/// Remove a mark; the removal reaches the container on the next sync.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_removeAnnotation(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    id: jlong,
) {
    if let Some(s) = unsafe { session(handle) } {
        s.remove_annotation(id);
    }
}

/// Jump to a mark. Returns whether the reader moved; the jump pushes
/// the return position for Back, like a followed link.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_gotoAnnotation(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    id: jlong,
) -> jboolean {
    unsafe { session(handle) }.is_some_and(|s| s.goto_annotation(id)) as jboolean
}

/// How many marks the book carries.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_annotationCount(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jint {
    unsafe { session(handle) }.map_or(-1, |s| s.annotations().len() as jint)
}

/// One mark's plain data: `[id, kind, spine, progressionBits]`, the
/// last a double's raw bits. Empty past the end. Its strings ride
/// [`annotationText`] and [`annotationColor`].
///
/// [`annotationText`]: Java_com_ophymx_chapbook_Native_annotationText
/// [`annotationColor`]: Java_com_ophymx_chapbook_Native_annotationColor
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_annotation(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    index: jint,
) -> jlongArray {
    use chapbook_reader::chapbook_library::AnnotationKind;
    let values: Vec<jlong> = unsafe { session(handle) }
        .and_then(|s| {
            let rows = s.annotations();
            rows.get(index as usize).map(|row| {
                vec![
                    row.id,
                    match row.kind {
                        AnnotationKind::Bookmark => 0,
                        AnnotationKind::Highlight => 1,
                        AnnotationKind::Note => 2,
                    },
                    row.spine_index as jlong,
                    row.progression.to_bits() as jlong,
                ]
            })
        })
        .unwrap_or_default();
    long_array_out(&env, &values)
}

/// A mark's quoted text or note body, by index, or null.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_annotationText(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    index: jint,
) -> jstring {
    let text = unsafe { session(handle) }.and_then(|s| {
        s.annotations()
            .get(index as usize)
            .and_then(|r| r.text.clone())
    });
    match text {
        Some(text) => string_out(&env, &text),
        None => JObject::null().into_raw(),
    }
}

/// A mark's chosen color, by index, or null for the theme's.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_annotationColor(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    index: jint,
) -> jstring {
    let color = unsafe { session(handle) }.and_then(|s| {
        s.annotations()
            .get(index as usize)
            .and_then(|r| r.color.clone())
    });
    match color {
        Some(color) => string_out(&env, &color),
        None => JObject::null().into_raw(),
    }
}

// ---- Page zoom (image books) ----

/// The pinch: zoom around a focal point in logical units. Image books
/// only — returns whether the view changed, and always false on prose,
/// where the shell maps the gesture to font size instead.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_setPageZoom(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    zoom: jfloat,
    focus_x: jfloat,
    focus_y: jfloat,
) -> jboolean {
    unsafe { session(handle) }.is_some_and(|s| s.set_page_zoom(zoom, focus_x, focus_y)) as jboolean
}

/// Pan the zoomed page by a pointer delta, clamped at the edges. False
/// at fit, so the drag falls through to a selection or a swipe turn.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_panPage(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    dx: jfloat,
    dy: jfloat,
) -> jboolean {
    unsafe { session(handle) }.is_some_and(|s| s.pan_page(dx, dy)) as jboolean
}

/// The current zoom, 1.0 at fit.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_pageZoom(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jfloat {
    unsafe { session(handle) }.map_or(1.0, |s| s.page_zoom())
}

/// The current pan `[x, y]` in page units — with the zoom, the forward
/// map for overlays a shell draws on a zoomed page.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_pagePan(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jfloatArray {
    let (x, y) = unsafe { session(handle) }.map_or((0.0, 0.0), |s| s.page_pan());
    float_array_out(&env, &[x, y])
}

// ---- Contents, search and the locator ----
//
// The three ways a reader goes somewhere on purpose. Contents cross
// flattened, in reading order with a depth, because every consumer of a
// TOC is a list with indentation and a tree across JNI is a shape
// Kotlin would only have to rebuild. Search results are held between
// the call that runs one and the calls that read it. A locator is two
// numbers, and it is the durable position — the one the library stores
// and marks anchor to — unlike `position`, which is the view.

/// The contents, flattened: one row per entry, `[depth, spine,
/// hasSpine, hasFragment]` — four longs each, laid end to end. Labels
/// ride [`tocLabel`].
///
/// [`tocLabel`]: Java_com_ophymx_chapbook_Native_tocLabel
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_toc(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jlongArray {
    let rows: Vec<jlong> = unsafe { session(handle) }
        .map(|s| {
            fn walk(
                entries: &[chapbook_reader::chapbook_core::TocEntry],
                depth: jlong,
                out: &mut Vec<jlong>,
            ) {
                for entry in entries {
                    out.push(depth);
                    out.push(entry.spine_index.unwrap_or(0) as jlong);
                    out.push(entry.spine_index.is_some() as jlong);
                    out.push(entry.fragment.is_some() as jlong);
                    walk(&entry.children, depth + 1, out);
                }
            }
            let mut out = Vec::new();
            walk(s.toc(), 0, &mut out);
            out
        })
        .unwrap_or_default();
    long_array_out(&env, &rows)
}

/// One entry's label, by flattened index, or null past the end.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_tocLabel(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    index: jint,
) -> jstring {
    let label = unsafe { session(handle) }.and_then(|s| {
        let mut labels = Vec::new();
        fn walk(entries: &[chapbook_reader::chapbook_core::TocEntry], out: &mut Vec<String>) {
            for entry in entries {
                out.push(entry.label.clone());
                walk(&entry.children, out);
            }
        }
        walk(s.toc(), &mut labels);
        labels.get(index as usize).cloned()
    });
    match label {
        Some(label) => string_out(&env, &label),
        None => JObject::null().into_raw(),
    }
}

/// Jump to a contents entry by flattened index. False for an entry that
/// links nowhere — a section heading — which is not an error.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_gotoToc(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    index: jint,
) -> jboolean {
    let Some(s) = (unsafe { session(handle) }) else {
        return 0;
    };
    // Flatten to owned entries: `goto_toc` takes one, and the borrow of
    // the tree cannot outlive the call that mutates the session.
    fn walk(
        entries: &[chapbook_reader::chapbook_core::TocEntry],
        out: &mut Vec<chapbook_reader::chapbook_core::TocEntry>,
    ) {
        for entry in entries {
            out.push(entry.clone());
            walk(&entry.children, out);
        }
    }
    let mut flat = Vec::new();
    walk(s.toc(), &mut flat);
    let Some(entry) = flat.get(index as usize) else {
        return 0;
    };
    s.goto_toc(entry) as jboolean
}

/// Search the whole book, keeping at most `limit` hits (0 for a sane
/// cap). Returns how many were found; read them with [`searchHit`] and
/// [`searchContext`]. Blocking — run it off the UI thread.
///
/// [`searchHit`]: Java_com_ophymx_chapbook_Native_searchHit
/// [`searchContext`]: Java_com_ophymx_chapbook_Native_searchContext
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_search(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    query: JString,
    limit: jint,
) -> jint {
    let (Some(shell), Some(query)) = (unsafe { shell(handle) }, string_in(&mut env, &query)) else {
        return 0;
    };
    let limit = if limit <= 0 { 500 } else { limit as usize };
    shell.hits = shell.session.search(&query, limit);
    shell.hits.len() as jint
}

/// Search one unit — the worker-drivable half. Replaces the last
/// search's results, as [`search`] does.
///
/// [`search`]: Java_com_ophymx_chapbook_Native_search
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_searchUnit(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    spine: jint,
    query: JString,
) -> jint {
    let (Some(shell), Some(query)) = (unsafe { shell(handle) }, string_in(&mut env, &query)) else {
        return 0;
    };
    if spine < 0 || spine as usize >= shell.session.spine_len() {
        return -1;
    }
    shell.hits = shell.session.search_unit(spine as usize, &query);
    shell.hits.len() as jint
}

/// One hit's plain data: `[spine, start, end, matchStart, matchEnd]`,
/// empty past the end. Its context rides [`searchContext`].
///
/// [`searchContext`]: Java_com_ophymx_chapbook_Native_searchContext
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_searchHit(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    index: jint,
) -> jlongArray {
    let values: Vec<jlong> = unsafe { shell(handle) }
        .and_then(|shell| {
            shell.hits.get(index as usize).map(|hit| {
                vec![
                    hit.locator.spine_index as jlong,
                    hit.locator.char_offset as jlong,
                    hit.end as jlong,
                    hit.match_range.0 as jlong,
                    hit.match_range.1 as jlong,
                ]
            })
        })
        .unwrap_or_default();
    long_array_out(&env, &values)
}

/// A hit's context — the match with a little text either side,
/// whitespace collapsed, for a results list.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_searchContext(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    index: jint,
) -> jstring {
    let context = unsafe { shell(handle) }
        .and_then(|shell| shell.hits.get(index as usize).map(|h| h.context.clone()));
    match context {
        Some(context) => string_out(&env, &context),
        None => JObject::null().into_raw(),
    }
}

/// The reader's durable position, packed `(spine << 32) | offset` — the
/// one the library stores and marks anchor to, unmoved by a font-size
/// change. `position` reports the view.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_locator(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jlong {
    unsafe { session(handle) }.map_or(-1, |s| {
        let locator = s.locator();
        ((locator.spine_index as jlong) << 32) | (locator.char_offset as jlong & 0xffff_ffff)
    })
}

/// Jump to a locator. False for a spine index the book does not have;
/// an offset past the unit's text lands at its end.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_gotoLocator(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    spine: jint,
    offset: jint,
) -> jboolean {
    if spine < 0 {
        return 0;
    }
    unsafe { session(handle) }.is_some_and(|s| {
        s.goto(chapbook_reader::chapbook_core::Locator::new(
            spine as usize,
            offset as u32,
        ))
    }) as jboolean
}

/// Jump to an element id within a unit — a footnote, a cross-reference.
/// A fragment the unit does not carry lands at the unit's start.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_gotoAnchor(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    spine: jint,
    fragment: JString,
) -> jboolean {
    let Some(fragment) = string_in(&mut env, &fragment) else {
        return 0;
    };
    if spine < 0 {
        return 0;
    }
    unsafe { session(handle) }.is_some_and(|s| s.goto_anchor(spine as usize, &fragment)) as jboolean
}

/// Whether the Back action has anywhere to return to — what greys out a
/// back button.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_canGoBack(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jboolean {
    unsafe { session(handle) }.is_some_and(|s| s.can_go_back()) as jboolean
}

/// Drop this book's own settings, so it follows the reader's defaults
/// again.
#[no_mangle]
pub extern "system" fn Java_com_ophymx_chapbook_Native_clearBookSettings(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) {
    if let Some(s) = unsafe { session(handle) } {
        s.clear_book_settings();
    }
}
