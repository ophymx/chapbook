# chapbook as an ereader platform

Chapbook started as an ereader and turned into the engine an ereader is
built on. This document takes that seriously: what "platform" has to mean
here, and the state of each seam a downstream app builds against.

It is a contract statement, not a journal. How each seam got here — the
spikes, the measurements, the corrections — lives in git; open work lives
in the task backlog. What belongs here is the test the platform must keep
passing and what is true of each axis today.

## The test

A platform is judged by what a downstream app can **substitute without
forking**. Five axes matter:

| Axis | Substitutions that must be cheap |
|---|---|
| Display | CPU raster, GPU, e-ink (greyscale, partial refresh), platform canvas, headless/export |
| Input | Touch, mouse, physical page-turn buttons, keyboard, gestures |
| Host | Rust binary, Android (JNI), iOS (Swift), WASM, embedded Linux |
| Content | EPUB, comics, PDF, remote streams — and formats not yet supported |
| Backend | Local library, remote catalogs, position/annotation sync services |

## Display

`Session::frame()` returns the paint-neutral ops — the documented backend
contract — and `paint_resources()` hands back the font database its glyph
runs name faces in plus the image store its image ops key into, which is
everything a shell needs to rasterize for itself: a GPU backend, a
platform canvas, an e-ink panel, an exporter. A test reproduces `render()`
through that public API and asserts the pixels match, which is what makes
it a seam rather than an accessor; `render()` remains the convenience
path. Two rasterizers implement the contract — tiny-skia on the CPU,
vello on the GPU — and a parity test aligns their output by row and
column ink profiles, the measure that catches a 2px displacement while
tolerating the antialiasing differences that centroid and per-cell
coverage both mistake for movement. A second implementation is what
keeps a seam a contract rather than a data structure.

A frame carries its own provenance. `FrameIntent` names the kind of
change, ordered by how much of the page it disturbs, and
`FrameIntent::update_class` joins it to the vendor-neutral
`UpdateClass` vocabulary; damage accumulates independently of that
ordering, so a highlight does not discard the region a live selection
already named. Changes that can state their region do (selections,
highlights, landed page images); the three that replace the page — a
turn, a unit change, a reflow — report `None`, the whole page, always
correct. Deriving damage by diffing display lists is deliberately not
done: a `GlyphRun` carries advances, not ink bounds, so a diffed rect
could under-cover and leave stale pixels on a panel, the one failure
mode damage must not have. A prefetched unit landing silently is the
other half of the same discipline: `poll_loaded` answers "did the page
on screen change", so background loads spend no refresh at all.

Panel colour and orientation are pipeline policy, so every backend
agrees: `PixelFormat::Grey { levels, dither }` reduces luminance (floored
at two levels, no ceiling — a cap would be a claim about which panels
exist), `DisplayList::dither_regions` scopes error diffusion to where
the page has images so body text is not stippled, and
`PageMetrics::rotation` turns output on its way to the buffer with
`panel_to_page` as its inverse for input. The turn is chapbook-paint's
(`rotate` for the pixels, `panel_rect` for a rect turned the same way);
the reduction is `mezzotint::encode`, and packing into a device's buffer
layout belongs one step lower still, to `Panel::blit`.

**The update seam itself is not chapbook's.** It is [mezzotint], and
that crate's docs are the contract: `UpdateClass` as the vendor-neutral
half of a waveform choice, `RefreshPolicy` for ghosting debt,
`PanelDriver` enforcing the rules that work on a desk and fail on a
device, `RecordingPanel` so all of it asserts on a build machine with no
panel attached, and the backends — a plain Linux framebuffer today, its
packing checked against pixel formats a kernel chose (a QEMU harness plus
read-back selftests on real hardware), including 1bpp packed mono with
kernel-declared polarity. A real e-ink backend cannot be written without
the device — `mxcfb` is one vendor's SoC interface, not a standard — and
it is mezzotint's increment to make, not chapbook's.

What chapbook owes that seam is one call: the pixels, the damage rect,
and which `UpdateClass` the change is.
`tools/chapbook-cli/examples/show.rs` is that pairing written out in
full, and it is the only place in this repo that opens a device.

[mezzotint]: https://crates.io/crates/mezzotint

## Input

`chapbook_core::input` is the model: an `Action` vocabulary, `TapZones`
that read the book's direction so a shell cannot default every platform
to LTR, and a `KeyMap` that knows Kobo and PocketBook page-turn buttons —
applied through `Session::apply`, whose `ActionOutcome` distinguishes
*repaint* from *consumed* because Android's volume slider is what
happens when a shell cannot. Navigation, annotations, in-book search,
and settings (persisted, scoped per-book or default) are session API;
`SHELLS.md` is the contract for driving all of it, enforced by
`chapbook_reader::conformance` and demonstrated by
`chapbook-viewer/examples/minimal.rs`.

## Host

The portability boundary is built and proven from outside the workspace
at every level it names:

- **The C ABI** is `crates/chapbook-ffi` with `include/chapbook.h`
  checked in beside it: hand-written, Contract tier, golden-tested
  against the crate, every entry point wrapped in `catch_unwind`,
  nothing crossing owned. `cb_capabilities()` exists because a header
  cannot say which artifact a host actually loaded.
- **Typed sources**: a path, bytes, a seekable handle, or a catalog URL,
  format sniffed from the bytes; every capability the constructor needs —
  fonts, credentials, transport, the library directory, a cache budget —
  arrives through `SessionConfig`.
- **Lifecycle and power**: `suspend()` closes the database rather than
  merely flushing (an iOS app holding a POSIX lock in a shared container
  when it suspends is killed by the watchdog), plus `set_cache_budget`
  and `release_caches`.
- **Android** is `crates/chapbook-jni` plus the `android/` Gradle pair —
  a direct Rust binding, deliberately not a consumer of the header;
  `docs/STABILITY.md` has that argument, `android/README.md` the
  platform notes.
- **iOS** is the Swift package in `ios/` over the header — the consumer
  that cannot route around it, which is what keeps a C ABI honest.
  `ios/README.md` has the platform notes.
- **Windows** is `crates/chapbook-viewer-win32`, over `chapbook-reader`
  in Rust rather than over the header — a desktop host has no boundary to
  cross, so putting one there would be ceremony. What it is evidence for
  is the *loop*: winit and GTK own the main loop and call into a shell,
  and this one calls `GetMessageW` itself, which is the shape a Windows
  app embedding chapbook as a child HWND under WinUI, WPF or MFC is
  actually in. It is also the first shell to run all three press hit
  tests in the order `SHELLS.md` specifies, and the first to deliver
  `Key::TurnPrev`/`TurnNext`, which arrive on a desktop as the two thumb
  buttons of a mouse. Its `uia` module puts the page behind
  `ITextProvider`, which is the second implementation of the text surface
  against a real assistive stack and the one that proves it is a seam:
  AT-SPI asks for text around an offset and UI Automation hands out a
  range that moves its own endpoints, and the same three accessors answer
  both. CI compiles and tests the workspace on `windows-latest`, which
  nothing did before it: the crate is a stub `main` everywhere else, so
  every line of it is judged there or nowhere.
- **WASM stays a demo, deliberately**: `wasm-bindgen` wraps Rust, not C,
  so a browser build is a sibling exporter over the same shape. The
  EPUB-only profile it forces (`--no-default-features`: no SQLite, no
  loader thread, fonts embedded, opened from bytes) is the same profile
  a stripped e-ink build wants, and is held open by a CI `cargo check`
  for `wasm32-unknown-unknown`.
- **Cross-compilation**: CI checks `chapbook-core` on armv7 and aarch64
  (no linker needed — it is where a device build starts), and everything
  except
  `chapbook-viewer-gtk` cross-builds and links for
  `aarch64-unknown-linux-gnu`; the linked binary needs `libc`, `libm`
  and `libgcc_s` and nothing else. GTK is a packaging exception
  (`gobject-sys` wants a pkg-config sysroot), not a code one.

## Content

The `Publication` trait in core is the format seam, and four producers
stand on it: EPUB, CBZ, PDF, and OPDS-PSE streams — the streamed one is
what the `unit_bytes` blocking contract exists for. Comic units
fabricate a one-page layout around an image fragment, so navigation,
the char map, and position persistence stay one code path; locators
count per-format progression units (chars for reflowable text, pages
for image books). `ARCHITECTURE.md` has the design. The formats beyond
EPUB are features (`cbz`, `pdf`, `opds`, on by default), so a device
build drops what its hardware will never open — the savings are the
TLS and PDF stacks — and CI checks the narrow configurations so they
cannot rot. Formats app builders will ask for and chapbook does not
read: MOBI/AZW3, CBR (blocked on pure-Rust RAR5 extraction, which does
not exist), FB2.

## Backend

The library is SQLite, bundled, and stays SQLite everywhere — replacing
it per-platform would make the storage layer unshareable, which is the
point of having one. The schema is sync-shaped and now sync-bearing:
`updated_at` on positions, soft deletes on the user tables, and a
`book_sync` row per book holding the services it answers to and the
revision each was last told about. Two protocols stand on it.
`opds-client` speaks OPDS Progression 1.0 for positions, behind a
non-default `progression` feature because the spec is an unreleased
draft (INTEROP.md §6); `chapbook-annotations` speaks the W3C Web
Annotation Protocol for marks, which is where the OPDS maintainers
point. `chapbook-sync` is the loop between those two and the library —
it owns its own connection rather than the session's, holds no schedule
because when to sync is the shell's call, and never belongs on a UI
thread.

Identity was the open question and is now answered in one direction,
which is the half worth stating precisely. In both protocols the URL
*is* the publication's identity, while the library keys books by edition
fingerprint; `chapbook_sync::targets_of` closes that gap for a book
downloaded from a catalog, reading the two service links off the entry
and recording them against the book's row. A book that arrives as bare
bytes — sideloaded, or adopted from a descriptor — has no entry and so
no service, and nothing maps a fingerprint back to one. That direction
is still open, and it is a product question before it is a protocol one.

The reach is built in the order the consumers asked for it. `chapbook
lib sync` drives it from the terminal; the desktop application drives it
from a window; and the C ABI carries it — `cb_sync_open` puts the worker
over the host's own transport, `cb_library_set_sync_targets` is where a
host that browsed a catalog records the services it learned, and reports
cross as typed per-book statuses. That boundary has **no credential
surface, deliberately**: a host transport is the host's networking, and
it authorizes its own requests — which also obliges it to write, so the
transport contract there includes the POST/PUT/DELETE half and the
response headers (`ETag`, `Location`) the annotation flows turn on.
Both mobile bindings wrap it — Kotlin's `SyncWorker` over
whatever client the app already has, Swift's over `URLSession` — so each
shell reconciles through the platform's own networking, credentials
attached by the host. kosync remains backlog.

Three conclusions here are load-bearing for every host and worth
restating wherever a shell author looks:

- **Transport belongs to the host.** A bundled networking stack costs an
  iOS app background transfer, system trust and ATS, costs Android
  `WorkManager`, and is unavailable in WASM. So `opds-client` opens no
  sockets: the caller injects a blocking `HttpClient` (`UreqHttp` is the
  default impl behind a feature). A blocking transport cannot be
  background transfer, so a host that owns one does not override the
  transport — it reads the entry's download URL, filename and media
  type (`CB_ENTRY_DOWNLOAD_*`, `Entry::download_request`) and its two
  sync services (`CB_ENTRY_PROGRESSION_URL`,
  `CB_ENTRY_ANNOTATION_CONTAINER`), runs the fetch under its own job
  system with its own credential, and hands the finished file to
  `cb_library_import_file`. Read the sync
  services before the transfer; the feed is gone by the time it lands.
- **Credentials belong to the platform's store.** A
  `chapbook_core::CredentialStore` is injected through `SessionConfig`;
  the value is an opaque `Authorization` header, the key is a stable
  non-secret, lookups never prompt (the loader thread cannot host a
  biometric dialog), and nothing in the library database holds a secret.
- **Custody: bookmarks, not copies.** A path-opened book is imported;
  bytes and descriptors are *adopted* — recorded under an edition
  fingerprint, never copied — so positions and annotations key on the
  book's identity while the file stays the platform's. Holding the way
  back to the file (a security-scoped bookmark, a URI grant) is the
  shell's half, re-resolved on every cold launch before the session is
  constructed.

## The targets the display seam has to survive

Not a roadmap — a set of shapes to check designs against, because each
pulls in a different direction and any two of them agreeing proves
nothing:

| Target | Surface | Pixels | Update model |
|---|---|---|---|
| Kobo Clara, Kindle (i.MX, Carta) | fbdev + `mxcfb` ioctls | 8bpp grey preferred; RGB565 the common default | EPDC waveforms, hardware dither, `quant_bit` per update |
| Kobo colour (Kaleido, MTK) | fbdev + MTK ioctls | 32bpp forced — the driver has no 8bpp | waveforms; colour via a filter array over a mono panel |
| Desktop | swapchain (winit/GTK, tiny-skia or vello) or a plain DIB blit (Win32 `StretchDIBits`) | RGBA | none; present every frame |
| Android | JNI to a `Surface`; Onyx adds `EpdController` | ARGB_8888 | none, or Onyx's own DU/GC/A2/REGAL |
| Pi + Waveshare SPI | SPI transfer plus a BUSY pin | 1bpp packed; some panels 2 or 4 levels | whole-panel or window refresh commands |

Everything below the last column is [mezzotint]'s to survive, and its
docs carry the argument for why `blit`/`submit`/`wait` does: a file
descriptor and an mmap, a JNI call, and an SPI transaction are the same
three operations. The table stays here because it is what chapbook's
*own* half has to hold up under — a session that can be told its pixel
format, that states damage it can stand behind, and that names the kind
of change rather than a waveform. Nothing above the seam varies across
these five rows, and that is the claim worth re-checking when a sixth
one arrives.

## Why not a web view

The obvious iOS substitution is a `WKWebView`, which is what most iOS
readers are. Measured on a synthetic dense-text book, CPU rasterization
is not the constraint — a page turn inside a paginated chapter is
single-digit milliseconds at phone resolutions, work that happens once
per gesture rather than once per frame. The argument that actually
matters is `LayeredLocator`: a web view's notion of where you are is a
function of its own line breaking, which moves under you when the OS
updates, and the quote-context, fraction and progression record cannot
be rebuilt above one — a reader built that way spends its life fighting
it.

The honest counterweight: chapbook's layout is a real subset of what
publishers ship, and a web view gets the long tail — ruby, broken
markup — for free. The pipeline is right *for a controlled-typography
reader*. It is the wrong tool for an app whose job is rendering
arbitrary publisher EPUBs faithfully, and no amount of FFI work changes
that.

## Ceilings to decide deliberately

These are choices, not oversights — but a platform should make them
explicitly rather than by default:

- **Vertical writing** is out of scope. This is the one gap that is
  genuinely *hard* rather than merely unbuilt: it reaches into the layout
  crate, the largest in the workspace. It is a hard ceiling for any CJK
  or manga-oriented app built on chapbook. Decide whether that ceiling is
  acceptable.
- **Bidirectional text works and is now tested** — Hebrew and Arabic
  shape, join, reorder, align, hit-test and select, against a real
  fixture in `crates/chapbook-reader/tests/bidi.rs`. It is listed here
  because it used to sit under the line above, untested, and three
  defects were waiting in it. One limitation remains and is pinned by a
  test: cosmic-text resolves the bidi base level per line from the first
  strong character, so a paragraph declaring `dir="rtl"` whose first word
  is Latin reorders as LTR. It aligns right regardless. Fixing that needs
  an upstream change or a different shaper.
- **Fixed-layout EPUB** stays rejected. Reasonable for one app; a bigger
  deal for a platform, since comics-as-FXL-EPUB is common in the wild
  and the profile in practice is narrow (one full-bleed image per page).
  A special-cased image-book path could accept exactly that profile
  while still rejecting general FXL.
- **Scope discipline.** Velocity goes into breadth — another format,
  another surface — because that work is more fun than hygiene. Breadth
  without the hygiene half (stability policy, conformance harness,
  shell docs, feature flags) produces an excellent personal ereader
  whose code merely looks modular.
