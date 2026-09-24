# iOS

A Swift library for building iOS ereader apps on chapbook, the
application built on it, and a demo app that exercises the flow real
readers live or die on.

| | |
|---|---|
| `Chapbook/` | The Swift package: `Session`, sources, input, rendering, the shelf, catalogs, sync, custody helpers |
| `App/` | The application: SwiftUI over the package, a model package with tests for that layer, a hand-rolled `.app` |
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
cd App && swift test            # the app's model, on the macOS slice too
./App/build.sh                  # the app itself, for the simulator
```

The XCFramework is a build product, not checked in. It is an XCFramework
by *necessity*: the device and simulator libraries are both arm64,
differing only in a Mach-O load command, and `lipo` refuses to fat them.
The macOS slice exists so `swift test` needs no simulator.

Those six are the `apple` job in CI, in that order, and that is the whole
gate: nothing else in the workspace compiles a line of Swift. Run them
before pushing anything under `ios/` — a Linux runner will not catch a
package that does not build, and for a while nothing did.

Three Mac traps, each cheap once written down: spell simulator compiles
`xcrun -sdk iphonesimulator swiftc` (bare `swiftc -sdk` leaves the
*linker* on the macOS sysroot); a Mac that has never completed Xcode's
first launch wedges `simctl` with no error — the `xcrun simctl` shim
runs `xcodebuild -runFirstLaunch` whenever CoreSimulator is older than
the Xcode it ships with, which waits for an admin at a GUI, and from a
headless session the framework's own binary at
`/Library/Developer/PrivateFrameworks/CoreSimulator.framework/Versions/A/Resources/bin/simctl`
answers where the shim hangs; and a simulator app that wants the
Keychain needs an `application-identifier` — carried in an
`__entitlements` linker section, the way Xcode builds it, never in the
ad hoc signature, which the simulator refuses to launch.

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

`Catalog` is where a phone's books come from: `fetch(_:)` a root,
`entries()` to draw the rows, a navigation row's `href` to drill in,
`facets()` and the page URLs for a long feed's chrome, `search(_:)`
where `hasSearch()` says there is one — and two ways to get a book onto
the shelf. `download(_:into:)` is the short one: it fetches the
acquisition, imports it into the library at the directory the sessions
use, **records the sync services the entry advertises**, and answers
with the `Library.Book` row it became. Those services live in the
catalog entry and nowhere else, so a book added any other way is one
that will never reconcile.
Every call that touches the network blocks; run them off the main actor
in a `Task` whose cancellation is the app's own, because the binding
invents no worker of its own. A 401 is an answer, not a failure: the
fetch throws a `ChapbookError` whose `isAuthRequired` is true and keeps
the authentication document, `authTitle()` and `authOffersBasic()` are
what a login sheet draws, `signIn(username:password:)` is what it
submits, and the fetch is simply tried again. Images cross as URLs,
never bytes — a cover grid is what the platform's image loader is for,
sending the same `Authorization` if the catalog wants one.

**`download(_:into:)` is the wrong call for a book on a phone**, and
the second door is the reason. The whole transfer happens inside that
call, so the process has to stay alive for it — and no transport rescues
that, because a background `URLSession` refuses completion-handler tasks
and wants a delegate, precisely because a transfer that survives
suspension is a job rather than a call. Use it for a tap the reader is
watching; use the other door for anything that has to outlive the
screen. `downloadRequest(_:)` hands back a `Catalog.DownloadRequest`
describing one fetch and steps aside: `urlRequest` is what you give a
background session, and when the file lands, `Library.importFile(at:)`
puts it on the shelf and `Library.setSyncTargets` records its
services. That is the same work
`download(_:into:)` does, taken apart — which is exactly why the
services have to be read **before** the transfer starts: they live in
the catalog entry, and by the time a background download lands the feed
is usually gone. Every `Entry` carries its request from the moment
it is read, so a screen that pages — rows from feed after feed, the
catalog holding only the last — still enqueues the book the row named,
never the one the current feed holds at that index. The request is `Codable` for the same reason — a
`URLSessionTask` has one `taskDescription` to carry it through a process
restart. It deliberately carries **no credential**: the app opened this
catalog, so it already knows which one the catalog takes, and adding the
header when the transfer starts keeps the secret out of a persisted task
description and makes a token rotated in between simply fresh. Every
other field is advice — rename the file, add headers, the engine reads a
book by its bytes. `importFile(at:)` does not consume the file it is
given (that one is the platform's), and importing the same bytes twice
answers with the row they already have, so a retried job needs no
bookkeeping of its own.

`SyncWorker` reconciles the shelf with a book's services — the position
with its OPDS Progression endpoint, marks with its Web Annotation
container — recorded per book by `Catalog.download`, or by
`Library.setSyncTargets` for a book that arrived any other way,
including a download the app ran itself. The
transport is the same choice a `SessionConfiguration` makes, with the
same default (`URLSession` on iOS, so requests honor ATS, the trust
store and the app's own configuration), plus the write half sync turns
on: PUT, POST and DELETE go out through the same session, and every
response's headers cross — `ETag` and `Location` are the annotation
flows' whole concurrency story. No credential crosses the boundary; a
service behind auth wants a `URLSession` configured to attach its own.
Reports come back typed through `drainReports()`, one per book and then
`.finished`; the worker owns a thread, and letting the last reference go
joins it.

The rest of the reader's interactive surface is wrapped in the shapes
Swift expects, and the package covers every entry point in `chapbook.h`
except `cb_font_source_android_system`, which resolves to nothing off
Android. That claim has been false before — the catalog landed on the
header an hour after it was first written, with a Kotlin binding and no
Swift one — so hold a header change to it: a new `cb_` export is a Swift
change in the same commit, or the sentence comes out.

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

## The app

`App/` is a reading application over the package, and its shape is
`chapbook-app`'s one level up — and the Android app's exactly: everything
that is not a widget lives in `App/Sources/ChapbookAppModel` — which books
the shelf shows and in what order (recently read first, the same default
as the desktop app and the CLI), how a file becomes a book, how a book is
opened and found again, and the threads the engine's rules demand — and
nothing in that module imports UIKit, so all of it runs under `swift test`
on the macOS slice with no screen. The screens (`App/Sources/ChapbookApp`,
SwiftUI, iOS 17) ask the model and draw, and only `App/build.sh` compiles
them, because a `.app` is not something SwiftPM produces for iOS. The
model is its own package so its tests join the CI gate the way the
library's do; it depends on `../Chapbook` by path.

**Custody follows the grant.** A file picked through the document
picker, or opened in place from Files, carries a security scope the app
can bookmark, so it is *adopted*: the library records it by content,
keeps no copy, and `Grants` maps the fingerprint to the bookmark for
next launch. A file that landed in the app's own container — the
`Inbox` a share fills — is *imported*, copied and the inbox file
removed. Both land on the same shelf; `Library.Book.fileURL` tells them
apart, and `Opener.open` takes whichever door the row has.

**The page is a `UIView` inside SwiftUI** (`PageView.swift`), because
the render is a `CGImage` onto a layer and VoiceOver wants an element
tree, and both are UIKit contracts. It is the demo's view with
gestures: taps in thirds, a swipe toward the leading edge (the book's,
read from `readingDirection()`), the middle band answering `toggleMenu`
which the view acts on itself since the engine has no menu. A hardware
keyboard's arrows, space and page keys go through the engine's default
key table as `UIKeyCommand`s. The session lives in `ReaderViewModel`,
not the view: a rotation re-lays the view out and must not reopen the
book. It is opened on a background queue, handed to the main actor, and
its place is saved exactly once, when the screen is popped — through
`savePosition()`, because letting a session go does not save. The page
reads its position *after each draw*, because a restored position lands
on the first frame, not at open. The page never goes under the status
bar or the home indicator: the reader box takes the safe area and the
paper colour fills the rest.

**Memory** is the phone's to say: the cache budget is set at open to a
quarter of `os_proc_available_memory()`, halved on a memory warning
through `setCacheBudget`, and `releaseCaches` follows every warning.
`suspend` runs from `didEnterBackground`, the last callback iOS
guarantees.

**The reader's chrome** is a title bar and a bottom bar the middle band
toggles, and four sheets off the bottom bar: contents (flattened, with
depth; the current unit bold; headings that link nowhere kept for their
children), search (unit by unit on the main actor, yielding between
units — the blocking whole-book call would want a worker, and a worker
would touch the session while the page draws; a hit jumps and stays
selected), marks (bookmark this page; every mark with its progression;
swipe to delete), and settings (text size, line height, justify,
publisher styles, theme, typeface, the progress readout, scoped to this
book or to every book, and a reset through `clearBookSettings`). The
title bar carries a slim whole-book progress bar — spine-weighted from
the position the shell already has, no new binding call — beside a
readout the settings sheet chooses: percent, pages left in the chapter,
or the chapter and page indices. It shows *Return* only while the
engine's Back has somewhere to go. A long press selects
the word under the finger and the drag that follows extends it; the
engine paints the selection and the view draws the handles, which drag
by re-anchoring at the other end. An action bar floats beside the
selection: highlight, note, copy. A tap on a stored highlight opens a
colour menu with a remove. A tap runs the contract's three hit tests in
order — link, highlight, band — and an `http(s)` link the engine
declines goes to the system. A pinch zooms an image book around the
fingers and steps the text size on prose.

**The catalog** browses OPDS over the app's own `URLSession` — the same
one that loads covers. A saved-catalogs list adds and removes catalogs
by URL; opening one browses it feed by feed, a navigation row pushing a
crumb and Back walking them before it leaves the screen. Facets come
grouped as the catalog groups them, paging is infinite scroll off the
feed's `next` link, and search is the feed's own. A publication's
**Get** enqueues a background `URLSession` download — the flow the
library already had a test for: describe the fetch off the entry while
the feed is open, carry it in the task's `taskDescription`, and when the
file lands import it and record the two sync services. The shelf shows
a downloaded book the moment its job lands. A 401 becomes a login form
drawn from the catalog's authentication document; signing in stores the
credential in the **Keychain**, keyed by origin (never a catalog URL,
whose path may be a secret), and every door attaches it at the moment
it builds a request — `URLSession` has no interceptor — so a token is
added when a transfer starts, not persisted with the task, and the trust
store stays the device's because no Rust TLS ships. Cleartext is allowed
only on the local network, for a dev server reached from the simulator.

Two launch arguments drive the app with no finger on the glass, for a
screenshot pass or a dev loop: `-open <file>` hands a book over the way
another app would, and `-catalog <url>` adds a catalog and browses it,
storing a `user:pass@` in the URL by origin rather than keeping it.

Verified on an iPhone 16 Pro simulator (iOS 18.5), driven through those
arguments: a book handed over, adopted and opened to its first page; the
shelf listing it after a relaunch, with search, filters and sort; a
catalog behind a login (the local sync server) refusing with a 401 that
became the sign-in form, and the feed listing with its Get buttons once
the credential was stored. What that drive could not do, because it has
no touch: the chrome and its sheets, a Get landing on the shelf, the
selection gestures, the pinch — those flows are the model tests'
(`ShelfModelTests`, `CatalogModelTests` with a canned catalog behind a
`URLProtocol`, `ReaderFlowTests`; 17 through `swift test`) and, for the
screens, a device's. Not yet built: sync, notifications for a landed
download, an app icon, and a physical device run — the same phases the
Android app has ahead of it.

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
  block for reasons the engine must not try to interpret — a background
  `URLSession` actually surviving suspension, since `swift test` asserts
  that a `DownloadRequest` crosses intact and that `importFile(at:)`
  shelves what comes back, and the app's `Downloads` runs the whole job
  against a canned server, but nothing here can suspend a process and
  wake its delegate — and a live VoiceOver pass over the iOS element
  list. The macOS accessibility
  half is asserted by `swift test` (value, ranges, extents, the word
  under a point); the iOS half compiles and mirrors the
  emulator-verified Android tree, but no screen reader has walked it
  yet.
