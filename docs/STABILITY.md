# API stability

A workspace this size is a lot of surface to promise nothing about and
far too much to promise everything about. This is the split: which
crates carry semver discipline, which are implementations you may depend
on at your own pace, and which are internals that will move under you.

Nothing here is a promise about **1.0**. The workspace is at `0.1.0` and
under 1.0 a minor bump may break anything — that is what `0.x` means, and
pinning `=0.1` is the honest way to depend on it today. What the tiers
decide is where breakage will be *treated as a cost* once versions start
meaning something, and where it will not be treated as a cost at all.

## The tiers

| Tier | Crates | What it means |
|---|---|---|
| **Contract** | `chapbook-core`, `chapbook-paint`, `chapbook-ffi` | Types that appear in signatures a downstream must name. Breaking one breaks every shell *and* every backend at once. Changed most reluctantly. |
| **API** | `chapbook-reader`, `chapbook-library`, `chapbook-annotations`, `chapbook-sync`, `opds-client`, `chapbook-app` | What a downstream calls. Semver discipline: breaking changes are deliberate, announced in the changelog, and worth the migration. |
| **Producer** | `chapbook-epub`, `chapbook-cbz`, `chapbook-pdf`, `chapbook-opds` | Format readers behind `Publication`. Depend on one only to open that format directly; through `chapbook-reader` they are an implementation detail. |
| **Backend** | `chapbook-render-tinyskia`, `chapbook-render-vello` | Implementations of a Contract-tier trait. The *trait* is stable; the crate implementing it is free to change, because substituting it is the point. |
| **Internal** | `chapbook-layout` | No stability of any kind. It exists to make the engine work, its DOM binding and cascade driver follow stylo's shape rather than a design of their own, and a stylo upgrade rewrites them. |
| **Not a library** | `chapbook-viewer`, `chapbook-viewer-gtk`, `chapbook-viewer-win32`, `tools/chapbook-cli`, `chapbook-jni`, `chapbook-app-gtk` | Their surface is not their Rust API. For the binaries it is a command line; for `chapbook-jni` it is the AAR's Kotlin API, which is why it is here rather than in a tier of its own. The reference shells exist to be read and copied, not linked. |

**`chapbook-app` moved from "Not a library" to API.** It was the desktop
application's model crate, serving one front end in the same repository.
It is now the application layer every front end drives — GTK directly,
iOS through `cb_app_*` in the C ABI, Android through `chapbook-jni` —
and its Rust surface is what the header and the JNI natives are shaped
over, so a change to it is felt in three languages. That is API-tier
blast radius. It is not Contract tier because its types do not appear in
a backend's or a producer's signatures; a downstream that wants only a
session still builds on `chapbook-reader` and never names it.
`docs/APP.md` says what it holds.

## Why the lines fall there

**Contract before API.** The natural first cut is "what does an app call",
which puts `chapbook-reader` at the top. But `reader` can add a method
without disturbing anyone, while a field added to `chapbook_core::Rect`
or a fourth variant of `chapbook_paint::DisplayOp` breaks every
rasterizer at once — including ones outside this repository, which is
the whole premise of `PLATFORM.md`.
Blast radius, not call frequency, is what earns the strictest tier.

That is also why `chapbook-paint` is Contract rather than Internal, even
though nothing outside the workspace has ever imported it: a render
backend cannot exist without naming `DisplayList`, and render backends
are the seam.

**Backends are stable in the direction that matters.** A rasterizer is
named by `DisplayList` and nothing else, so the promise belongs to the
trait and the implementing crate stays free to change — substituting it
is the point.

**The panel seam is somebody else's stability problem now.** A device
port implements `mezzotint::Panel`, and mezzotint versions on its own
schedule. What chapbook owes is only that the three types it re-exports
from there — `UpdateClass`, `PanelRect`, `PixelFormat` — keep coming from
one place; a mezzotint major bump is therefore a Contract-tier change
here, and is taken deliberately for that reason.

**`chapbook-library` is API, not Internal**, because it is not hidden:
`chapbook-reader` re-exports it and its `AnnotationKind` and record types
appear in the session's own signatures. A crate whose types leak through
a stable API is a stable API, whatever its intent was.

**`opds-client` is the one member with a life of its own,** and the only
one whose name does not start with `chapbook`. It is a working OPDS
1.2/2.0 client with no ereader attached — it does not depend on
`chapbook-core` and never will — so it carries the API tier and is
expected to leave this repository for its own eventually. It is also the
only crate here whose *feature* surface is part of the promise: the
`ureq` transport is a default feature, and a caller that turns it off and
supplies its own `HttpClient` must keep working.

**`chapbook-annotations` is API** for the same reason
`chapbook-library` is: a shell calls it directly to sync a reader's marks,
and its `Mark` is built out of `chapbook_library::AnnotationKind` and
`chapbook_core::LayeredLocator`, so its signatures are already made of
another tier's types. Its wire format is not the promise, though — the
Web Annotation Data Model is, and this crate follows it rather than
versioning its own shape. What *is* promised is that a document it does
not fully understand survives a round trip through it unchanged: a
container is shared with other reading apps, and a lossy parse there is
their data loss, not ours.

**`chapbook-sync` is API** because a shell drives it: it decides when to
reconcile, and this is what it calls. The crate deliberately holds no
schedule of its own — every page turn is wrong on a metered radio, on
close is wrong for a session that crashes — so the *timing* is the
shell's and the promise here is only about what the calls do and what
they report.

**`chapbook-opds` is Producer,** now that it is only the binding: an
OPDS-PSE stream presented as a `Publication`, plus the error seam into
`ChapbookError`. It re-exports `opds-client` wholesale, so a consumer
inside the workspace still depends on one crate, but the tier follows
what the crate itself is — a format reader like the other three.

**Internal means internal.** `chapbook-layout` — its `dom` binding, its
`cascade` driver, and layout itself — is shaped by stylo's trait
requirements, not by a design anyone chose, and the pinned stylo set
upgrades all-at-once as a deliberate task that rewrites it. Promising
anything about that surface would be promising something about Servo's.

## What this asks of a change

- **Contract or API tier:** a breaking change needs a reason in the commit
  message, not just a diff. Prefer additive change; prefer a new method to
  a changed signature; prefer a new variant on a `#[non_exhaustive]` enum
  to a changed one.
- **Producer or Backend tier:** break freely, fix the callers in the same
  commit. The workspace builds together, so the compiler finds them.
- **Internal tier:** no ceremony. These follow the engine.
- **Any tier:** if a change moves a type *between* crates, it moves
  between tiers too. `rotate` moving from `chapbook-render-tinyskia` into
  `chapbook-paint` promoted it from Backend to Contract; the panel seam
  leaving for mezzotint took `Panel` and `PanelDriver` out of chapbook's
  tiers altogether, which is a larger version of the same event. Both are
  the correct outcome and worth noticing at the time rather than later.

## Depending on chapbook

A shell depends on **`chapbook-reader` alone**. It re-exports
`chapbook_core`, `chapbook_paint`, `chapbook_library`,
`chapbook_render_tinyskia`, `tiny_skia` and `cosmic_text`, so a shell
cannot skew versions with the engine it drives. `docs/SHELLS.md` §9 says
the same thing from the shell's side.

Reaching past that re-export — naming `chapbook-core` as a direct
dependency, say — is supported and sometimes necessary (a shell driving
a panel names `mezzotint` too, since the driver and the `Panel` trait are
deliberately not re-exported), but it is a version you now have to keep
in step yourself.

Nothing is published to crates.io yet. When it is, the Internal and
"Not a library" tiers get `publish = false` unless there is a reason not
to; until then, this document is the only thing that distinguishes them.

**`chapbook-ffi` is Contract tier for a different reason than the other
two.** `chapbook-core` and `chapbook-paint` are types a downstream *names*;
`chapbook-ffi` is a header a downstream *compiles against*, in a language
with no way to express a version bound. A Kotlin or Swift host does not
resolve this crate — it links a `.so` or a `.a` and trusts the header it was
given. So the usual escape hatch, "breaking changes are deliberate and
announced", is worth less here than anywhere else in the workspace: there is
no `cargo update` to hold back and no compiler error to arrive first, only a
struct whose layout quietly changed underneath a caller.

Concretely, three things in it are permanent and one is not. The `cb_status`
numbers, the `#[repr(C)]` struct layouts and the exported symbol names are
the contract. The strings behind `cb_last_error_message` are not, and say so
in their own documentation — they exist for logs and bug reports, and a host
that branches on their text has written a bug the codes were there to
prevent. Adding functions and adding enum variants at the end are additive
and expected; renumbering, reordering or repurposing anything is not.

`include/chapbook.h` is checked in rather than generated at build time so
that consuming the ABI needs no cbindgen, no build script and no Rust
toolchain — and it is a golden, so the Rust and the header cannot drift
apart without the gate saying so.

**The Spike tier is retired, and the prediction that justified it was
wrong.** It said `chapbook-jni` would, once the C ABI existed, "become a
thin JNI layer over it or go away." It did neither, and the reasoning is
worth keeping because it is the same reasoning any future host binding
should apply.

JNI *is* a C ABI: the JVM finds native methods by exported symbol name with
C linkage, and Rust emits those directly. So a Rust JNI layer over
`chapbook-ffi` would put two C-shaped boundaries back to back with Rust in
the middle converting in both directions — a `CString` allocated per call
purely to satisfy a boundary both sides are on the far side of, and a
length-probe-then-fill dance whose entire purpose is that a *C* caller never
frees Rust memory. Writing the glue in C instead removes the round trip, but
buys only what a four-line `clang -fsyntax-only` already buys, at the price
of a second build system and a third hand-written layer to keep in sync.

So `chapbook-jni` stays what it was: a direct Rust binding over
`chapbook-reader`, exporting JNI symbols. It is no longer throwaway — it is
the native half of the Android artifact — and it is *not* a Contract-tier
consumer of `chapbook-ffi`.

That leaves `chapbook-ffi` with one guaranteed consumer rather than two, and
that is fine: iOS is the one that cannot route around it, because Swift
speaks C and nothing else. A boundary is kept honest by the host with no
alternative, not by the host that was talked into it.
