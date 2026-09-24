# chapbook

Core components for a lightweight ereader, in Rust.

A reader turns a book into pages, remembers where you are, and gets those
pages onto a screen. Chapbook treats each of those three as a seam with a
contract, not as an implementation detail:

- **Pagination is the model, not a cut applied afterwards.** Pages, break
  rules, and widows/orphans are what the layout engine computes; there is no
  scrolling document underneath being sliced up. The CSS fragmentation
  properties servo-mode stylo does not carry — `break-*`, `page-break-*`,
  `widows`, `orphans`, `hyphens` — run through chapbook-layout's own sidecar
  cascade, so a book's break rules are honoured rather than approximated.
- **A reading position is a versioned locator, not an offset into a
  layout.** `LayeredLocator` records quote context, spine fraction, and
  whole-book progression, and resolves through those layers in order, so a
  place survives relayout, a font-size change, a move to a screen of another
  size, and — via the quote layer — a replaced edition of the same book. Highlights are
  stored the same way and re-anchor exactly as a position does; positions
  exchange with other readers as EPUB CFIs.
- **A page leaves the engine as a paint-neutral display list.**
  `Session::frame()` plus `paint_resources()` is the entire backend
  contract: glyph runs, rects and image ops, and the font database and image
  store they name. tiny-skia on the CPU and vello on the GPU are two
  implementations of it, held to each other by a parity test; a Linux
  framebuffer is a third consumer. Frames carry damage and intent, so a
  screen that must be *asked* to change — e-ink, with update classes and a
  ghosting budget — is a first-class target rather than a later port.

One `Session` drives all of it: a Rust binary, Android over JNI, iOS over a
hand-written C ABI, a browser over `wasm-bindgen`. Fonts, HTTP, credentials,
and storage arrive by injection, because a stack bundled into the engine is
precisely the one a host cannot substitute.

Underneath is no webview and no full HTML5 browser — just the EPUB 3
standards (XHTML content documents and the EPUB 3 CSS profile) on strong
upstream crates: [stylo] (the CSS engine behind Firefox and Servo) for the
cascade, [cosmic-text] for shaping and line layout, [tiny-skia] for CPU
rasterization, [rbook] for EPUB container handling. That is a deliberate
trade rather than a purity claim: a web view gets the long tail — ruby,
broken markup — for free, and chapbook's layout is a real subset of what
publishers ship. What a web view cannot give back is the first two bullets
above, because its notion of where you are is a function of its own line
breaking, which moves under you when the OS updates. Chapbook is the right
shape for a controlled-typography reader and the wrong one for an app whose
job is rendering arbitrary publisher EPUBs faithfully;
[docs/PLATFORM.md](docs/PLATFORM.md) makes that argument in full.

[stylo]: https://crates.io/crates/stylo
[cosmic-text]: https://crates.io/crates/cosmic-text
[tiny-skia]: https://crates.io/crates/tiny-skia
[rbook]: https://crates.io/crates/rbook

## Workspace

The **tier** says how much the API is expected to hold still —
[docs/STABILITY.md](docs/STABILITY.md) explains where the lines fall and
why. A shell depends on `chapbook-reader` alone; it re-exports the rest.

| Crate | Role | Tier |
|---|---|---|
| `chapbook-core` | Shared primitives: geometry, page metrics, locators, errors | Contract |
| `chapbook-epub` | EPUB container/package/spine/TOC reading | Producer |
| `chapbook-layout` | Arena DOM + stylo trait bindings (`::dom`), cascade driver (`::cascade`), pagination-first block + inline layout via cosmic-text | Internal |
| `chapbook-paint` | Format-neutral page model + paint-neutral display list | Contract |
| `chapbook-render-tinyskia` | CPU rasterization backend | Backend |
| `chapbook-render-vello` | GPU rasterization backend (vello + wgpu) | Backend |
| `opds-client` | OPDS 1.2/2.0 catalog client, bring-your-own-HTTP (no chapbook dependency) | API |
| `chapbook-opds` | Binds `opds-client` to chapbook: OPDS-PSE streamed comics as `Publication`s | Producer |
| `chapbook-cbz` | CBZ comic-book archive reading | Producer |
| `chapbook-pdf` | PDF reading, rasterized via hayro (pure Rust) | Producer |
| `chapbook-library` | Local bookshelf: metadata, positions, annotations (SQLite) | API |
| `chapbook-reader` | Shared reading session (open/layout/navigate/select/persist) | API |
| `chapbook-viewer` | Minimal reference viewer (winit + softbuffer) | Not a library |
| `chapbook-viewer-gtk` | GTK4 reference viewer (Linux only) | Not a library |
| `chapbook-viewer-win32` | Win32 reference viewer, with a UI Automation text provider (Windows only) | Not a library |
| `tools/chapbook-cli` | Dev/test CLI exercising each pipeline stage | Not a library |
| `chapbook-ffi` | The C ABI for hosts that speak C — iOS, embedders; `include/chapbook.h` | Contract |
| `chapbook-jni` | Android JNI binding, paired with `android/` | Not a library |
| `chapbook-app` | The application layer: what a reading app decides, shared by every front end through `Platform` | API |
| `chapbook-app-gtk` | GTK4 desktop application over `chapbook-app` (Linux only) | Not a library |

## Embedding from another language

`crates/chapbook-ffi` is a hand-written C ABI over `chapbook-reader`, with a
checked-in header. Swift reaches it through C interop, a browser through
`wasm-bindgen` over the same core, and anything embedding the `.so` links it
directly.

```c
#include "chapbook.h"

cb_config *config = cb_config_new(cb_font_source_host());
cb_config_set_library_dir(config, "/path/to/library");

cb_session *session = cb_session_open_path("book.epub", config);  /* consumes config */
if (!session) { /* cb_last_error_message(...) says why */ }

cb_session_set_metrics(session, (cb_metrics){
    .width = 600, .height = 800, .dpi_scale = 2,
    .margin_top = 40, .margin_right = 40, .margin_bottom = 40, .margin_left = 40,
    .rotation = CB_ROTATION_NONE,
});

uint32_t w, h;
cb_session_render_size(session, &w, &h);          /* allocate exactly this */
cb_session_render_into(session, pixels, len, w, h, w * 4);

bool moved;
cb_session_next_page(session, &moved);            /* use `moved`, not the position */
cb_session_close(session);
```

Codes are the contract and strings are not; nothing crosses owned, so there
is no `cb_free_string`; no call unwinds into the caller. Install
`cb_set_log_callback` first — the engine is silent until a host gives it
somewhere to speak. The full rules are in the header and in
[docs/STABILITY.md](docs/STABILITY.md). Link `libchapbook_ffi.a` (iOS, via
an XCFramework) or `libchapbook_ffi.so`.

Android does **not** go through this header: Kotlin reaches Rust over JNI,
which is already a C ABI, so `chapbook-jni` binds `chapbook-reader`
directly rather than stacking a second boundary on the first. See
[docs/STABILITY.md](docs/STABILITY.md).

## Android

`android/` is a Gradle project with an AAR library module and a demo app,
over `chapbook-jni`. It builds, draws a book, opens one from a `content://`
URI with no path and no extension, and passes the conformance harness on a
device. Read [android/README.md](android/README.md) before touching it,
including the prerequisites, which are not obvious.

```sh
export ANDROID_NDK_HOME=$HOME/Android/Sdk/ndk/<version>
./android/build-jni.sh release      # cargo-ndk into jniLibs, then check linkage
cd android && ./gradlew :demo:assembleDebug
```

## iOS

`ios/` is a Swift package, `Chapbook`, over the C header — a library for
building iOS ereader apps — the application built on it (`ios/App`: the
shelf, a catalog browser with background downloads and sign-in, and the
reader with its chrome), plus a small demo app that picks a book, stores
a security-scoped bookmark, and reopens it cold at the page the reader
left. Read [ios/README.md](ios/README.md), including the platform notes.

```sh
./ios/build-xcframework.sh          # Rust staticlibs → Chapbook.xcframework
```

## Getting started

Rust stable (MSRV 1.92; `rust-toolchain.toml` pins the channel). One system
library, and only for the GTK viewer:

```sh
sudo apt install libgtk-4-dev          # Debian/Ubuntu; gtk4-sys wants gtk4.pc
```

Everything else builds from source — SQLite is bundled, fontconfig is a
pure-Rust parser — so skipping `chapbook-viewer-gtk` needs no system
packages at all. Off Linux you skip it whether you meant to or not: `gtk4`
is a target-gated dependency, the crate compiles to a stub `main`, and
`cargo test --workspace` runs on macOS and Windows without GTK or
pkg-config installed. On a Mac that has `brew install gtk4 libadwaita`,
the two GTK crates opt back in behind a `macos` feature, so the GTK shells
can be developed there against the same widgets under GTK's Quartz
backend; the feature is off by default so a stock Mac stays GTK-free.
`chapbook-viewer-win32` is the mirror image and needs
nothing either way: the `windows` crate is metadata and an import library,
not a system package, so the crate is gated to Windows because a viewer
that cannot open a window is worse than one that says so at compile time.

```sh
cargo run -p chapbook-viewer -- <book.epub|comic.cbz|doc.pdf|opds-url>
cargo run -p chapbook-viewer -- --gpu <book.epub>   # vello + wgpu
cargo run -p chapbook-viewer-gtk -- <book.epub>     # GTK4, Linux
cargo run -p chapbook-app-gtk                        # the GTK application, Linux
cargo run -p chapbook-app-gtk --features macos       # …or macOS with Homebrew GTK
cargo run -p chapbook-viewer-win32 -- <book.epub>   # Win32, Windows only
cargo run -p chapbook-cli -- --help                 # the `chapbook` dev CLI
```

The checks CI runs, in order:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets   # CI sets RUSTFLAGS=-D warnings
cargo test --workspace
```

## Status

The core pipeline works end-to-end: open (or download via OPDS) an EPUB,
cascade its styles through stylo, paginate with cosmic-text, render pages
with tiny-skia, and read it in the reference viewer — with the layered
positions above persisted per book (rationale in
[docs/LOCATORS.md](docs/LOCATORS.md), interchange via `chapbook cfi`).
Embedded fonts (including obfuscated ones), images, and
text decorations render, with light/sepia/dark themes (sepia recolors
defaults; dark forces readability), and press-drag text selection that
copies to the clipboard (`c`) or becomes a stored highlight (`h`), kept in
the library as layered locators so it re-anchors like a position.

Comics and PDFs work too: local CBZ archives (with `ComicInfo.xml` for
metadata and bookmarks when present), OPDS-PSE page streams, and PDFs
(rasterized with the pure-Rust hayro engine, with a text layer so selection
works there too, and the document outline as a table of contents). They
read in the same viewers with page-unit positions — their pages load and
decode on a background thread.

Pages rasterize on the CPU with tiny-skia, or on the GPU through vello and
wgpu — the same session and the same display list, a different backend
consuming it. Panels — screens that are asked to change rather than
presented to — are [mezzotint]'s half of the job: a `Panel` trait with a
driver that picks the update class, avoids writing under an in-flight
refresh, and rations the ghosting flash, over e-ink controllers and the
plain Linux framebuffer. chapbook names the kind of change and hands over
the pixels; `cargo run -p chapbook-cli --features fbdev --example show`
reads a book on `/dev/fb0` through the pairing.

[mezzotint]: https://crates.io/crates/mezzotint

Explicitly out of scope:
fixed-layout EPUB, JavaScript/scripted content, inline MathML layout
(block equations render natively from OpenType MATH metrics, with STIX
Two Math embedded; inline math renders via the EPUB altimg/alttext
fallback), vertical writing modes, media overlays, DRM. Floated images and width-bearing asides wrap
text for real; hyphenation is dictionary-based (en-US); remaining niche
gaps: CSS counters in generated content, shrink-to-fit floats, `ex`/`ch`
units resolved by approximation.

## Docs

| | |
|---|---|
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | The design: crate boundaries and why they fall where they do |
| [docs/SHELLS.md](docs/SHELLS.md) | Writing a shell against `Session`: the loop, the loader rule, and the conformance harness |
| [docs/APP.md](docs/APP.md) | The application layer above the session: which decisions are shared, which are the platform's, and how mobile, desktop and e-ink differ |
| [docs/STABILITY.md](docs/STABILITY.md) | Which crates carry semver discipline, which are internals, and why |
| [docs/LOCATORS.md](docs/LOCATORS.md) | Why reading positions are layered and versioned (the spec is `chapbook_core::locator`'s docs) |
| [crates/opds-client/INTEROP.md](crates/opds-client/INTEROP.md) | What the OPDS client must interoperate with, and how it was verified |
| [docs/PLATFORM.md](docs/PLATFORM.md) | The five substitution axes a downstream app builds against, and the state of each seam |
| [CONTRIBUTING.md](CONTRIBUTING.md) | The verification gate, the invariants, and the things that fail quietly |
| [NOTICE](NOTICE) | Third-party licences a binary carries |

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Dependencies are not uniformly either — the CSS engine is MPL-2.0, the
rasterizer BSD-3-Clause, the EPUB parser Apache-2.0 only — so distributing
a **binary** carries terms beyond those two. [NOTICE](NOTICE) says which,
which builds they apply to, and what each obligates; `deny.toml` is the
allow-list CI enforces so the set cannot widen unnoticed.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
