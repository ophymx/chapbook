# The application layer

`chapbook-reader::Session` keeps everything book-shaped out of a shell.
`crates/chapbook-app` keeps everything *app*-shaped out of one. It sits
between the session and the front end, and it is the layer that had been
written three times — in Rust for the GTK desktop app, in Kotlin for
Android, in Swift for iOS — each time with the same decisions and slightly
different bugs. It is now written once, in Rust, and every front end asks
it instead:

```
SwiftUI / Compose / GTK        widgets, gestures, threads, the main loop
        │
        ▼
chapbook-app  (App, Catalog)   what the app decides       ← this document
        │
        ▼
chapbook-reader (Session)      what a book is
chapbook-library, chapbook-sync, chapbook-opds
```

The desktop reaches it directly as a Rust crate. iOS reaches it through the
C ABI (`cb_app_*` in `include/chapbook.h`, wrapped by `Chapbook.App` in the
Swift package). Android reaches it over JNI (`com.ophymx.chapbook.App` in
the AAR). All three drive the same code; the Rust tests under
`crates/chapbook-app/tests` are the tests of the behaviour, and the Kotlin
and Swift model tests are left to check the platform half and the thread
discipline.

## The question that draws the line

Every decision in a reading app was sorted with one question: **could two
apps on the same device reasonably answer this differently?**

- If not — a shelf opens on the book the reader was in; a credential is
  keyed by origin and never by a catalog URL; a 401 is a login, not a
  failure; a download is one job per entry; a position is saved before
  the screen goes; a memory warning halves the page cache — it is
  **policy**. It lives in `chapbook-app` and is the same on every device.
- If the answer depends on the operating system — how a file grant is
  expressed, which store keeps a secret, which HTTP stack honours the
  device's trust store, how much memory a process may take, when the
  process is about to be killed — it is a **platform capability**. It
  arrives through the `Platform` record a front end hands in, or as a
  plain number the policy is parameterised by.

The old desktop crate assumed a desktop in exactly four places, which is
why the mobile apps could not use it: host fonts, credentials from the
environment, a bundled TLS stack, and a shelf that only imports. Those
four are now the `Platform`:

| Field | What it is | Desktop | Phone | E-ink |
|---|---|---|---|---|
| `fonts` | Where faces come from | host font database | embedded directory in the bundle | embedded directory |
| `credentials` | The `CredentialStore` a sign-in lands in | env / keyring / none | Keychain, Keystore | file under the library dir, or none |
| `transport` | The `HttpClient` fetches go through | bundled `ureq` (`bundled-http` feature) | `URLSession`, OkHttp | bundled `ureq`, or the device's stack |
| `device_name` | What a progression service shows beside this device | hostname | "iPhone", model name | model name |

`App::desktop()` fills all four the old way, so the GTK app and the CLI
did not change shape. A phone fills them with its own answers and the
crate assumes nothing about where it runs.

## What the layer decides

| Area | `App` / `Catalog` surface | The decision |
|---|---|---|
| Shelf | `shelf(filter)`, `series`, `book`, `remove`, `set_finished` | Recently-read first by default; series grouped; states are the library's |
| Custody | `import`, `adopt`, `adopt_open`, `grant`, `remember_grant`, `forget_grant`, `open_book` | Import copies and the library owns the file; adopt records by content, keeps no copy, and stores the platform's grant (opaque bytes) under the fingerprint. `open_book` answers `Session`, `Adopted { grant }` or `Missing` — the front end resolves a grant, since only it knows what its grants are |
| Session config | `session_config` | Fonts, library, cache budget and everything else a session is opened with, identical for every door a book comes in through |
| Place | `Place::of`, `Readout`, `ProgressLabel` | Spine-weighted book fraction, pages left, chapter page; which readout is shown is a preference the layer keeps |
| Memory | `cache_budget_for(device_bytes)`, `after_memory_warning` | A quarter of the device, clamped to 16–192 MiB; a warning halves it with a 4 MiB floor and releases pages now |
| Search | `SearchWalk` | Walks the book one spine unit per step so a screen can stay responsive, caps hits at 200, `show_hit` selects and turns to the page |
| Selection | `highlight_selection`, `note_on_selection` | Which annotation a gesture makes and how it is coloured |
| Catalogs | `catalogs`, `add_catalog`, `rename_catalog`, `remove_catalog`, `browse` | Saved beside the shelf, whitespace trimmed, credential never stored with the URL |
| Browsing | `Catalog::{fetch, go, back, apply_facet, search, load_more, sign_in}`, `BrowseState` | A navigation row pushes a crumb; Back walks crumbs before it leaves; a facet replaces, a page appends; a 401 is `Login`, a sign-in stores by origin and fetches again |
| Downloads | `Download`, `DownloadOutcome::of_status`, `land_download` | The request carries everything the landing needs except the credential; 2xx lands, 401/403 is refused, 5xx or no answer is `Again`, the rest are gone; landing imports and records the progression and annotation endpoints for sync |
| Sync | `sync_all`, `sync_book`, `sync_events`, `describe_sync` | Which books sync where, how the device is identified, what an event means |
| Credentials | `CredentialKey::http_origin`, `basic_authorization` | The key drops the path (a per-user API key may live there) and strips `user:pass@`; Basic is encoded once |
| Preferences | `progress_label`, `set_progress_label` | Shell display preferences, kept beside the shelf so every front end on the device agrees |

What it deliberately does not hold: words (it answers in enums and
numbers; the `describe_*` functions are the desktop's English), threads
(an `App` and a `Catalog` are each one thread's at a time, like a session;
the front end picks the thread), display and input (those are the
session's contract, `SHELLS.md`), and the main loop.

## How much of it is mobile-specific

Almost none of the policy. What was mobile-specific in the Kotlin and
Swift copies turned out to be the platform half — and that half is
*smaller* than it looked, because each copy had also re-implemented the
policy around it. The matrix, feature by feature:

| Feature | Policy (shared) | Desktop | Phone | E-ink |
|---|---|---|---|---|
| A picked file | adopt vs import, the fingerprint, the grant map | a grant is the path; the desktop imports by default and may adopt | a grant is a SAF URI or a security-scoped bookmark | a grant is the path on the device's storage; adopt is the natural door, since books arrive by USB or card |
| A file another app sends | import while the bytes are ours | n/a | `VIEW` intent, share sheet `Inbox` | n/a, or a watched folder |
| Opening a book | `open_book`, `session_config` | direct | resolve the grant to a descriptor, then `adopt_open` / open by descriptor | direct, by path |
| Page cache size | `cache_budget_for` | ample; the ceiling applies | a quarter of the phone; the ceiling applies | a quarter of very little; the **floor** is what matters |
| Memory pressure | `after_memory_warning` | never called | `didReceiveMemoryWarning`, `onTrimMemory` | usually never called; the small budget stands in for it |
| Position saved | before the screen goes, exactly once | window close | view disappears / background | power-off, sleep, page-turn button held |
| Readout | `Place`, `ProgressLabel` | percent | percent | pages left is the sensible default; the preference key is shared |
| Search | `SearchWalk` steps | one step per idle tick | one step per task hop | one step per refresh budget |
| Catalog browsing | all of it | ureq | `URLSession` / OkHttp | ureq or the device's stack |
| Sign-in | key by origin, store through the platform | env / keyring | Keychain / Keystore | a file store; the device has no secure enclave to speak of |
| Download | `Download` + `DownloadOutcome` + `land_download` | a thread | a background transfer session | a thread; `Again` matters more because the wifi drops |
| Sync | all of it | on demand | on demand, background fetch | on wake, on the sync button; must finish before the device sleeps |
| Progress label preference | kept beside the shelf | the same | the same | the same |

So the answer to "how much is mobile-specific" is: the four `Platform`
fields, the grant resolution step in `open_book` (which is the front
end's on every platform, just a different kind of grant), and the two
memory hooks — and a desktop implements the hooks by not calling them.
Everything above the line is shared, and the mobile apps did not have a
single policy that the desktop did not also need.

## Does it apply to e-ink

Yes, and with less to adapt than a phone. An e-ink reader is a
single-purpose Linux device: files arrive by path, there is no
sandbox to bookmark, no keychain, no background-transfer service and no
memory-warning signal. That makes its `Platform` closer to the desktop's
than to a phone's — embedded fonts, a file-backed or absent credential
store, the bundled transport, a model name — and every policy row above
holds as written. Three things are worth knowing:

- **The floor, not the ceiling.** `cache_budget_for` is written for the
  small end: a 512 MiB device asks for 128 MiB and gets it, a 256 MiB
  device gets 64 MiB, and the 16 MiB floor is the promise that the
  cache never disappears entirely. `after_memory_warning` exists for a
  device that can deliver one (a cgroup notification, a low-memory
  killer's warning), and costs nothing where none comes.
- **Pages, not percent.** `ProgressLabel::PagesLeft` and
  `ChapterPage` are the readouts an e-ink reader shows, and `Place`
  supplies them from the layout rather than from a fraction, so a
  device's front end chooses a default and the preference is kept
  where the shelf is.
- **What this layer does not cover for e-ink is display and power**,
  and those are not app policy: a greyscale panel with partial refresh
  is the Display axis in `PLATFORM.md` (and the `frame()` contract in
  `SHELLS.md`), page-turn buttons are the Input axis, and when to sync
  before sleep is the device's scheduler. The application layer is
  ready for an e-ink front end; the e-ink work that remains is below it,
  in the render backend, not beside it.

## Threads

An `App` holds a library connection and is one thread's at a time. A
front end keeps it on whichever thread it keeps the shelf on — the GTK
main loop, a Kotlin single-thread dispatcher, a Swift actor on a serial
queue — and opens a second handle for a second thread rather than sharing
one: the library is SQLite in WAL mode, and two connections on one
directory are the supported shape. The iOS app keeps one handle on the
main actor for the synchronous questions a screen asks (saved catalogs, a
preference), one in the shelf's actor for custody and landing, and one
per catalog session. A `Catalog` is the same: it belongs to the thread
that does its blocking fetches.

The threads the application needs beyond that — the session's loader, the
sync worker — are the engine crates' own and reach back only through
wakers. Nothing in `chapbook-app` spawns, because every platform has a
better answer to "run this off the main thread" than a library could
invent.

## Where each front end binds

| Front end | Binding | Platform half kept in the front end |
|---|---|---|
| `chapbook-app-gtk` (Rust) | `chapbook_app::App::desktop` | widgets; nothing else |
| Android (`android/app`) | `com.ophymx.chapbook.App` in the AAR, over `chapbook-jni` | `Grants` (URIs as bytes), `Credentials` (Keystore-backed `CredentialStore`), OkHttp transport, `Opener` resolving a URI to a descriptor, `Downloads` over `DownloadManager`, the single-thread dispatcher |
| iOS (`ios/App`) | `Chapbook.App` in the Swift package, over `cb_app_*` | `Grants` (bookmarks as bytes), `Credentials` (Keychain-backed `CredentialStore`), `URLSession` transport, `Opener` resolving a bookmark to a descriptor, `Downloads` over a background session, the actors |

The model layers in Kotlin and Swift still exist, and they are still the
right place for a screen to look; what changed is that they delegate
every decision and keep only the platform half and the isolation. A new
front end — an e-ink shell, a WASM one — writes that half and nothing
more.

## Features

`chapbook-app` builds without a network stack: `catalog` adds browsing,
downloads and sync over a transport the platform supplies;
`bundled-http` adds `ureq` so a desktop needs none; `desktop` is the old
default set. `chapbook-ffi` exposes the layer under its `app` feature
(on by default, and pulled in by `opds`), and a host probes for it with
`CB_CAP_APP`. `chapbook-jni` always has it.
