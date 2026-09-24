# Android

Two Gradle modules over one Rust binding:

| | |
|---|---|
| `chapbook/` | library module → AAR; `jniLibs/{arm64-v8a,x86_64}/libchapbook.so` |
| `demo/` | app module: one `View`, one book, the conformance report |
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
