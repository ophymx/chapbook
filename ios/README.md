# iOS

A Swift library for building iOS ereader apps on chapbook, and a demo
app that exercises the flow real readers live or die on.

| | |
|---|---|
| `Chapbook/` | The Swift package: `Session`, sources, input, rendering, sync, custody helpers |
| `demo/` | A hand-rolled `.app`: picker once, bookmark stored, cold resolve forever after |
| `build-xcframework.sh` | Rust staticlibs → `Chapbook.xcframework` (device, simulator, macOS slices) |
| `typecheck-slices.sh` | The iOS slices compiled — the half `swift test` cannot run |

The package wraps `chapbook.h` — the Contract-tier C ABI from
`crates/chapbook-ffi` — which travels inside the XCFramework with a
module map, so nothing here copies a header or generates a binding.
Swift is the consumer that cannot route around the C ABI, which is what
keeps it honest.

## Building

```sh
rustup target add aarch64-apple-ios aarch64-apple-ios-sim aarch64-apple-darwin
./build-xcframework.sh          # required first; the package binds its output
cd Chapbook && swift test       # runs natively on the macOS slice
./typecheck-slices.sh           # the iOS slices, which swift test cannot run
./demo/build.sh                 # simulator .app, no Xcode project
```

The XCFramework is a build product, not checked in. It is an XCFramework
by *necessity*: the device and simulator libraries are both arm64,
differing only in a Mach-O load command, and `lipo` refuses to fat them.
The macOS slice exists so `swift test` needs no simulator.

Those four are the `apple` job in CI, in that order, and that is the whole
gate: nothing else in the workspace compiles a line of Swift. Run them
before pushing anything under `ios/` — a Linux runner will not catch a
package that does not build, and for a while nothing did.

Two Mac traps, both cheap once written down: spell simulator compiles
`xcrun -sdk iphonesimulator swiftc` (bare `swiftc -sdk` leaves the
*linker* on the macOS sysroot), and a Mac that has never completed
Xcode's first launch wedges `simctl` with no error — it needs an admin
at a GUI once, and no headless invocation can substitute.

## The library

`Session` is the reader loop: open from a [`BookSource`] (path, bytes,
or a file descriptor the session takes ownership of), `setMetrics`,
`renderImage`, taps through `tapAction`/`apply`, `suspend` from
`didEnterBackground`. Three of its choices carry the platform findings
they were bought with:

- **`Session` is deliberately not `Sendable`.** The engine handle is
  movable between threads and never shareable — Swift 6's region
  isolation can *transfer* a session and refuses to share one, which is
  the engine's contract made compiler-checked. A corollary: a top-level
  `let` is a global, and a global can never be proven sent, so scope
  sessions to a view controller or an actor, never a singleton.
- **`renderImage` is zero-copy and that is the only form offered.** The
  `CGImage` is built over the engine-filled buffer through a
  `CGDataProvider` whose release callback owns the deallocation; the
  measured alternative (`CGBitmapContext` + `CreateImage`) costs half a
  render per frame in copying. The buffer lives exactly as long as
  CoreGraphics holds the image — freeing on any other schedule draws
  garbage with no error, which is why the library never exposes the
  buffer at all.
- **`SecurityScopedBook` encodes the custody flow**, including the two
  rules the simulator taught: the scope dance is unconditional (a URL
  in the app's own container still demands it), and a stored bookmark
  must be re-resolved before the session is constructed on every cold
  launch, because scoped access does not survive relaunch.
- **`PageAccessibility` makes the rasterized page readable** — a
  picture of text is unusable with a screen reader. One name, two
  platform-shaped halves over the engine's text surface: on iOS, one
  `UIAccessibilityElement` per visual line so VoiceOver swipes in
  reading order; on macOS, one `NSAccessibility` static-text element
  answering range, extent and point questions in UTF-16 — the
  scalar↔UTF-16 conversion lives there so it exists exactly once. A
  host owes constructing it over the page view and one `pageChanged()`
  per render, which no-ops unless the page's text actually moved and
  otherwise rebuilds the tree and announces the turn. Geometry assumes
  page space and the view's logical coordinates coincide (metrics from
  the view's own bounds, no rotation) — a rotating shell maps the rects
  the same way it maps its pixels.

`Library` is the shelf: `books(_:)` over a `Query` — search, collection,
series, reading state, sort, paging — plus collection management and
"mark as read". It sits beside a session rather than replacing it: a
book reaches the library by being *opened*, so an app's "add to library"
is a read and `Session.bookID()` says which row that became. Two notes
that shape how an app holds it. It is not `Sendable` for the same reason
`Session` is not, but unlike a session it may be held *while* one is
open — the database is WAL, and two connections is the ordinary way to
draw a shelf while a book is being read. And a `Query`'s rows are copied
out rather than left behind a cursor, because a search field issues a
query per keystroke and the list being drawn must not move underneath
the draw.

A book opened by descriptor **keeps its place**: the engine adopts it
into the library by a fingerprint of its bytes, so position, annotations
and per-book settings persist with no path ever crossing. The app's half
of custody is holding the bookmark that reaches the file again.

`SyncWorker` reconciles the shelf with a book's services — the position
with its OPDS Progression endpoint, marks with its Web Annotation
container — recorded per book with `Library.setSyncTargets` off the
catalog entry it was downloaded from. The transport is the same choice a
`SessionConfiguration` makes, with the same default (`URLSession` on
iOS, so requests honor ATS, the trust store and the app's own
configuration), plus the write half sync turns on: PUT, POST and DELETE
go out through the same session, and every response's headers cross —
`ETag` and `Location` are the annotation flows' whole concurrency story.
No credential crosses the boundary; a service behind auth wants a
`URLSession` configured to attach its own. Reports come back typed
through `drainReports()`, one per book and then `.finished`; the worker
owns a thread, and letting the last reference go joins it.

The rest of the reader's interactive surface is wrapped in the shapes
Swift expects, and the package now covers every entry point in
`chapbook.h` except `cb_font_source_android_system`, which resolves to
nothing off Android:

- **Getting somewhere** — `tableOfContents()` returns entries carrying
  their nesting and their index, `go(to:)` takes a `TOCEntry`, a
  `Locator` or an `Annotation`, and `go(toAnchor:inSpine:)` takes a
  fragment. `locator()` is the durable place to save; `position()` is
  the view. Jumps push the back trail and page turns do not, which is
  what `canGoBack()` reports.
- **Presses, in the order `docs/SHELLS.md` fixes**: `link(at:)`, then
  `highlight(at:)`, then the tap zones. Links and highlights are exact,
  so a miss falls through naturally — ask in the other order and the
  turn band swallows every link in the outer thirds, which reads as
  "links don't work in this app" rather than as a precedence bug.
  `follow(link:)` answers `false` for an external URL, which is the
  app's cue to open a browser.
- **Selection** — `beginSelection(at:)` / `dragSelection(to:)` /
  `clearSelection()` for press-drag, `selectWord(at:)` for a long
  press, `select(_:)` to place one directly, and `selectedText()` for
  the clipboard. Grab-handle geometry is `rects(for:)` over
  `selectedRange()`.
- **Marks** — `addBookmark()`, `addHighlight()`, `addNote(_:)`,
  `annotations()`, `setHighlightColor(_:for:)`, `removeAnnotation(_:)`.
  They persist in the library and travel to the book's annotation
  container on the next sync, so a removal here is a removal
  everywhere. Ids are stable; **indices are not across a mutation**, so
  re-enumerate after an add or remove.
- **Search** — `search(_:limit:)` blocks over the whole spine and
  belongs off the main actor; `searchUnit(_:for:)` is the
  worker-drivable half. A hit carries both a `locator` to jump to and
  the `locators` to hand `select(_:)`, which is how it gets painted.
- **Typefaces** — `fontFamilies()` for the picker, `fontFamily()` /
  `setFontFamily(_:scope:)` for the choice, and `FontSource` gained
  `addingDirectory(_:)`, `settingGenerics(_:)` and
  `usingPlatformGenerics()`. The generics have no partial form on
  purpose: every platform's built-in answer is wrong somewhere and
  wrong silently.
- **Pinch and pan** (`Zoom.swift`) are image-book only and answer
  `false` on reflowable text, where the same gesture means "bigger
  text" — a settings change the app maps to the font actions itself.
  Input is mapped through the zoom automatically; output geometry stays
  in fit-page space, so an app maps its own overlays forward with
  `view = fit * zoom + pan`.
- **`EngineLog.write(_:level:target:)`** puts the app's own lines in the
  engine's stream, in order with the engine's — one path and one
  ordering to read when a reader sends a bug report.

`Session.drainEvents()` is the session narrating what is not "repaint":
loads landing and failing, the position moving (moves the app did not
make included), the book finishing. Drain after a wake or an action —
the engine coalesces on its side, so draining rarely cannot miss a move
— and a progress bar, a sync client and a "mark as read" flow all stop
polling.

## The demo

First launch: picker (a fixture book can be staged into the app's
Documents to have something to pick), bookmark stored, warm open. Every
later launch: cold resolve, no picker, the same page. Outer thirds turn
pages, the middle tap toggles a status overlay. Console lines are
`DEMO`-prefixed:

```sh
xcrun simctl install booted demo/build/ChapbookDemo.app
xcrun simctl launch --console-pty booted com.ophymx.chapbook.demo
```

## Platform notes

What is settled, and what still needs a physical device.

- **Pixels are premultiplied RGBA8888, no swizzle.** Settled with the
  sepia theme — warm paper reads `R > G > B`, and a channel swap would
  have come back cold blue — byte-identical to what an Android device
  sampled. Label CoreGraphics `premultipliedLast | byteOrder32Big`.
- **`suspend()` releases the database's POSIX locks**, not just the
  position. An app holding an advisory lock on a file in a *shared*
  container when it suspends is killed by the watchdog (`0xdead10cc`).
  Irrelevant in the app's private container; load-bearing the day a
  share extension or widget forces an app group — put the library
  directory in the group container then, and suspend on the way out.
- **Fonts:** fontdb has no iOS branch, so `FontSource.host` resolves
  empty and the open refuses with a message saying so — bundle faces
  and use `.embedded`. Whether `/System/Library/Fonts` (265 faces,
  recursive scan reaches them all) is readable from inside App Sandbox
  is a device question; the simulator's yes proves nothing. If it is
  closed, bundled faces are the only real fallback — CoreText
  enumeration yields names, not bytes — and note San Francisco cannot
  be among them: it is licensed for use through the system, not for
  redistribution in a bundle.
- **Dynamic Type is the app's to honour**: map `UIContentSizeCategory`
  onto `ReadingSettings.baseFontSize`; the engine keeps the place
  across the reflow.
- **Shipping an XCFramework for others to embed** wants a
  `PrivacyInfo.xcprivacy`: bundled SQLite's `fstat`/`statfs` fall under
  Apple's required-reason categories (file timestamps, disk space).
  Check the current list at submission time.
- **Still device-only:** bundled SQLite under real App Sandbox
  confinement (the simulator ran it, but `simctl` is not the sandbox),
  bookmark revocation (file moved or deleted underneath a stored
  bookmark), iCloud placeholders — a file that is legal, named,
  and not yet downloaded, where acquiring the descriptor can fail or
  block for reasons the engine must not try to interpret — and a live
  VoiceOver pass over the iOS element list. The macOS accessibility
  half is asserted by `swift test` (value, ranges, extents, the word
  under a point); the iOS half compiles and mirrors the
  emulator-verified Android tree, but no screen reader has walked it
  yet.
