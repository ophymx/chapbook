# Writing a shell

Chapbook is an engine, not an app. A **shell** is the part you write: it
owns a window or a panel, it owns input, and it owns the process. Between
those, it drives one object — `chapbook_reader::Session` — which owns
everything else: the open publication, fonts, layout, the reading
position, settings, selection, annotations, and the library.

This document is the contract from the shell's side. `ARCHITECTURE.md`
explains how the engine computes a page; nothing here is about that.
`PLATFORM.md` explains which seams are substitutable; this is how to sit
on top of them.

There are five shells in the workspace to read alongside it:

| | crate | what it shows |
|---|---|---|
| smallest | `chapbook-viewer/examples/minimal.rs` | the whole contract, nothing else |
| desktop | `chapbook-viewer` | selection, links, clipboard, touch, GPU |
| toolkit | `chapbook-viewer-gtk` (Linux only) | the same session under someone else's main loop |
| platform | `chapbook-viewer-win32` (Windows only) | the same session under a main loop the shell pumps itself |
| device | `tools/chapbook-cli/examples/show.rs` | rasterizing yourself, panel policy, damage |

Start from `minimal.rs`. It exists to be copied.

A shell that is also an *app* — a shelf, catalogs, sign-ins, downloads,
sync — has a second contract above this one: `chapbook-app`, which
holds the decisions an app makes so that no front end makes them
twice. `APP.md` is that contract; this document stays about the session.

## The shape of a shell

```rust
let mut session = Session::open(&source, fonts)?;  // 1. open, with fonts
session.set_metrics(metrics);                      // 2. say how big a page is
loop {
    // 3. turn input into Session calls
    if let Some(action) = keys.action(engine_key(event)?) {
        let outcome = session.apply(action);  // Changed / Unchanged / Unhandled
        if !outcome.consumed() {
            pass_to_platform(event);
        }
    }
    // 4. ask what to draw, and draw it
    if let Some(pixmap) = session.render() {
        blit(&pixmap);
    }
}
session.save_position();                           // 5. leave a bookmark
```

Everything below is a detail of one of those five steps. The order
matters: `Session` produces nothing at all until it has metrics, because
it does not know what a page is until you tell it.

## 1. Opening

`Session::open` takes a string and dispatches on it: a path to an `.epub`,
a `.cbz`, or a `.pdf`, or an OPDS URL for a page-streamed comic. Which of
those compile in is a feature choice — `cbz`, `pdf` and `opds` are all on
by default and a device build turns off what its hardware will never open.
A format that was compiled out is refused at open time, with
`ChapbookError::FormatNotBuilt`, rather than at compile time — so the same
shell source builds against every configuration.

It also takes a `FontSource`, and that argument is required. On a desktop
you want `FontSource::host()`:

```rust
let session = Session::open(&source, FontSource::host())?;
```

The reason it is not the default is that "no fonts" fails silently. A
session with an empty font database lays out, renders, paints and passes
conformance — it just paginates every book to one blank page, so there is
nowhere to navigate to, nothing for search to find, and no page for a TOC
entry to land on. And fontdb has no Android, iOS or wasm branch, so an
empty database is the *ordinary* result on three platforms rather than a
corner case. A shell on one of those supplies its own:

```rust
// Android: the faces are there, but nothing scans for them, the generics
// name Microsoft families the device does not have, and cosmic-text's
// fallback list for this target is empty.
let session = Session::open(&source, FontSource::android_system())?;
```

`FontSource` names three things separately, because a build can get any
one right and the others wrong: which faces exist, what the five CSS
generics mean, and what to try when a glyph is missing. `chapbook_core::font`
documents each.

For the middle one, `Generics::Platform` is usually what you want. It
takes chapbook's own table for the target you built for — Android and iOS
have one, and everywhere else it means the same as `Generics::Host`,
because fontconfig or fontdb already answered correctly there. The
alternative is spelling five families out per platform in your shell,
which works and which every shell was otherwise going to do separately.
`Generics::Explicit` still overrides it when you know better.

Two things to read back after opening. `session.font_report()` says how
many faces loaded and names any generic that resolved to a family nothing
carries — worth printing once at startup on a platform you have not run on,
since none of these failures announce themselves. `session.font_families()`
lists what the session can match, which is what a font-family picker needs.

Opening also imports or matches the book in the library, which is how
step 5 has somewhere to put a position. Say where it lives with
`SessionConfig::with_library_dir`; leaving it unset asks
`Library::default_dir()`, which is `$CHAPBOOK_LIBRARY_DIR` when set, else
each desktop platform's own convention — the XDG data directory on Linux,
`~/Library/Application Support` on macOS, `%APPDATA%` on Windows. Anywhere
with no such convention (Android, iOS, wasm, a daemon with no `HOME`) it
is an error rather than a guess, because the guess it used to make was a
relative path and a library in whatever directory the process started in.
If the library cannot be opened at all the session says so on stderr and
reads on without one. A shell that wants no library — a preview pane, a
test — points `with_library_dir` at a scratch directory.

## 2. Metrics

```rust
session.set_metrics(PageMetrics {
    size: Size::new(600.0, 800.0),   // the whole page, margins included
    margins: EdgeSizes::uniform(32.0),
    dpi_scale: 1.0,
    rotation: Rotation::None,
});
```

`size` is in CSS pixels and in **reading orientation** — the orientation
text flows in, not the orientation your panel is bolted to the case in.
`dpi_scale` is passed to rasterizers and does not affect layout: a hidpi
window sets `size` to the logical size and `dpi_scale` to the ratio, so
the same book paginates identically at every scale factor.

`rotation` is likewise not a layout input. It is applied on the way to the
panel, which is why turning the panel does not repaginate the book — see
§7.

Call `set_metrics` again on every resize and scale-factor change. It is
cheap when nothing changed (it compares first), it relayouts and keeps the
reader's place when the page box changed, and it skips the relayout
entirely when only the rotation did.

## 3. Input

The verbs below are the direct route and stay supported. Above them sits
`chapbook_core::input`, which is what to reach for first:

- **`Action`** is the vocabulary — `NextPage`, `PrevPage`, `NextUnit`,
  `PrevUnit`, `Back`, `FontUp`, `FontDown`, `CycleTheme`, `ToggleMenu` —
  and **`Session::apply(action)`** applies one. Translate your platform's
  events into an `Action` and a binding is written once rather than once
  per shell. `Action` is `#[non_exhaustive]`: bookmarks and a jump to the
  table of contents are plainly coming, so match with a fallback arm.
- **`KeyMap`** is the default binding table, and it already knows more
  than most shells deliver: `Key::TurnPrev`/`TurnNext` are the bezel
  buttons on a Kobo or a PocketBook, and the volume keys Android readers
  borrow are bound too. A desktop has a pair after all —
  `chapbook-viewer-win32` hands the two thumb buttons of a mouse to
  `TurnPrev`/`TurnNext` — which is the argument for the vocabulary being
  the engine's rather than each shell's. Your job is one function from
  your platform's key names to `Key`; `bind` and `unbind` adjust the
  rest.
- **`TapZones::action_at(x, y, &metrics)`** is the tap policy: three
  vertical bands in the reading direction, taking *panel* coordinates and
  undoing the rotation for you, so this is the one hit test you do not
  have to put through `panel_to_page` yourself. Build it with
  **`TapZones::new(session.reading_direction())`**, not `default()`. The
  book declares which edge it reads from — EPUB's
  `page-progression-direction` — and `default()` is `Ltr` with no way to
  know better, so a shell that picks for itself picks `Ltr` everywhere
  and pages manga backwards.

**Which hit test wins.** A press can land on three things and they are
not mutually exclusive, so the order is part of the contract:

1. `session.link_at(x, y)` — a footnote or a cross-reference.
2. `session.highlight_at(x, y)` — an existing annotation to recolor or
   delete.
3. `zones.action_at(x, y, &metrics)` — the page turn.

Links and highlights are exact: both are `None` unless the press is
inside the marked text, so a miss falls through to the tap zone
naturally. Ask in the other order and the turn band swallows every link
in the outer thirds of the page, which reads as "links don't work in this
app" rather than as a precedence bug.

All three take **panel** coordinates and undo the rotation internally.
That is uniform on purpose: no hit test in the reader wants page
coordinates from you.

They also all take **logical units** — the same space you gave
`set_metrics`, not your platform's raw event coordinates. On Android that
means `MotionEvent.x / density`; forward the raw value and every tap
lands in the last band on a 3x screen, silently, because a tap that
always means "next page" is not an error anything can report.

`chapbook-viewer-gtk` runs 1 and 3 and skips 2 — it has a key for adding
a highlight and no gesture for touching one — so treat the ordering above
as the contract rather than as a transcription of that file.
`chapbook-viewer-win32` runs all three, which is what the ordering was
written for: a press inside a highlight in the outer third of the page
reports the highlight there and does not turn.

## 3a. The page in front of a screen reader

A rasterized page is a picture, and a picture of text is unusable with a
screen reader. Three accessors exist for exactly that, and they are the
whole of what an accessibility tree needs: `page_text_runs` for the
lines, `speakable_page` for the words and the string they sit in, and
`range_rects` for the geometry of any locator range. `word_at` answers a
dictionary tap out of the same table.

Two shells wrap them, and reading both is worthwhile because the two
platforms ask opposite questions of the same data.
`chapbook-viewer-gtk`'s `PageArea` implements GTK's `AccessibleText`,
where the client asks for *text at a granularity around an offset*.
`chapbook-viewer-win32`'s `uia` module implements `ITextProvider`, where
the client is handed a *range object that moves its own endpoints by
unit* and asks it questions. Neither shape is the accessor's, which is
what makes the accessor a seam rather than one platform's tree written
in Rust.

Two things that module settles for anyone writing a third. Offsets on
the boundary are character offsets into the speakable string, because
that is the space `WordSpan` already carries — locator offsets stay
inside the shell. And the tree is fed from a *snapshot* taken after each
paint rather than from the session, because UI Automation calls a
provider from its own threads and `Session` is `Send` but not `Sync`;
the one thing a client asks for that a snapshot cannot answer is a
mutation, and that posts back to the UI thread. A platform whose
accessibility callbacks arrive on the UI thread — GTK's do — needs
neither.

The unit a screen reader actually navigates by is the line, and it is
exact. Paragraphs are not: the speakable page collapses whitespace and
carries no paragraph structure, so both shells resolve a paragraph to
the whole page. Giving the text surface real paragraph spans is the
engine change that would fix it in both at once.

`apply` answers two questions, not one, and you need both:
`ActionOutcome::needs_redraw()` says whether to repaint, and
`consumed()` says whether to tell your platform you took the event. They
are not the same bit and neither implies the other. The last page of a
book is `Unchanged` — nothing to repaint, but the reader still owns that
keypress, and since `KeyMap` binds the volume keys by default, an
Android shell that returns "nothing changed" to `onKeyDown` gets the
system volume slider drawn over the book. iOS's responder chain has the
same shape with quieter symptoms.

`Unhandled` is the third state and means *let the platform have this*.
Two things produce it. `ToggleMenu`, because a reader's chrome is yours
and the engine has none. And `Back` with an empty trail — which is
deliberate, and the reason the distinction is the engine's to draw rather
than yours: whether there is anywhere to return to is a fact about the
back stack, and the bottom of it is exactly where Android's and iOS's own
Back should take over and leave the reader. Forward the gesture
unconditionally; you do not have to shadow the history to know when not
to.

What is deliberately *not* here is a gesture recognizer. Your platform
ships a better one than this repo would, and its conventions — fling
velocity, edge slop, the long-press timeout someone set in accessibility
settings — are what your app is judged on. Recognize the gesture natively,
then say what it meant.

`chapbook-viewer-gtk` is the worked example. Its `engine_key` is the whole
of its keyboard translation, it unbinds `m` because it has no menu to
toggle, and it gives `TapZones` an inert middle band for the same reason.

Navigation, in rough order of how often a shell wires it up:

- `next_page` / `prev_page` — the ordinary turn. Crosses into the
  neighbouring spine unit at the ends.
- `next_unit` / `prev_unit` — chapter skip.
- `goto(locator)`, `goto_anchor(spine, fragment)`, `goto_toc(entry)` —
  jumps. `toc()` gives you the table of contents to build a menu from.
- `link_at(x, y)` then `follow_link(href)` — a tap on a link. `link_at`
  returns the href under a point, `follow_link` navigates it and returns
  false for anything that is not a reading position (an external URL is
  the shell's problem, and the shell's opportunity: open a browser).
- `back()` / `can_go_back()` — a 64-deep stack. Jumps push onto it; page
  turns deliberately do not, or "back" would just be "previous page".

**All four navigation calls return `bool`: whether the position moved.**
Use it. Do not derive it, and in particular do not compare `page()`
across a turn — a turn off the end of a unit crosses into the next one by
resetting the page to 0, so a successful move looks like a failed one.
Most books open on a single-page cover, so a loop built on that
comparison stops on the very first turn. That is not a hypothetical: it is
what the fbdev shell did on its first run against real hardware, and it is
why `Session::position()` returns the `(spine, page)` pair as one value.

Selection, links and text:

- `selection_begin(x, y)` on press (returns false if there is no text
  there — that is your cue to treat it as something else), `selection_drag`
  on move, `selection_clear` to drop it.
- `select_range(start, end)` places a selection directly, which is how you
  show a search hit or an annotation.
- `selected_text()` for the clipboard, `selected_range()` to know whether
  there is one.
- `search_unit(spine, query)` for one unit — the piece you can drive from
  a worker — and `search(query, limit)` for the whole spine, which blocks.

Settings and annotations round it out: `set_settings` with a
`SettingsScope` of `Global` or `ThisBook`, the `adjust_font` /
`cycle_theme` / `set_font_family` conveniences over it, and
`add_highlight` / `add_note` / `add_bookmark` / `annotations` /
`goto_annotation` / `remove_annotation`.

`set_font_family(Some(name), scope)` is the reader choosing a typeface;
`None` returns the book to the publisher's. Offer names from
`font_families()` — a picker cannot offer what it cannot enumerate, and
that list grows as chapters load, because a book's own `@font-face`
families join it when their unit lays out. A name nothing matches is not
an error; the cascade falls through to the next family, exactly as it
would for an unknown family in a publisher's stylesheet.

The choice **beats** the publisher's `font-family`, unlike `base_font_px`
and `line_height`, which lose to a publisher that specifies. That is
deliberate: nearly every real EPUB sets `body { font-family }`, so a
polite rule would do nothing on nearly every book. Monospace elements and
their contents keep their font, because a code listing reflowed into the
reader's serif is a bug people report rather than a preference they
expressed. `publisher_styles: false` remains the blunter instrument.

Across the C ABI the family travels on its own calls —
`cb_session_font_family_count` / `_at` to enumerate,
`cb_session_font_family` to read, `cb_session_set_font_family` to set —
because `cb_settings` is plain data a host holds by value and a string
cannot live there. `cb_session_set_settings` **preserves** the family
rather than clearing it, so changing the font size does not silently
discard the typeface.

Hit-testing takes page coordinates. If your panel is rotated, put the
event through `PageMetrics::panel_to_page` first — the engine does not see
your rotation on the way in, only on the way out.

## 4. Drawing

Two ways, and the difference is who owns the rasterizer.

**`session.render() -> Option<Pixmap>`** is the whole pipeline plus the
bundled CPU backend. A shell that just wants pixels calls this and blits.
It also applies the panel policy on the way out — the pixel format set by
`set_pixel_format`, then the rotation from the metrics — so the pixmap is
already in panel orientation and the panel's colour depth.

**`session.render_into(dst, width, height, stride) -> bool`** is the same
picture drawn straight into a buffer you already own — a locked Android
bitmap, a `CGBitmapContext`, the array behind a WASM `ImageData` — so the
engine does not allocate one per frame for you to copy out of. Ask
`session.render_size()` for the dimensions first; it accounts for rotation,
and `render_into` refuses rather than misdraws if the size does not match.
Pixels are premultiplied RGBA8888. An unrotated page with `stride ==
width * 4` costs no allocation and no copy; a rotated page or a padded
stride is correct but goes through an intermediate.

**Memory.** A session caches laid-out chapters and decoded page images, and
both accumulate as you read. `session.set_cache_budget(bytes)` caps them
together; `session.cache_bytes()` says what is held now. The default is
generous enough for a desktop and too generous for a phone — say your own
number, and lower it when the platform warns you, which evicts immediately
rather than at the next page turn. Eviction never drops the unit on screen,
and everything else is re-read and re-decoded on demand, so the only cost of
a small budget is a slower page-back.

**Diagnostics.** The engine reports through the `log` crate and installs no
backend, so by default it says nothing. Install one early — `android_logger`,
`oslog`, `console_log`, `env_logger`, whatever the platform already has — or
call `chapbook_core::log_to_stderr()` for a terminal. Records carry the
emitting crate as their target, so `chapbook_reader` and `chapbook_library`
filter apart. `error` means something the reader asked for did not happen or
state was lost; `warn` means degraded but nothing lost; `info` is worth
knowing and not a problem. A session at rest is silent — nothing is logged
per frame or per page turn.

**Lifecycle.** `session.release_caches()` gives back everything but the page
on screen — call it when the platform warns about memory. `session.suspend()`
is the stronger one: persist the position, close the database, drop the
caches. Call it when the platform says you are about to be stopped, because
that is the only guaranteed callback and it is on a clock. The session keeps
working afterwards; the library reopens on the next access.

**Dropping a session blocks** until its loader thread finishes whatever it
was fetching — which on a cold comic page over a slow network is seconds.
That is deliberate. The worker holds the publication, and for a streamed
comic the publication holds your HTTP transport; returning sooner would
hand you back control while your own context was still in use on a thread
you cannot see. If you are being torn down in a hurry, `suspend()` is what
you want — it persists and lets go without waiting on the network.

**`session.frame() -> Option<Frame>`** is the seam. `Frame` carries:

- `list` — a `DisplayList` of paint-neutral ops. There are exactly three:
  `FillRect`, `GlyphRun`, `Image`. Everything else lowers to those.
- `intent` — a `FrameIntent` saying what changed.
- `damage` — `Option<Rect>`, the region the change disturbs, or `None`
  meaning "assume the whole page", which is always correct and sometimes
  wasteful. Selections, highlights and a landed page image state a region;
  a turn, a unit change and a reflow replace the page and say so.

Glyph runs name faces in the session's font database and `Image` ops carry
keys, not pixels, so a shell rasterizing for itself also needs
`session.paint_resources()`, which hands back the `FontSystem` and the
`ImageStore` as disjoint borrows.

Both return `None` before metrics are set, and while the current unit has
no page — an image book still decoding, or a unit that failed to load.
`None` means "nothing to draw yet", not "error".

**Taking a frame consumes the change record.** The next `frame()` reports
`Repaint` until something else moves. So take one frame per paint, and do
not call `frame()` to poke the engine into doing something — if you want
the current unit laid out without consuming the record, ask for
`page_count()`.

`render()` takes a frame internally, so it consumes the record as well.
The two are alternatives, not layers: a shell that wants both the pixels
and the intent should call `frame()` and rasterize the list itself.

`FrameIntent` is ordered by how much of the page a change disturbs:

```
Repaint < Selection < Annotation < ContentArrived < PageTurn < UnitChange < Relayout
```

Several changes before a paint collapse to the strongest. A windowed shell
can ignore intent and repaint. A device shell cannot: `intent.update_class()`
maps it to the least disruptive panel update that still renders the change
faithfully, which is the difference between a page turn that feels instant
and one that flashes the whole screen black.

## 5. Position

`save_position()` captures the place as a layered locator and persists it.
Reopening the same book restores it. Call it when you exit — including the
paths that are easy to forget, like the window's close button — and after
any jump you would be sorry to lose.

`current_offset()` is the raw offset of the current page in the unit's
locator space, if you want to show or sync a position rather than store
one. `locator()` gives the full locator. `LOCATORS.md` explains what
survives a relayout and what does not, which matters the moment you sync
positions between two devices with different screens.

## 6. Background loads, and the one rule that is not negotiable

Comic pages and PDF rasterizations decode on the session's loader thread.
Text chapters do not — an EPUB chapter is a local zip read plus a cascade,
and it happens inline.

> **Never call `unit_bytes` or `resource` from your event loop.** They are
> "blocking fetch plus cache": seconds for a cold page over the network,
> and a failure mode of `ChapbookError::Network`. That is the loader
> thread's job, and the session already has one.

What a shell does instead:

```rust
// Once, at startup: let the loader wake you.
let proxy = event_loop.create_proxy();
session.set_waker(move || { let _ = proxy.send_event(()); });

// When woken:
let redraw = session.poll_loaded();
for event in session.drain_events() {
    match event {
        SessionEvent::UnitFailed { spine, message } => show_error(spine, &message),
        _ => {}
    }
}
if redraw {
    request_redraw();
}
```

`poll_loaded` drains finished loads into the caches and returns whether
**the page on screen changed**. A prefetched unit landing does not count:
nothing the reader can see moved, and treating it as a change costs a
full-page panel update. `has_pending_loads()` says whether any loads are
still in flight, which is what a placeholder page is telling the reader
about. A
shell with no thread-safe wakeup can poll `has_pending_loads` instead of
installing a waker; a shell that does neither shows placeholders forever.

The waker is called from the loader thread, so it must be `Send + Sync`
and must not touch the session. Post an event; do the work on your own
thread. Only the *sender* has to cross threads, which is what lets a
toolkit with no `Send` session still get a real wakeup — the GTK shell
pushes down a channel from the waker and receives on the main context,
where touching the session is fine. It used to poll on a 100ms timer for
want of that.

**`drain_events` is the other half of being woken.** `poll_loaded` answers
"should I repaint"; `SessionEvent` answers "is there anything to tell the
reader". Four of them:

| | |
|---|---|
| `UnitFailed { spine, message }` | A page that will never arrive. The reader is looking at a placeholder that is not going to resolve, and this is the only way to say so — the failure is recorded internally so it is not retried every frame, and it used to stop there. `message` is for a person and is free to change; do not match on it. |
| `UnitLoaded { spine }` | A background unit decoded, prefetches included — which `poll_loaded` deliberately does not report, because nothing visible moved. |
| `PositionChanged { spine, page }` | Where the reader ended up, including moves you did not make: a restored position resolving after open, a load settling the page. What a progress UI and a sync client both want. |
| `BookFinished` | The last page of the last unit, on the transition rather than on every drain, re-arming if they leave and come back. Whether that means "mark as read" is yours to decide. |

It is a queue you drain, not a callback you install, because a `Session`
is `Send` but not `Sync` and every mutation takes `&mut self` — a handler
fired from inside those could not call back into the session, which is a
rule a host would break. Turning ten pages between drains reports one
`PositionChanged`, not ten: you asked where the reader is, not for a
transcript.

## 7. Panels

A framebuffer or e-ink panel needs three things a window does not. The
panel itself is [mezzotint] — a separate crate, because nothing under
that seam knows what a book is — so this section is the join, and
mezzotint's own docs are the contract for everything below it.

**Pixel format.** `session.set_pixel_format(PixelFormat::Grey { levels, dither })`
makes `render()` reduce for a panel that cannot show full colour. Panel
policy belongs to the target, not to whichever rasterizer produced the
pixels — which is also why `chapbook_paint::rotate` lives above the
backends rather than inside one.

A shell rasterizing its own frames does the same reduction itself, and
must hand over the diffusion regions rather than a bare flag:

```rust
let dithered = frame.list.dither_regions(scale);
let whole = mezzotint::PanelRect::full(w, h);
mezzotint::encode::quantize_for(&mut pixels, w, h, whole, format, &dithered);
```

A page is not one kind of thing. Diffusing error through body text
stipples the antialiased edge of every glyph; *not* diffusing it through a
photograph turns the photograph into a silhouette. `dither_regions` asks
the display list which pixels came from images — the last point at which
anything knows — so the diffusion happens over those and nowhere else. At
sixteen levels this is a refinement; at two, which is what a 1bpp panel
has, it is the difference between a readable page and an unreadable one.

Pass `&[]` for the regions and nothing diffuses at all, which is the
right answer for a page of pure text. A panel asking for `PixelFormat::Rgba`
means "do not reduce; I will", and `quantize_for` does nothing — that
panel reduces at `submit`, where it knows the update class, so hand the
same regions to `Update::dithering_within` and let it decide.

**Rotation.** Set it in `PageMetrics` and the page is laid out unturned and
turned on the way out. `panel_size()` gives you the buffer size (axes
swapped on a quarter turn) and `panel_to_page()` untwists input
coordinates. Changing only the rotation does not rebuild the layout —
`same_layout()` is what decides that — so a shell may turn the panel as
often as it likes.

**Update classes.** `mezzotint::PanelDriver` wraps a `Panel` implementation
and takes `present(src, damage, class)`. `damage` is `None` for "the whole
panel", the same convention `Frame::damage` uses, so one passes straight
into the other — via `chapbook_paint::panel_rect(rect, page, scale,
rotation)`, which converts page coordinates to panel ones with the
rotation folded in, rounding outward so a stale sliver cannot survive.

`intent.update_class()` is an `Option`: `None` means the frame is a plain
repaint and there is nothing for the panel to do, which on e-ink is a
saving worth taking rather than a case to paper over. `RefreshPolicy`
decides separately when to spend a full flash the content did not ask
for, to pay off ghosting.

Two calls a shell owes the panel and tends to forget:
`PanelDriver::settle(src, &dithered)` repaints whatever a fast update left
degraded — cheap when nothing is owed, so an idle tick is the right place
for it — and `flush()` before tearing the panel down.

[mezzotint]: https://crates.io/crates/mezzotint

## 8. Proving it

The failure this document keeps returning to — a shell that drove the
session wrong and stopped turning pages — was invisible to every test in
the workspace, because none of them drove a shell. So there is now a
harness for exactly that:

```rust
use chapbook_reader::conformance::Harness;

#[test]
fn my_shell_drives_the_session_correctly() {
    let fonts = FontSource::embedded("fixtures/fonts", "Crimson Text");
    Harness::new(move || Session::open("fixture.epub", fonts.clone()).unwrap())
        .run()
        .assert_ok();
}
```

It opens a fresh session per check and asserts the rules this document
states: that a crossed unit still counts as a move, that turning forward
reaches the end of the book and stops, that turns are reversible, that a
resize keeps the reader's place and reports `Relayout`, that a rotation is
not a reflow, that taking a frame consumes the change record, that damage
stays inside the page, that background loads converge, that a selection
dies with its page, and that a position survives a restart.

Checks that cannot apply report `Skipped` rather than passing quietly — a
comic has no text to select, a one-chapter book has no unit to cross — so
read the report, not just the boolean.

From a terminal, against any book:

```sh
cargo run -p chapbook-reader --example conform -- mybook.epub
```

It writes to the library, because checking that a position survives a
restart means saving one. Point `CHAPBOOK_LIBRARY_DIR` somewhere scratch
if that matters.

## 9. What to depend on

Depend on `chapbook-reader` and nothing else. It re-exports everything a
shell consumes — `chapbook_core`, `chapbook_paint`, `chapbook_library`,
`chapbook_render_tinyskia`, `tiny_skia`, `cosmic_text` — specifically so a
shell cannot skew versions with the engine it is driving.

Model types come from `chapbook_core`. `chapbook_layout` — including its
`dom` and `cascade` modules — is internal; a shell that reaches into it has
found a gap in this document, and the gap is the bug.
