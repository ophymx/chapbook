# Android

Two Gradle modules over one Rust binding:

| | |
|---|---|
| `chapbook/` | library module → AAR; `jniLibs/{arm64-v8a,x86_64}/libchapbook.so` |
| `demo/` | app module: one `View`, one book, the conformance report — the spike, kept as the binding's harness |
| `app/` | the application: Compose over the AAR, a Kotlin model layer, tests for that layer |
| `build-jni.sh` | cargo-ndk into `jniLibs`, then the two link checks below |

The native half is `crates/chapbook-jni`, a direct Rust binding over
`chapbook-reader`. It deliberately does **not** consume `chapbook.h`: JNI
is already a C ABI, and stacking one on the other is two boundaries back
to back with Rust in the middle converting both ways — `docs/STABILITY.md`
has the argument. `build-jni.sh` still compiles the header under the NDK's
clang for both ABIs, `jni.h` included, so "the C ABI works on Android" is
checked without being shipped.

## Building

```sh
sdkmanager --install "ndk;28.2.13676358"
rustup target add aarch64-linux-android x86_64-linux-android
cargo install cargo-ndk

export ANDROID_NDK_HOME=$HOME/Android/Sdk/ndk/<version>
export ANDROID_HOME=$HOME/Android/Sdk
./android/build-jni.sh release
cd android && ./gradlew :demo:assembleDebug
./gradlew :chapbook:connectedDebugAndroidTest   # with a device or AVD attached
```

Prerequisites that are not guessable:

- **The NDK is the only prerequisite, and it is not optional.** The engine
  cross-compiles clean with no NDK at all except for one C dependency:
  bundled SQLite (`libsqlite3-sys`, on every build path — the library is
  where positions live). It wants the NDK's clang; `cargo-ndk` exists to
  set `CC`/`AR` per target and nothing else needs configuring. (`ring`
  would be the second, but this binding bundles no TLS — see below.)
- **NDK 28.2 rather than the newest**: the current stable line, and it
  emits 16 KB page alignment by default, which Android 15 requires of
  anything targeting API 35+.
- **Gradle needs a JDK, not a JRE.** With only a JRE, toolchain
  auto-detection selects an installation with no compiler and fails naming
  the toolchain rather than the missing package. Android Studio's bundled
  JBR is not the escape hatch it looks like (Java 25, which Gradle
  refuses). Install a JDK (`openjdk-21-jdk` on Debian); no `JAVA_HOME`, no
  toolchain settings needed.

## The app

`app/` is a reading application over the AAR, and its shape is
`chapbook-app`'s one level up: everything that is not a widget lives in
`app/src/main/kotlin/.../model` — which books the shelf shows and in what
order (recently read first, the same default as the desktop app and the
CLI), how a file becomes a book, how a book is opened and found again,
and the threads the engine's rules demand — and nothing in that package
imports Compose, so all of it runs under `ShelfModelTest` with no screen.
The screens (`ui/`) ask the model and draw. The desktop model crate
itself is not used: it bundles `ureq`, reads credentials from the
environment, refuses adopted books and keeps English strings in Rust,
each of which is wrong on a phone, while every decision it holds already
has a Kotlin home in the AAR.

**Custody follows the grant.** A file picked through `OpenDocument` has
a read grant that persists, so it is *adopted*: the library records it
by content, keeps no copy, and `Grants` maps the fingerprint to the URI
for next launch. A file arriving through a `VIEW` intent has a one-shot
grant, so it is *imported* — copied while the bytes are still ours,
because there is no way to reach the file again. Both land on the same
shelf; `Book.filePath` tells them apart, and `Opener.open` takes
whichever door the row has.

**The page is a `View` inside Compose** (`ui/PageView.kt`), because the
render is `render_into` on a locked `Bitmap` and TalkBack wants an
accessibility node provider, and both are `View` contracts. It is the
demo's view with a gesture detector: taps in thirds, a fling toward the
leading edge (the book's, read from `readingDirection`), the middle band
answering `toggle-menu` which the view acts on itself since the engine
has no menu. Volume keys arrive at the activity and reach the page
through `KeyRouter`, installed while the reader is showing. The session
lives in `ReaderViewModel`, not the view: a rotation recreates the view
and must not reopen the book. It is opened on an IO thread, handed to
the main thread, and closed exactly once when the screen is popped. The
page reads its position *after each draw*, because a restored position
lands on the first frame, not at open. The page never goes under the
status bar or the camera cutout: the reader box takes the safe drawing
insets and the app's background fills the rest.

**Memory** is the phone's to say: `cacheBudget` is set at open to a
quarter of `ActivityManager.memoryClass`, halved on a real
`onTrimMemory` warning, and `releaseCaches` follows every warning.
`suspend` runs from `ON_STOP`, the last callback Android guarantees.

**The reader's chrome** is a title bar and a bottom bar the middle band
toggles, and four sheets off the bottom bar: contents (flattened, with
depth; the current unit bold; headings that link nowhere kept for their
children), search (unit by unit on the main thread, yielding between
units — the blocking whole-book call would want a worker, and a worker
would touch the session while the page draws; a hit jumps and stays
selected), marks (bookmark this page; every mark with its progression;
delete), and settings (text size, line height, justify, publisher
styles, theme, typeface, scoped to this book or to every book, and a
reset). The title bar shows *Return* only while the engine's Back has
somewhere to go. A long press selects the word under the finger and the
drag that follows extends it; the engine paints the selection and the
view draws the handles, which drag by re-anchoring at the other end. An
action bar floats beside the selection: highlight, note, copy. A tap on
a stored highlight opens a colour menu with a remove. A tap runs the
contract's three hit tests in order — link, highlight, band — and an
`http(s)` link the engine declines goes to a browser. A pinch zooms an
image book around the fingers and steps the text size on prose. Back
peels one layer at a time: sheet, selection, chrome, book.

Verified on a Pixel 6 Pro (Android 17): the shelf with covers, search,
sort and state filters; a book in through the `VIEW` intent; page turns
by tap, fling and volume key, one page per input; the chrome toggling
from the middle band; position restored across close and reopen;
rotation keeping the book open; home, a memory trim and return; a
contents jump and the Return that follows it; sepia and a larger size
from the settings sheet; a long-pressed word highlighted, recoloured
from its menu, listed in marks, and given a note; search for a word
with its matches bold and a hit selected on its page; the page's text
runs in the accessibility tree with geometry. `ShelfModelTest` (5),
`ReaderFlowTest` (3) and the library module's tests (7) run through
`connectedDebugAndroidTest`. Not exercised by that drive, because `adb
input` has no second finger and the fixture has no external link: the
pinch and the browser hand-off. Note that a `connected*AndroidTest` run
uninstalls the app afterwards and takes its data with it — an empty
shelf after a test run is that, not a bug.

**The catalog** (`ui/CatalogScreen.kt`, `model/Catalog*.kt`) browses OPDS
over the app's own OkHttp client — the same one that loads covers and, in
a later phase, drives sync. A saved-catalogs list adds and removes
catalogs by URL; opening one browses it feed by feed, a navigation row
pushing a crumb and Back walking them before it leaves the screen. Facets
cross grouped as the catalog groups them (one control per group, the
facets of a group alternatives), paging is infinite scroll off the feed's
`next` link, and search is the feed's own. A publication's **Get** enqueues
a `WorkManager` job — the background-download flow the binding already
had a test for: describe the fetch off the entry while the feed is open,
run it as a foreground job with a progress notification, import the file,
record the two sync services. The shelf shows a downloaded book the
moment its job succeeds. A 401 becomes a login form drawn from the
catalog's authentication document; signing in stores the credential in
the **Keystore**, keyed by origin (never a catalog URL, whose path may be
a secret), and OkHttp attaches it per request — so a token is added when
a transfer runs, not persisted in `WorkManager`'s input, and the trust
store stays the device's because no Rust TLS ships. Cleartext is allowed
only for `localhost` and the emulator's host alias, for a dev server
reached through `adb reverse`.

Binding: the Kotlin `Catalog` gained `facets()` over two new JNI entry
points (`catalogFacets`, `catalogFacetText`), which flatten the feed's
facet groups the C ABI already carried.

Verified against a live OPDS server (mocklib, and by extension any real
one) reached from the Pixel through `adb reverse`: browse, facets,
paging, covers, a download landing on the shelf, and — against an
auth-required instance — a 401 becoming a login and a sign-in reaching
the feed. Device tests: `CatalogFacetsTest` in the library module (facets
grouped and resolved) and `CatalogModelTest` in the app (origin parsing,
a Keystore credential round-trip, the OkHttp transport meeting the
engine's contract against a `MockWebServer`).

Toolchain: OkHttp is pinned to 5.4.0, the last release built for
`compileSdk 36`; 5.5 wants 37, part of the same deferred toolchain move
the AndroidX line is pinned around.

Not yet built: sync, then polish and an emulator CI job — the phases that
follow.

The app's dependencies are pinned to the last releases built against
`compileSdk 36`: everything after mid-2026 wants `compileSdk 37` and
AGP 9.1, which is a toolchain move for all three modules and a task of
its own. Kotlin is 2.4.20 across the project; `kotlinOptions` is gone
from that line, so the modules use `kotlin { compilerOptions { … } }`.

## What the platform lends you, and what it does not

The NDK's sysroot is a fixed list, and the dynamic linker refuses
everything outside it. No `libsqlite`, no `libssl`, no `libcrypto`:
Android's own SQLite and BoringSSL are reachable only from Java. So SQLite
is bundled (`rusqlite`'s `bundled` feature is the only supported
arrangement for native code, not a workaround) and crypto is bundled too.
The part that must **not** be bundled is the trust store: `webpki-roots`
ignores enterprise roots, user CAs, network security config and OS root
updates. OPDS and sync ship on Android with no Rust TLS at all: the
binding's `opds` feature leaves `ureq` off, and `Catalog` and `SyncWorker`
both fetch through a Kotlin `SyncTransport` over the platform's own HTTP
stack, so the trust store is the device's by construction and the app
attaches its own credentials per request.
Two sysroot entries are actively wanted: `libjnigraphics`
(`AndroidBitmap_lockPixels`, the `render_into` destination) and
`libnativewindow` for a `SurfaceView` path later.

## Failure modes a green build cannot see

Both produce a complete, installable app that dies on the device, and
`build-jni.sh` checks both after every build:

- A missing `#[link(name = "jnigraphics")]` leaves `AndroidBitmap_*`
  symbols `UND` with no `DT_NEEDED` entry; nothing fails until
  `System.loadLibrary`.
- A drifted `external fun` name fails at first call — Kotlin and Rust
  never reference each other at compile time.

What that check does not reach is whether a call *answers correctly*,
and the one place that is asserted is `chapbook/src/androidTest`. It is
instrumented rather than a JVM unit test because the `.so` links
`libjnigraphics`, which no desktop JVM can load, so it runs on a device
or the AVD (`connectedDebugAndroidTest`) and CI, having no emulator, does
not run it. `DownloadFlowTest` pins the background-download flow taken
apart — `Catalog.downloadRequest` → a `WorkManager` job the app runs →
`Library.importFile` → `Library.setSyncTargets` — against the same
fixtures and properties `swift test` pins for iOS: the URL crosses
absolute, the id opaque, `Accept` alone with no `Authorization`, nothing
fetched while describing, a navigation row answers null, an extensionless
file shelves and is not consumed, the same bytes twice are one row, the
services read back after the import, and junk fails instead of crashing.
The navigation-row check is the one that catches the bug this class of
test exists for: swap the download-URL field for the href and it fails.
What no test here reaches is a worker actually surviving a suspended
process; that is device-only and the platform's promise.

## Conventions worth keeping

- **Actions cross as their names** (`"next-page"`), not ordinals: `Action`
  is `#[non_exhaustive]` and Kotlin cannot notice a reorder. Keycodes go
  the other way — translated in Rust — because `KEYCODE_VOLUME_UP` can
  never be renumbered and our ordinals can.
- **Divide `MotionEvent` coordinates by density** before `tapAction`: the
  event is in view pixels, metrics are logical units, and forwarding raw
  coordinates puts every tap in the last band on a 3x screen with no
  error anywhere.
- **`onKeyDown` returns what `applyAction` says.** `Unchanged` still
  consumes the event — that is the volume slider staying off the last
  page of the book — and `onKeyUp` must claim the key without acting, or
  the system acts (consume only down) or two pages turn per press (act on
  both).
- **A descriptor-opened book keeps its place**: it is adopted into the
  library by content fingerprint. What the engine cannot do is reopen the
  file — take a persistable URI permission and re-resolve it on launch,
  before constructing the session.
- **`Library` is the shelf**, and it is what turns those fingerprints
  into a browsable list: `books(BookQuery)` over search, collection,
  series, reading state, sort and paging, plus collections and "mark as
  read". A book gets there by being *opened*, so an app's "add to
  library" is a `Session.open`/`openFd` and `Session.bookId()` names the
  row it created. `Book.fingerprint` is the key to store a URI grant
  under: it identifies the file across a reinstall, while the id
  identifies the reader's history of it. A `Library` may be held while a
  session is open — the database is WAL — but like a session it belongs
  to one thread at a time.

Release `.so` size, for the record: 16.4 MB arm64 with CBZ and PDF built
in. The answers when it matters are feature flags and per-ABI bundle
splits, same as every other target.
