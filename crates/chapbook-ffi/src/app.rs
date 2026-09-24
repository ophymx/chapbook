//! The application layer, crossing C: `cb_app_*`, `cb_reader_*`, the
//! search walk, and the browse verbs on a catalog.
//!
//! Everything below [`cb_session`] crossed this boundary already — a book,
//! a shelf, a catalog, a sync worker — and every host then wrote the same
//! application over them: which door a file comes in through and how the
//! book is found again, where the reader is as one number, how much
//! memory a page cache may take, how a search walks a book without
//! blocking a draw, what a catalog's 401 turns into, what a landed
//! download does. Two phones wrote it twice. `chapbook-app` is that
//! application written once, and this module is it crossing to the hosts
//! that are not Rust.
//!
//! # What crosses and what does not
//!
//! **The decisions cross; the platform stays home.** A host still owns
//! its grants (a content URI, a bookmark), its secret store, its HTTP
//! stack, its job system and its memory warnings. Each of those reaches
//! the layer as a capability — a credential store installed with
//! [`cb_config_set_credential_store`], a transport with
//! [`cb_config_set_http_transport_full`](crate::cb_config_set_http_transport_full),
//! a grant as opaque bytes, a memory figure as a number — and the layer
//! answers with what to do.
//!
//! **Answers are enums and numbers, never sentences.** A
//! [`cb_browse_state`], a [`cb_place`], a [`cb_download_outcome`]. The
//! host has the strings table.
//!
//! **Threads are the host's.** A [`cb_app`] is one thread's at a time,
//! like the library connection it holds; a catalog likewise, on whichever
//! thread does the blocking fetches. Nothing here spawns for a host.
//!
//! # The credential store
//!
//! The one capability that was missing from this ABI. Sync said the
//! transport does its own authorization, and for a transport that is
//! true — but the *application* needs to store a sign-in by origin and
//! read it back before a fetch, and both phones had written that in
//! their own language over their own keystore. So the store crosses the
//! way the transport does: three callbacks and a finalizer, the value an
//! opaque `Authorization` header, the key a string the engine derives
//! ([`cb_credential_key`]) and the host never parses.

use std::ffi::{c_char, c_void};

use crate::config::cb_config;
use crate::error::{cb_status, fail, guard};
use crate::navigation::cb_search_hit;
use crate::session::{cb_session, cb_wake_fn};
use crate::sync::cb_sync_report;

#[cfg(feature = "app")]
use crate::abi::{slice_out, str_in, str_out};
#[cfg(feature = "app")]
use crate::error::{clear_last_error, from_error};

// ---- The credential store ----

/// The answer to a credential lookup, under construction. Opaque; the
/// engine hands one to the get callback and it is dead when the callback
/// returns. Saying nothing means *missing* — prompt.
#[derive(Default)]
pub struct cb_credential_response {
    found: Option<String>,
    locked: bool,
    failed: Option<String>,
}

/// Looks up the credential stored under `key`.
///
/// Before returning, call [`cb_credential_response_found`] with the
/// stored `Authorization` value, [`cb_credential_response_locked`] if
/// something is stored but cannot be read right now (device locked,
/// biometrics not satisfied — do not prompt for a new one), or nothing at
/// all for *missing*. **It may fire on any thread** and must not put up
/// a dialog: a lookup is a lookup.
pub type cb_credential_get_fn = Option<
    unsafe extern "C" fn(
        key: *const c_char,
        response: *mut cb_credential_response,
        user: *mut c_void,
    ),
>;

/// Persists `authorization` under `key`, replacing what was there. Return
/// `CB_OK`, or any failure code; the layer reports it and carries on
/// signed in for this session.
pub type cb_credential_store_fn = Option<
    unsafe extern "C" fn(
        key: *const c_char,
        authorization: *const c_char,
        user: *mut c_void,
    ) -> cb_status,
>;

/// Forgets whatever is under `key`. Nothing there is success.
pub type cb_credential_forget_fn =
    Option<unsafe extern "C" fn(key: *const c_char, user: *mut c_void) -> cb_status>;

/// The stored value, verbatim: `Basic dXNlcjpwdw==`, `Bearer eyJ...`.
#[no_mangle]
pub unsafe extern "C" fn cb_credential_response_found(
    response: *mut cb_credential_response,
    authorization: *const c_char,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        // SAFETY: the pointer the callback was handed, within the call.
        let Some(response) = (unsafe { response.as_mut() }) else {
            return fail(cb_status::CB_ERR_NULL_ARGUMENT, "response is null");
        };
        // SAFETY: the header's contract.
        let Some(authorization) = (unsafe { crate::abi::str_in(authorization, "authorization") })
        else {
            return cb_status::CB_ERR_NULL_ARGUMENT;
        };
        response.found = Some(authorization.to_string());
        cb_status::CB_OK
    })
}

/// Something is stored and cannot be read right now.
#[no_mangle]
pub unsafe extern "C" fn cb_credential_response_locked(
    response: *mut cb_credential_response,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        // SAFETY: the pointer the callback was handed, within the call.
        let Some(response) = (unsafe { response.as_mut() }) else {
            return fail(cb_status::CB_ERR_NULL_ARGUMENT, "response is null");
        };
        response.locked = true;
        cb_status::CB_OK
    })
}

/// The store itself failed. `message` is for a log, never for a reader.
#[no_mangle]
pub unsafe extern "C" fn cb_credential_response_fail(
    response: *mut cb_credential_response,
    message: *const c_char,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        // SAFETY: the pointer the callback was handed, within the call.
        let Some(response) = (unsafe { response.as_mut() }) else {
            return fail(cb_status::CB_ERR_NULL_ARGUMENT, "response is null");
        };
        // SAFETY: the header's contract.
        let Some(message) = (unsafe { crate::abi::str_in(message, "message") }) else {
            return cb_status::CB_ERR_NULL_ARGUMENT;
        };
        response.failed = Some(message.to_string());
        cb_status::CB_OK
    })
}

/// Keep secrets in the host's store instead of nowhere.
///
/// `get` is required; `store` and `forget` may be null for a read-only
/// store, and a sign-in then lasts the session. `user` is handed to every
/// callback and released by `finalize` exactly once — when the config is
/// freed unopened, when the last session or app holding the store goes,
/// or before this call returns a failure. The callbacks may fire on any
/// thread, the loader's included.
///
/// The key is a string the engine derives and the host stores under
/// verbatim — `kSecAttrService`, a Keystore alias, a preferences key. Its
/// shape is [`cb_credential_key`]'s and never a URL: a catalog URL's
/// path can itself be a secret.
#[no_mangle]
pub unsafe extern "C" fn cb_config_set_credential_store(
    config: *mut cb_config,
    get: cb_credential_get_fn,
    store: cb_credential_store_fn,
    forget: cb_credential_forget_fn,
    finalize: crate::http::cb_http_finalize_fn,
    user: *mut c_void,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        let decline = |code, message| {
            if let Some(finalize) = finalize {
                // SAFETY: the host's own finalizer with the host's own
                // pointer, called exactly once.
                unsafe { finalize(user) };
            }
            fail(code, message)
        };
        // SAFETY: a handle from `cb_config_new`, not yet consumed.
        let Some(config) = (unsafe { config.as_mut() }) else {
            return decline(cb_status::CB_ERR_NULL_ARGUMENT, "config is null");
        };
        let Some(get) = get else {
            return decline(
                cb_status::CB_ERR_NULL_ARGUMENT,
                "get callback is null; a store that cannot answer is not one",
            );
        };
        config.inner.credentials = std::sync::Arc::new(host::HostCredentials {
            get,
            store,
            forget,
            finalize,
            user: user as usize,
        });
        cb_status::CB_OK
    })
}

/// The `CredentialStore` the callbacks become.
mod host {
    use std::ffi::{c_void, CString};

    use chapbook_reader::chapbook_core::{
        ChapbookError, Credential, CredentialKey, CredentialLookup, CredentialStore, Freshness,
    };

    use super::{cb_credential_forget_fn, cb_credential_response, cb_credential_store_fn};
    use crate::error::cb_status;
    use crate::http::cb_http_finalize_fn;

    pub(super) struct HostCredentials {
        pub get:
            unsafe extern "C" fn(*const std::ffi::c_char, *mut cb_credential_response, *mut c_void),
        pub store: cb_credential_store_fn,
        pub forget: cb_credential_forget_fn,
        pub finalize: cb_http_finalize_fn,
        /// The host's pointer as an address, as the transport keeps its
        /// own: the ABI contract carries the thread-safety requirement.
        pub user: usize,
    }

    impl HostCredentials {
        fn user(&self) -> *mut c_void {
            self.user as *mut c_void
        }
    }

    impl Drop for HostCredentials {
        fn drop(&mut self) {
            if let Some(finalize) = self.finalize {
                // SAFETY: the host's own finalizer with the host's own
                // pointer, called exactly once — nothing else ever calls it.
                unsafe { finalize(self.user()) };
            }
        }
    }

    impl CredentialStore for HostCredentials {
        fn get(&self, key: &CredentialKey, _freshness: Freshness) -> CredentialLookup {
            let Ok(key) = CString::new(key.as_str()) else {
                return CredentialLookup::Failed("key contains an interior NUL".into());
            };
            let mut response = cb_credential_response::default();
            // SAFETY: the callback the host installed, with pointers valid
            // for exactly this call.
            unsafe { (self.get)(key.as_ptr(), &mut response, self.user()) };
            if let Some(message) = response.failed {
                return CredentialLookup::Failed(message);
            }
            if response.locked {
                return CredentialLookup::Locked;
            }
            match response.found {
                Some(authorization) => CredentialLookup::Found(Credential::new(authorization)),
                None => CredentialLookup::Missing,
            }
        }

        fn store(&self, key: &CredentialKey, credential: &Credential) -> Result<(), ChapbookError> {
            let Some(store) = self.store else {
                return Err(ChapbookError::Credential(
                    "the host credential store is read-only".into(),
                ));
            };
            let no_nul =
                |what: &str| ChapbookError::Credential(format!("{what} contains an interior NUL"));
            let key = CString::new(key.as_str()).map_err(|_| no_nul("the key"))?;
            let value = CString::new(credential.authorization.as_str())
                .map_err(|_| no_nul("the credential"))?;
            // SAFETY: as above.
            let status = unsafe { store(key.as_ptr(), value.as_ptr(), self.user()) };
            if status == cb_status::CB_OK {
                Ok(())
            } else {
                Err(ChapbookError::Credential(format!(
                    "the host credential store refused ({})",
                    status as i32
                )))
            }
        }

        fn forget(&self, key: &CredentialKey) -> Result<(), ChapbookError> {
            let Some(forget) = self.forget else {
                return Err(ChapbookError::Credential(
                    "the host credential store is read-only".into(),
                ));
            };
            let key = CString::new(key.as_str()).map_err(|_| {
                ChapbookError::Credential("the key contains an interior NUL".into())
            })?;
            // SAFETY: as above.
            let status = unsafe { forget(key.as_ptr(), self.user()) };
            if status == cb_status::CB_OK {
                Ok(())
            } else {
                Err(ChapbookError::Credential(format!(
                    "the host credential store refused ({})",
                    status as i32
                )))
            }
        }
    }
}

/// The key a credential for `url` lives under: scheme, host and any
/// explicit port, lower-cased, the path deliberately discarded. What a
/// host's own code — a cover loader attaching a header, a download job
/// reading back a token — asks its store for, so it agrees with what the
/// layer stored. `CB_ERR_UNAVAILABLE` for anything that is not
/// `scheme://host`.
#[no_mangle]
pub unsafe extern "C" fn cb_credential_key(
    url: *const c_char,
    buf: *mut c_char,
    cap: usize,
    needed: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        use chapbook_reader::chapbook_core::CredentialKey;
        crate::error::clear_last_error();
        // SAFETY: the header's contract.
        let Some(url) = (unsafe { crate::abi::str_in(url, "url") }) else {
            return cb_status::CB_ERR_NULL_ARGUMENT;
        };
        let Some(key) = CredentialKey::http_origin(url) else {
            return fail(cb_status::CB_ERR_UNAVAILABLE, "not a URL with an origin");
        };
        // SAFETY: the header's contract for the buffer triple.
        unsafe { crate::abi::str_out(key.as_str(), buf, cap, needed) }
    })
}

/// The `Authorization` value for HTTP Basic, from a username and
/// password — the engine's chore, so no host guesses at the encoding.
#[no_mangle]
pub unsafe extern "C" fn cb_basic_authorization(
    username: *const c_char,
    password: *const c_char,
    buf: *mut c_char,
    cap: usize,
    needed: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        crate::error::clear_last_error();
        // SAFETY: the header's contract for both.
        let (Some(username), Some(password)) = (
            unsafe { crate::abi::str_in(username, "username") },
            unsafe { crate::abi::str_in(password, "password") },
        ) else {
            return cb_status::CB_ERR_NULL_ARGUMENT;
        };
        let value = chapbook_reader::chapbook_core::basic_authorization(username, password);
        // SAFETY: the header's contract for the buffer triple.
        unsafe { crate::abi::str_out(&value, buf, cap, needed) }
    })
}

// ---- The app ----

/// One running application over one library directory. Opaque.
///
/// Holds a library connection, so it is one thread's at a time and may
/// move between them; a host keeps it on the thread it keeps the shelf
/// on. Coexists with any number of sessions, a `cb_library` and a
/// catalog over the same directory — the database is WAL.
pub struct cb_app {
    #[cfg(feature = "app")]
    inner: chapbook_app::App,
    /// Sync reports drained from the driver and not yet handed out, and
    /// the strings the last-returned one borrows.
    #[cfg(feature = "opds")]
    pending: std::collections::VecDeque<chapbook_app::SyncStatus>,
    #[cfg(feature = "opds")]
    strings: Vec<std::ffi::CString>,
}

macro_rules! with_app {
    (($($unused:ident),* $(,)?) $body:block) => {{
        #[cfg(feature = "app")]
        $body
        #[cfg(not(feature = "app"))]
        {
            $(let _ = $unused;)*
            fail(
                cb_status::CB_ERR_FORMAT_NOT_BUILT,
                "this build has no application layer",
            )
        }
    }};
}

#[cfg(feature = "app")]
macro_rules! app_ref {
    ($app:expr) => {
        // SAFETY: a handle from `cb_app_open`, not yet closed.
        match unsafe { $app.as_ref() } {
            Some(app) => app,
            None => return fail(cb_status::CB_ERR_NULL_ARGUMENT, "app is null"),
        }
    };
}

#[cfg(feature = "app")]
macro_rules! app_mut {
    ($app:expr) => {
        // SAFETY: a handle from `cb_app_open`, not yet closed.
        match unsafe { $app.as_mut() } {
            Some(app) => app,
            None => return fail(cb_status::CB_ERR_NULL_ARGUMENT, "app is null"),
        }
    };
}

#[cfg(feature = "app")]
macro_rules! out {
    ($ptr:expr, $value:expr, $what:literal) => {{
        if $ptr.is_null() {
            return fail(
                cb_status::CB_ERR_NULL_ARGUMENT,
                concat!($what, " out-pointer is null"),
            );
        }
        // SAFETY: checked non-null just above.
        unsafe { *$ptr = $value };
    }};
}

/// Open the application. **Consumes `config`** either way.
///
/// The config is the platform: its fonts, its library directory (which
/// is required here — an app has to persist somewhere), the credential
/// store from [`cb_config_set_credential_store`] and the transport from
/// [`cb_config_set_http_transport_full`](crate::cb_config_set_http_transport_full).
/// Every session the app opens, and every config
/// [`cb_app_session_config`] hands back, is built from the same answers,
/// so a host configures its platform once. `device_name` is what a
/// progression service shows beside this device's position.
///
/// In a build without the application layer (`cb_capabilities()` lacks
/// `CB_CAP_APP`) this reports `CB_ERR_FORMAT_NOT_BUILT`.
#[no_mangle]
pub unsafe extern "C" fn cb_app_open(
    config: *mut cb_config,
    device_name: *const c_char,
    out: *mut *mut cb_app,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        if config.is_null() {
            return fail(cb_status::CB_ERR_NULL_ARGUMENT, "config is null");
        }
        // SAFETY: a handle from `cb_config_new`, consumed exactly here —
        // on every path, which the header states.
        let config: Box<cb_config> = unsafe { Box::from_raw(config) };
        with_app!((config, device_name, out) {
            clear_last_error();
            // SAFETY: the header's contract.
            let Some(name) = (unsafe { str_in(device_name, "device_name") }) else {
                return cb_status::CB_ERR_NULL_ARGUMENT;
            };
            if out.is_null() {
                return fail(cb_status::CB_ERR_NULL_ARGUMENT, "out is null");
            }
            let inner = config.inner;
            let Some(dir) = inner.library_dir else {
                return fail(
                    cb_status::CB_ERR_INVALID_ARGUMENT,
                    "the app needs a library directory; set one with cb_config_set_library_dir",
                );
            };
            let platform = chapbook_app::Platform::new(inner.fonts)
                .with_credentials(inner.credentials)
                .with_device_name(name);
            #[cfg(feature = "opds")]
            let platform = match inner.transport {
                Some(transport) => platform.with_transport(transport),
                None => platform,
            };
            match chapbook_app::App::open(&dir, platform) {
                Ok(app) => {
                    let handle = Box::new(cb_app {
                        inner: app,
                        #[cfg(feature = "opds")]
                        pending: std::collections::VecDeque::new(),
                        #[cfg(feature = "opds")]
                        strings: Vec::new(),
                    });
                    // SAFETY: checked non-null above.
                    unsafe { *out = Box::into_raw(handle) };
                    cb_status::CB_OK
                }
                Err(e) => from_error(&e),
            }
        })
    })
}

/// Close the application. Accepts null. Joins the sync driver if one was
/// started, so a returned close means nothing is still writing to the
/// library.
#[no_mangle]
pub unsafe extern "C" fn cb_app_close(app: *mut cb_app) {
    guard((), || {
        if !app.is_null() {
            // SAFETY: a handle from `cb_app_open`, freed once.
            drop(unsafe { Box::from_raw(app) });
        }
    })
}

/// The configuration every session in this app opens with: the platform
/// the app was opened on, and the app's library directory. A fresh
/// handle each call, the caller's to consume with a `cb_session_open_*`
/// — after adding a cache budget, which is the one thing the app leaves
/// to the host (see [`cb_reader_cache_budget_for`]). Null in a build
/// without the application layer.
#[no_mangle]
pub unsafe extern "C" fn cb_app_session_config(app: *const cb_app) -> *mut cb_config {
    guard(std::ptr::null_mut(), || {
        #[cfg(feature = "app")]
        {
            // SAFETY: a handle from `cb_app_open`, not yet closed.
            let Some(app) = (unsafe { app.as_ref() }) else {
                fail(cb_status::CB_ERR_NULL_ARGUMENT, "app is null");
                return std::ptr::null_mut();
            };
            Box::into_raw(Box::new(cb_config {
                inner: app.inner.session_config(),
            }))
        }
        #[cfg(not(feature = "app"))]
        {
            let _ = app;
            fail(
                cb_status::CB_ERR_FORMAT_NOT_BUILT,
                "this build has no application layer",
            );
            std::ptr::null_mut()
        }
    })
}

// ---- Custody ----
//
// Two doors in, one door out. Which door a file takes is the host's
// call — only the host knows whether its grant to a file persists — and
// what each door does is the layer's.

/// Import: copy a file into the library and answer with its row. The
/// door for a file the app will never see again — a share, a viewer
/// intent, a desktop's file dialog.
#[no_mangle]
pub unsafe extern "C" fn cb_app_import(
    app: *mut cb_app,
    path: *const c_char,
    book: *mut i64,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_app!((app, path, book) {
            clear_last_error();
            let app = app_mut!(app);
            // SAFETY: the header's contract.
            let Some(path) = (unsafe { str_in(path, "path") }) else {
                return cb_status::CB_ERR_NULL_ARGUMENT;
            };
            match app.inner.import(std::path::Path::new(path)) {
                Ok(record) => {
                    out!(book, record.id.0, "book");
                    cb_status::CB_OK
                }
                Err(e) => from_error(&e),
            }
        })
    })
}

/// Adopt: record a book the platform owns, from a descriptor, and
/// remember `grant` as the way to reach it again — a persisted content
/// URI, a security-scoped bookmark, whatever the host's bytes are. The
/// library keeps no copy. **Takes ownership of `fd`.**
///
/// Opening is what records the book: a session is opened with the app's
/// configuration, asked which row it became, and dropped, saving
/// nothing. The same bytes adopted twice are one row. Unix only, like
/// [`cb_session_open_fd`](crate::cb_session_open_fd).
#[cfg(unix)]
#[no_mangle]
pub unsafe extern "C" fn cb_app_adopt_fd(
    app: *mut cb_app,
    fd: i32,
    format: crate::session::cb_format,
    grant: *const u8,
    grant_len: usize,
    book: *mut i64,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        if fd < 0 {
            return fail(cb_status::CB_ERR_INVALID_ARGUMENT, "fd is negative");
        }
        // SAFETY: the header's contract — an owned descriptor the caller
        // has given up, which this `File` closes on drop. Taken before
        // any decline so ownership is unconditional, as the header says.
        let file = unsafe {
            use std::os::fd::FromRawFd;
            std::fs::File::from_raw_fd(fd)
        };
        with_app!((app, file, format, grant, grant_len, book) {
            use chapbook_reader::chapbook_core::Source;
            clear_last_error();
            let app = app_mut!(app);
            let Some(grant) = (unsafe { bytes_in(grant, grant_len, "grant") }) else {
                return cb_status::CB_ERR_NULL_ARGUMENT;
            };
            let source = Source::Reader {
                format: format.into(),
                reader: Box::new(file),
            };
            match app.inner.adopt(source, grant) {
                Ok(id) => {
                    out!(book, id.0, "book");
                    cb_status::CB_OK
                }
                Err(e) => from_error(&e),
            }
        })
    })
}

/// [`cb_app_adopt_fd`] for a session the host already opened over a
/// descriptor with [`cb_app_session_config`]: remember `grant` under the
/// book it reached. `CB_ERR_UNAVAILABLE` for a session that reached no
/// shelf.
#[no_mangle]
pub unsafe extern "C" fn cb_app_adopt(
    app: *mut cb_app,
    session: *const cb_session,
    grant: *const u8,
    grant_len: usize,
    book: *mut i64,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_app!((app, session, grant, grant_len, book) {
            clear_last_error();
            let app = app_mut!(app);
            // SAFETY: a handle from an open call, not yet closed.
            let Some(session) = (unsafe { session.as_ref() }) else {
                return fail(cb_status::CB_ERR_NULL_ARGUMENT, "session is null");
            };
            let Some(grant) = (unsafe { bytes_in(grant, grant_len, "grant") }) else {
                return cb_status::CB_ERR_NULL_ARGUMENT;
            };
            if session.inner.book_id().is_none() {
                return fail(
                    cb_status::CB_ERR_UNAVAILABLE,
                    "the session reached no shelf, so there is nothing to adopt",
                );
            }
            match app.inner.adopt_open(&session.inner, grant) {
                Ok(id) => {
                    out!(book, id.0, "book");
                    cb_status::CB_OK
                }
                Err(e) => from_error(&e),
            }
        })
    })
}

/// The grant that reaches an adopted book, by the row's fingerprint
/// ([`cb_shelf_fingerprint`](crate::cb_shelf_fingerprint)), as the bytes
/// the host handed over. The two-call idiom, for bytes: `needed` is set
/// on every path. `CB_ERR_UNAVAILABLE` when none was remembered.
#[no_mangle]
pub unsafe extern "C" fn cb_app_grant(
    app: *const cb_app,
    fingerprint: *const c_char,
    buf: *mut u8,
    cap: usize,
    needed: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_app!((app, fingerprint, buf, cap, needed) {
            clear_last_error();
            let app = app_ref!(app);
            // SAFETY: the header's contract.
            let Some(fingerprint) = (unsafe { str_in(fingerprint, "fingerprint") }) else {
                return cb_status::CB_ERR_NULL_ARGUMENT;
            };
            match app.inner.grant(fingerprint) {
                Ok(Some(grant)) => {
                    // SAFETY: the header's contract for the buffer triple.
                    unsafe { slice_out(&grant, buf, cap, needed) }
                }
                Ok(None) => fail(cb_status::CB_ERR_UNAVAILABLE, "no grant was remembered"),
                Err(e) => from_error(&e),
            }
        })
    })
}

/// Remember how to reach a book, by fingerprint — for a host that
/// re-granted a file the row already had.
#[no_mangle]
pub unsafe extern "C" fn cb_app_remember_grant(
    app: *mut cb_app,
    fingerprint: *const c_char,
    grant: *const u8,
    grant_len: usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_app!((app, fingerprint, grant, grant_len) {
            clear_last_error();
            let app = app_mut!(app);
            // SAFETY: the header's contract.
            let Some(fingerprint) = (unsafe { str_in(fingerprint, "fingerprint") }) else {
                return cb_status::CB_ERR_NULL_ARGUMENT;
            };
            let Some(grant) = (unsafe { bytes_in(grant, grant_len, "grant") }) else {
                return cb_status::CB_ERR_NULL_ARGUMENT;
            };
            match app.inner.remember_grant(fingerprint, grant) {
                Ok(()) => cb_status::CB_OK,
                Err(e) => from_error(&e),
            }
        })
    })
}

/// Forget how to reach a book. Succeeds when nothing was remembered.
#[no_mangle]
pub unsafe extern "C" fn cb_app_forget_grant(
    app: *mut cb_app,
    fingerprint: *const c_char,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_app!((app, fingerprint) {
            clear_last_error();
            let app = app_mut!(app);
            // SAFETY: the header's contract.
            let Some(fingerprint) = (unsafe { str_in(fingerprint, "fingerprint") }) else {
                return cb_status::CB_ERR_NULL_ARGUMENT;
            };
            match app.inner.forget_grant(fingerprint) {
                Ok(()) => cb_status::CB_OK,
                Err(e) => from_error(&e),
            }
        })
    })
}

/// What opening a shelf row came to.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum cb_opened {
    /// The library's own copy, open: `session` is set.
    CB_OPENED_SESSION = 0,
    /// The platform's file. Read the grant with [`cb_app_grant`] by the
    /// row's fingerprint, resolve it to a descriptor, and open with
    /// [`cb_session_open_fd`](crate::cb_session_open_fd) over
    /// [`cb_app_session_config`]. `session` is null.
    CB_OPENED_ADOPTED = 1,
    /// Out of reach: the copy is gone from disk, or an adopted book whose
    /// grant was never kept. The row survives; say the file is out of
    /// reach. `session` is null.
    CB_OPENED_MISSING = 2,
}

/// Open a shelf row for reading, whichever door it came in through. A
/// row that has left the shelf is `CB_ERR_LIBRARY`; a book that will not
/// open is its own error, as it would be from `cb_session_open_path`.
#[no_mangle]
pub unsafe extern "C" fn cb_app_open_book(
    app: *const cb_app,
    book: i64,
    session: *mut *mut cb_session,
    how: *mut cb_opened,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_app!((app, book, session, how) {
            use chapbook_app::Opened;
            clear_last_error();
            let app = app_ref!(app);
            if session.is_null() || how.is_null() {
                return fail(cb_status::CB_ERR_NULL_ARGUMENT, "an out-pointer is null");
            }
            match app.inner.open_book(chapbook_reader::chapbook_library::BookId(book)) {
                Ok(Opened::Session(opened)) => {
                    // SAFETY: checked non-null above.
                    unsafe {
                        *session = Box::into_raw(cb_session::boxed(*opened));
                        *how = cb_opened::CB_OPENED_SESSION;
                    }
                    cb_status::CB_OK
                }
                Ok(Opened::Adopted { .. }) => {
                    // SAFETY: as above.
                    unsafe {
                        *session = std::ptr::null_mut();
                        *how = cb_opened::CB_OPENED_ADOPTED;
                    }
                    cb_status::CB_OK
                }
                Ok(Opened::Missing) => {
                    // SAFETY: as above.
                    unsafe {
                        *session = std::ptr::null_mut();
                        *how = cb_opened::CB_OPENED_MISSING;
                    }
                    cb_status::CB_OK
                }
                Err(e) => from_error(&e),
            }
        })
    })
}

// ---- Preferences ----

/// What the reader's progress readout says, beside the whole-book bar.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum cb_progress_label {
    /// Percent of the whole book.
    CB_PROGRESS_PERCENT = 0,
    /// Pages left in this unit.
    CB_PROGRESS_PAGES_LEFT = 1,
    /// Unit and page, as raw indices.
    CB_PROGRESS_CHAPTER_PAGE = 2,
}

#[cfg(feature = "app")]
impl From<chapbook_app::ProgressLabel> for cb_progress_label {
    fn from(label: chapbook_app::ProgressLabel) -> cb_progress_label {
        use chapbook_app::ProgressLabel;
        match label {
            ProgressLabel::Percent => cb_progress_label::CB_PROGRESS_PERCENT,
            ProgressLabel::PagesLeft => cb_progress_label::CB_PROGRESS_PAGES_LEFT,
            ProgressLabel::ChapterPage => cb_progress_label::CB_PROGRESS_CHAPTER_PAGE,
        }
    }
}

#[cfg(feature = "app")]
impl From<cb_progress_label> for chapbook_app::ProgressLabel {
    fn from(label: cb_progress_label) -> chapbook_app::ProgressLabel {
        use chapbook_app::ProgressLabel;
        match label {
            cb_progress_label::CB_PROGRESS_PERCENT => ProgressLabel::Percent,
            cb_progress_label::CB_PROGRESS_PAGES_LEFT => ProgressLabel::PagesLeft,
            cb_progress_label::CB_PROGRESS_CHAPTER_PAGE => ProgressLabel::ChapterPage,
        }
    }
}

/// The reader's chosen readout. Percent until they choose.
#[no_mangle]
pub unsafe extern "C" fn cb_app_progress_label(
    app: *const cb_app,
    label: *mut cb_progress_label,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_app!((app, label) {
            clear_last_error();
            let app = app_ref!(app);
            out!(label, app.inner.progress_label().into(), "label");
            cb_status::CB_OK
        })
    })
}

/// Keep the reader's choice of readout, for every launch after this.
#[no_mangle]
pub unsafe extern "C" fn cb_app_set_progress_label(
    app: *mut cb_app,
    label: cb_progress_label,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_app!((app, label) {
            clear_last_error();
            let app = app_mut!(app);
            match app.inner.set_progress_label(label.into()) {
                Ok(()) => cb_status::CB_OK,
                Err(e) => from_error(&e),
            }
        })
    })
}

// ---- The reader's policy ----

/// Where the reader is, for the chrome, as one value read after a draw —
/// which is when a position is authoritative, because a restored
/// position lands on the first frame rather than at open. The title is
/// [`cb_session_title`](crate::cb_session_title)'s.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct cb_place {
    pub spine: usize,
    pub spine_len: usize,
    pub page: usize,
    pub page_count: usize,
    /// Whole-book progress, 0 to 1, spine-weighted the way the engine's
    /// own progression is: each unit a `1/spine_len` slice, the page's
    /// place within it added.
    pub book_fraction: f64,
    /// Pages after this one in the current unit — the pages-left readout.
    pub pages_left: usize,
    /// Whether the engine's Back has anywhere to go — what decides
    /// whether a *Return* is drawn.
    pub can_go_back: bool,
}

/// Read the place off a session. Lays the current unit out if nothing
/// has yet, which is why it takes the session mutably.
#[no_mangle]
pub unsafe extern "C" fn cb_reader_place(
    session: *mut cb_session,
    out: *mut cb_place,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_app!((session, out) {
            clear_last_error();
            // SAFETY: a handle from an open call, not yet closed.
            let Some(session) = (unsafe { session.as_mut() }) else {
                return fail(cb_status::CB_ERR_NULL_ARGUMENT, "session is null");
            };
            let place = chapbook_app::reader::Place::of(&mut session.inner);
            let filled = cb_place {
                spine: place.spine,
                spine_len: place.spine_len,
                page: place.page,
                page_count: place.page_count,
                book_fraction: place.book_fraction,
                pages_left: place.pages_left(),
                can_go_back: place.can_go_back,
            };
            out!(out, filled, "place");
            cb_status::CB_OK
        })
    })
}

/// How much of what the platform says this process may use goes to a
/// session's page cache: a quarter of `available_bytes`, between a floor
/// below which a page-back rereads the chapter and a cap above which a
/// phone holds chapters nobody will page back to. The number is the
/// platform's — `ActivityManager.memoryClass`,
/// `os_proc_available_memory`, a fixed figure on a device with no such
/// callback — and the policy is the same for all of them. Hand the
/// answer to `cb_config_set_cache_budget`.
#[no_mangle]
pub extern "C" fn cb_reader_cache_budget_for(available_bytes: u64) -> usize {
    guard(0, || {
        #[cfg(feature = "app")]
        {
            chapbook_app::reader::cache_budget_for(available_bytes)
        }
        #[cfg(not(feature = "app"))]
        {
            // The engine's own floor, so a build without the layer still
            // gets a number rather than zero, which would evict the page
            // being read.
            let _ = available_bytes;
            16 << 20
        }
    })
}

/// What a memory warning does to a session: halve its budget, which
/// evicts at once, and release its caches. Halving keeps the next
/// warning from finding the same cache; the floor keeps the page on
/// screen. `budget` receives the budget now in force.
#[no_mangle]
pub unsafe extern "C" fn cb_reader_after_memory_warning(
    session: *mut cb_session,
    budget: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_app!((session, budget) {
            clear_last_error();
            // SAFETY: a handle from an open call, not yet closed.
            let Some(session) = (unsafe { session.as_mut() }) else {
                return fail(cb_status::CB_ERR_NULL_ARGUMENT, "session is null");
            };
            let now = chapbook_app::reader::after_memory_warning(&mut session.inner);
            if !budget.is_null() {
                // SAFETY: checked non-null just above.
                unsafe { *budget = now };
            }
            cb_status::CB_OK
        })
    })
}

/// Jump to a hit and leave it selected, so the eye finds it — what a
/// results row does when tapped. `moved` receives whether the position
/// changed.
#[no_mangle]
pub unsafe extern "C" fn cb_reader_show_hit(
    session: *mut cb_session,
    hit: *const cb_search_hit,
    moved: *mut bool,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_app!((session, hit, moved) {
            clear_last_error();
            // SAFETY: a handle from an open call, not yet closed.
            let Some(session) = (unsafe { session.as_mut() }) else {
                return fail(cb_status::CB_ERR_NULL_ARGUMENT, "session is null");
            };
            // SAFETY: the caller's struct, readable for the call.
            let Some(hit) = (unsafe { hit.as_ref() }) else {
                return fail(cb_status::CB_ERR_NULL_ARGUMENT, "hit is null");
            };
            let hit = chapbook_reader::SearchHit {
                locator: chapbook_reader::chapbook_core::Locator {
                    spine_index: hit.spine,
                    char_offset: hit.start,
                },
                end: hit.end,
                context: String::new(),
                match_range: (hit.match_start, hit.match_end),
            };
            let did = chapbook_app::reader::show_hit(&mut session.inner, &hit);
            if !moved.is_null() {
                // SAFETY: checked non-null just above.
                unsafe { *moved = did };
            }
            cb_status::CB_OK
        })
    })
}

/// The selection becomes a highlight, and the selection goes.
/// `CB_ERR_UNAVAILABLE` when nothing is selected.
#[no_mangle]
pub unsafe extern "C" fn cb_reader_highlight_selection(
    session: *mut cb_session,
    id: *mut i64,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_app!((session, id) {
            clear_last_error();
            // SAFETY: a handle from an open call, not yet closed.
            let Some(session) = (unsafe { session.as_mut() }) else {
                return fail(cb_status::CB_ERR_NULL_ARGUMENT, "session is null");
            };
            let Some(made) = chapbook_app::reader::highlight_selection(&mut session.inner) else {
                return fail(cb_status::CB_ERR_UNAVAILABLE, "nothing is selected");
            };
            out!(id, made, "id");
            cb_status::CB_OK
        })
    })
}

/// The selection becomes a note with `body`, and the selection goes.
/// `CB_ERR_UNAVAILABLE` when nothing is selected.
#[no_mangle]
pub unsafe extern "C" fn cb_reader_note_on_selection(
    session: *mut cb_session,
    body: *const c_char,
    id: *mut i64,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_app!((session, body, id) {
            clear_last_error();
            // SAFETY: a handle from an open call, not yet closed.
            let Some(session) = (unsafe { session.as_mut() }) else {
                return fail(cb_status::CB_ERR_NULL_ARGUMENT, "session is null");
            };
            // SAFETY: the header's contract.
            let Some(body) = (unsafe { str_in(body, "body") }) else {
                return cb_status::CB_ERR_NULL_ARGUMENT;
            };
            let Some(made) = chapbook_app::reader::note_on_selection(&mut session.inner, body)
            else {
                return fail(cb_status::CB_ERR_UNAVAILABLE, "nothing is selected");
            };
            out!(id, made, "id");
            cb_status::CB_OK
        })
    })
}

// ---- The search walk ----

/// A search walked one unit at a time on the session's own thread, so
/// the page stays responsive between steps. Opaque.
///
/// The blocking whole-book `cb_session_search` would want a worker, and a
/// worker would touch the session while the page draws — the one rule
/// the engine has. So the walk is a cursor: the host calls
/// [`cb_search_walk_step`] from its own loop, yielding however its
/// platform yields between units, and reads the hits so far after each.
/// It stops itself at a cap past which a results list is a scroll
/// nobody finishes.
pub struct cb_search_walk {
    #[cfg(feature = "app")]
    inner: chapbook_app::reader::SearchWalk,
}

/// Begin a search. `CB_ERR_INVALID_ARGUMENT` for a query that is only
/// whitespace, which clears rather than searches.
#[no_mangle]
pub unsafe extern "C" fn cb_search_walk_open(
    query: *const c_char,
    out: *mut *mut cb_search_walk,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_app!((query, out) {
            clear_last_error();
            // SAFETY: the header's contract.
            let Some(query) = (unsafe { str_in(query, "query") }) else {
                return cb_status::CB_ERR_NULL_ARGUMENT;
            };
            if out.is_null() {
                return fail(cb_status::CB_ERR_NULL_ARGUMENT, "out is null");
            }
            let Some(walk) = chapbook_app::reader::SearchWalk::new(query) else {
                return fail(cb_status::CB_ERR_INVALID_ARGUMENT, "the query is blank");
            };
            // SAFETY: checked non-null above.
            unsafe { *out = Box::into_raw(Box::new(cb_search_walk { inner: walk })) };
            cb_status::CB_OK
        })
    })
}

/// Search the next unit of `session`. `more` receives whether there is
/// another to search: false once the last unit is searched or the cap is
/// reached, and on every call after.
#[no_mangle]
pub unsafe extern "C" fn cb_search_walk_step(
    walk: *mut cb_search_walk,
    session: *mut cb_session,
    more: *mut bool,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_app!((walk, session, more) {
            clear_last_error();
            // SAFETY: a handle from `cb_search_walk_open`, not yet closed.
            let Some(walk) = (unsafe { walk.as_mut() }) else {
                return fail(cb_status::CB_ERR_NULL_ARGUMENT, "walk is null");
            };
            // SAFETY: a handle from an open call, not yet closed.
            let Some(session) = (unsafe { session.as_mut() }) else {
                return fail(cb_status::CB_ERR_NULL_ARGUMENT, "session is null");
            };
            let left = walk.inner.step(&mut session.inner);
            out!(more, left, "more");
            cb_status::CB_OK
        })
    })
}

/// How many hits the walk has found so far.
#[no_mangle]
pub unsafe extern "C" fn cb_search_walk_hit_count(
    walk: *const cb_search_walk,
    count: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_app!((walk, count) {
            clear_last_error();
            // SAFETY: a handle from `cb_search_walk_open`, not yet closed.
            let Some(walk) = (unsafe { walk.as_ref() }) else {
                return fail(cb_status::CB_ERR_NULL_ARGUMENT, "walk is null");
            };
            out!(count, walk.inner.hits().len(), "count");
            cb_status::CB_OK
        })
    })
}

/// One hit's plain data, by index into the hits so far.
#[no_mangle]
pub unsafe extern "C" fn cb_search_walk_hit(
    walk: *const cb_search_walk,
    index: usize,
    out: *mut cb_search_hit,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_app!((walk, index, out) {
            clear_last_error();
            // SAFETY: a handle from `cb_search_walk_open`, not yet closed.
            let Some(walk) = (unsafe { walk.as_ref() }) else {
                return fail(cb_status::CB_ERR_NULL_ARGUMENT, "walk is null");
            };
            let Some(hit) = walk.inner.hits().get(index) else {
                return fail(
                    cb_status::CB_ERR_INVALID_ARGUMENT,
                    format!("hit index {index} out of {}", walk.inner.hits().len()),
                );
            };
            let filled = cb_search_hit {
                spine: hit.locator.spine_index,
                start: hit.locator.char_offset,
                end: hit.end,
                match_start: hit.match_range.0,
                match_end: hit.match_range.1,
            };
            out!(out, filled, "hit");
            cb_status::CB_OK
        })
    })
}

/// A hit's context: the match with a little text either side, for a
/// results row.
#[no_mangle]
pub unsafe extern "C" fn cb_search_walk_context(
    walk: *const cb_search_walk,
    index: usize,
    buf: *mut c_char,
    cap: usize,
    needed: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_app!((walk, index, buf, cap, needed) {
            clear_last_error();
            // SAFETY: a handle from `cb_search_walk_open`, not yet closed.
            let Some(walk) = (unsafe { walk.as_ref() }) else {
                return fail(cb_status::CB_ERR_NULL_ARGUMENT, "walk is null");
            };
            let Some(hit) = walk.inner.hits().get(index) else {
                return fail(
                    cb_status::CB_ERR_INVALID_ARGUMENT,
                    format!("hit index {index} out of {}", walk.inner.hits().len()),
                );
            };
            // SAFETY: the header's contract for the buffer triple.
            unsafe { str_out(&hit.context, buf, cap, needed) }
        })
    })
}

/// Close a walk. Accepts null.
#[no_mangle]
pub unsafe extern "C" fn cb_search_walk_close(walk: *mut cb_search_walk) {
    guard((), || {
        if !walk.is_null() {
            // SAFETY: a handle from `cb_search_walk_open`, freed once.
            drop(unsafe { Box::from_raw(walk) });
        }
    })
}

// ---- Saved catalogs ----

/// Which string a saved catalog accessor answers with.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum cb_saved_catalog_field {
    /// What the reader called it, or what its feed did; may be empty.
    CB_SAVED_CATALOG_TITLE = 0,
    /// Opaque and possibly secret-bearing: never log it.
    CB_SAVED_CATALOG_URL = 1,
}

macro_rules! with_opds {
    (($($unused:ident),* $(,)?) $body:block) => {{
        #[cfg(feature = "opds")]
        $body
        #[cfg(not(feature = "opds"))]
        {
            $(let _ = $unused;)*
            fail(
                cb_status::CB_ERR_FORMAT_NOT_BUILT,
                "this build has no OPDS support, so there is no catalog to keep",
            )
        }
    }};
}

/// How many catalogs the reader has added.
#[no_mangle]
pub unsafe extern "C" fn cb_app_catalog_count(app: *const cb_app, count: *mut usize) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_opds!((app, count) {
            clear_last_error();
            let app = app_ref!(app);
            match app.inner.catalogs() {
                Ok(catalogs) => {
                    out!(count, catalogs.len(), "count");
                    cb_status::CB_OK
                }
                Err(e) => from_error(&e),
            }
        })
    })
}

/// The id of the saved catalog at `index`, in the order they were added
/// — the key for [`cb_app_browse`], [`cb_app_rename_catalog`] and
/// [`cb_app_remove_catalog`].
#[no_mangle]
pub unsafe extern "C" fn cb_app_catalog_id(
    app: *const cb_app,
    index: usize,
    id: *mut i64,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_opds!((app, index, id) {
            clear_last_error();
            let app = app_ref!(app);
            match app.inner.catalogs() {
                Ok(catalogs) => match catalogs.get(index) {
                    Some(saved) => {
                        out!(id, saved.id, "id");
                        cb_status::CB_OK
                    }
                    None => fail(
                        cb_status::CB_ERR_INVALID_ARGUMENT,
                        format!("catalog index {index} out of {}", catalogs.len()),
                    ),
                },
                Err(e) => from_error(&e),
            }
        })
    })
}

/// One of a saved catalog's strings, by index.
#[no_mangle]
pub unsafe extern "C" fn cb_app_catalog_text(
    app: *const cb_app,
    index: usize,
    field: cb_saved_catalog_field,
    buf: *mut c_char,
    cap: usize,
    needed: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_opds!((app, index, field, buf, cap, needed) {
            clear_last_error();
            let app = app_ref!(app);
            let catalogs = match app.inner.catalogs() {
                Ok(catalogs) => catalogs,
                Err(e) => return from_error(&e),
            };
            let Some(saved) = catalogs.get(index) else {
                return fail(
                    cb_status::CB_ERR_INVALID_ARGUMENT,
                    format!("catalog index {index} out of {}", catalogs.len()),
                );
            };
            let value = match field {
                cb_saved_catalog_field::CB_SAVED_CATALOG_TITLE => &saved.title,
                cb_saved_catalog_field::CB_SAVED_CATALOG_URL => &saved.url,
            };
            // SAFETY: the header's contract for the buffer triple.
            unsafe { str_out(value, buf, cap, needed) }
        })
    })
}

/// Add a catalog. `title` may be empty until its feed says what it is
/// called. Surrounding whitespace on either is dropped.
#[no_mangle]
pub unsafe extern "C" fn cb_app_add_catalog(
    app: *mut cb_app,
    url: *const c_char,
    title: *const c_char,
    id: *mut i64,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_opds!((app, url, title, id) {
            clear_last_error();
            let app = app_mut!(app);
            // SAFETY: the header's contract for both.
            let (Some(url), Some(title)) = (
                unsafe { str_in(url, "url") },
                unsafe { str_in(title, "title") },
            ) else {
                return cb_status::CB_ERR_NULL_ARGUMENT;
            };
            if url.trim().is_empty() {
                return fail(cb_status::CB_ERR_INVALID_ARGUMENT, "url is empty");
            }
            match app.inner.add_catalog(url, title) {
                Ok(saved) => {
                    out!(id, saved.id, "id");
                    cb_status::CB_OK
                }
                Err(e) => from_error(&e),
            }
        })
    })
}

/// Give a saved catalog a title. `CB_ERR_UNAVAILABLE` for an id that
/// has been removed.
#[no_mangle]
pub unsafe extern "C" fn cb_app_rename_catalog(
    app: *mut cb_app,
    id: i64,
    title: *const c_char,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_opds!((app, id, title) {
            clear_last_error();
            let app = app_mut!(app);
            // SAFETY: the header's contract.
            let Some(title) = (unsafe { str_in(title, "title") }) else {
                return cb_status::CB_ERR_NULL_ARGUMENT;
            };
            match app.inner.rename_catalog(id, title) {
                Ok(true) => cb_status::CB_OK,
                Ok(false) => fail(cb_status::CB_ERR_UNAVAILABLE, "no such catalog"),
                Err(e) => from_error(&e),
            }
        })
    })
}

/// Take a catalog off the list. Its books stay on the shelf, and its
/// credential stays in the store — keyed by origin, it may serve another
/// catalog on the same host. `CB_ERR_UNAVAILABLE` when already gone.
#[no_mangle]
pub unsafe extern "C" fn cb_app_remove_catalog(app: *mut cb_app, id: i64) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_opds!((app, id) {
            clear_last_error();
            let app = app_mut!(app);
            match app.inner.remove_catalog(id) {
                Ok(true) => cb_status::CB_OK,
                Ok(false) => fail(cb_status::CB_ERR_UNAVAILABLE, "no such catalog"),
                Err(e) => from_error(&e),
            }
        })
    })
}

// ---- Browsing ----

/// Browse a catalog: a [`cb_catalog`](crate::cb_catalog) over the app's
/// transport and credential store, titled after the saved row `catalog`
/// until its feed says otherwise, or over no row at all when `catalog`
/// is zero — a URL the reader pasted. Every accessor in the catalog
/// module reads it; the verbs below drive it.
///
/// The handle is the caller's to close with `cb_catalog_close` and, like
/// every catalog, one thread's at a time — the thread that does the
/// blocking fetches. It does not need the app to stay open.
#[no_mangle]
pub unsafe extern "C" fn cb_app_browse(
    app: *const cb_app,
    catalog: i64,
    out: *mut *mut crate::catalog::cb_catalog,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_opds!((app, catalog, out) {
            clear_last_error();
            let app = app_ref!(app);
            if out.is_null() {
                return fail(cb_status::CB_ERR_NULL_ARGUMENT, "out is null");
            }
            let opened = if catalog == 0 {
                app.inner.open_catalog()
            } else {
                match app.inner.catalog(catalog) {
                    Ok(Some(saved)) => app.inner.browse(&saved),
                    Ok(None) => return fail(cb_status::CB_ERR_UNAVAILABLE, "no such catalog"),
                    Err(e) => return from_error(&e),
                }
            };
            match opened {
                Ok(inner) => {
                    // SAFETY: checked non-null above.
                    unsafe {
                        *out = Box::into_raw(Box::new(crate::catalog::cb_catalog { inner }));
                    }
                    cb_status::CB_OK
                }
                Err(e) => from_error(&e),
            }
        })
    })
}

/// What a catalog screen shows.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum cb_browse_state {
    /// Nothing fetched yet.
    CB_BROWSE_OPENING = 0,
    /// A feed is held; read it through the `cb_catalog_*` accessors.
    CB_BROWSE_FEED = 1,
    /// A 401 with a login to draw: `CB_BROWSE_LOGIN_TITLE`,
    /// [`cb_catalog_auth_offers_basic`](crate::cb_catalog_auth_offers_basic),
    /// then [`cb_catalog_sign_in`].
    CB_BROWSE_LOGIN = 2,
    /// The last fetch failed and there is nothing to show;
    /// `CB_BROWSE_FAILURE_URL` and `CB_BROWSE_FAILURE_REASON` say what.
    CB_BROWSE_FAILED = 3,
}

/// Which string [`cb_catalog_browse_text`] answers with.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum cb_browse_field {
    /// What goes at the top: the held feed's title, or the saved
    /// catalog's until there is one.
    CB_BROWSE_TITLE = 0,
    /// The URL the held feed came from.
    CB_BROWSE_URL = 1,
    /// `CB_BROWSE_LOGIN` only: the catalog's own name for its login.
    CB_BROWSE_LOGIN_TITLE = 2,
    /// `CB_BROWSE_LOGIN` only: the URL that was refused, which a sign-in
    /// fetches again.
    CB_BROWSE_RETRY_URL = 3,
    /// `CB_BROWSE_FAILED` only: the URL that failed.
    CB_BROWSE_FAILURE_URL = 4,
    /// `CB_BROWSE_FAILED` only: why, for a log. The host has its own
    /// sentence for the reader.
    CB_BROWSE_FAILURE_REASON = 5,
}

#[cfg(feature = "opds")]
macro_rules! catalog_mut {
    ($catalog:expr) => {
        // SAFETY: a handle from `cb_catalog_open` or `cb_app_browse`, not
        // yet closed.
        match unsafe { $catalog.as_mut() } {
            Some(catalog) => catalog,
            None => return fail(cb_status::CB_ERR_NULL_ARGUMENT, "catalog is null"),
        }
    };
}

#[cfg(feature = "opds")]
macro_rules! catalog_ref {
    ($catalog:expr) => {
        // SAFETY: a handle from `cb_catalog_open` or `cb_app_browse`, not
        // yet closed.
        match unsafe { $catalog.as_ref() } {
            Some(catalog) => catalog,
            None => return fail(cb_status::CB_ERR_NULL_ARGUMENT, "catalog is null"),
        }
    };
}

/// Open a feed the reader chose — the root, a navigation row's href, a
/// facet — pushing a crumb [`cb_catalog_back`] returns to. The state
/// follows whatever happens; the status says which: `CB_OK` for a feed,
/// `CB_ERR_AUTH_REQUIRED` for a login, the fetch's own code for a
/// failure. Blocking.
#[no_mangle]
pub unsafe extern "C" fn cb_catalog_go(
    catalog: *mut crate::catalog::cb_catalog,
    url: *const c_char,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_opds!((catalog, url) {
            clear_last_error();
            let catalog = catalog_mut!(catalog);
            // SAFETY: the header's contract.
            let Some(url) = (unsafe { str_in(url, "url") }) else {
                return cb_status::CB_ERR_NULL_ARGUMENT;
            };
            match catalog.inner.go(url) {
                Ok(()) => cb_status::CB_OK,
                Err(e) => crate::catalog::opds_failure(e),
            }
        })
    })
}

/// Back, inside the catalog: fetch the previous crumb again. `stayed`
/// receives false at the root, which is the cue to leave the screen.
/// Blocking when it stays.
#[no_mangle]
pub unsafe extern "C" fn cb_catalog_back(
    catalog: *mut crate::catalog::cb_catalog,
    stayed: *mut bool,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_opds!((catalog, stayed) {
            clear_last_error();
            let catalog = catalog_mut!(catalog);
            let did = catalog.inner.back();
            out!(stayed, did, "stayed");
            cb_status::CB_OK
        })
    })
}

/// Fetch the next page and append its rows to the held feed, for an
/// infinite scroll. `appended` receives false when there is no next
/// page. On failure the held feed is untouched and the state stays a
/// feed: a page that did not arrive is a row that did not appear, not a
/// screen that failed. Blocking.
#[no_mangle]
pub unsafe extern "C" fn cb_catalog_load_more(
    catalog: *mut crate::catalog::cb_catalog,
    appended: *mut bool,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_opds!((catalog, appended) {
            clear_last_error();
            let catalog = catalog_mut!(catalog);
            match catalog.inner.load_more() {
                Ok(did) => {
                    out!(appended, did, "appended");
                    cb_status::CB_OK
                }
                Err(e) => crate::catalog::opds_failure(e),
            }
        })
    })
}

/// Narrow by a facet of the held feed, by index into
/// [`cb_catalog_facet`](crate::cb_catalog_facet)'s list — a fetch that
/// pushes a crumb, like [`cb_catalog_go`]. Blocking.
#[no_mangle]
pub unsafe extern "C" fn cb_catalog_apply_facet(
    catalog: *mut crate::catalog::cb_catalog,
    index: usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_opds!((catalog, index) {
            clear_last_error();
            let catalog = catalog_mut!(catalog);
            if index >= catalog.inner.facets().len() {
                return fail(
                    cb_status::CB_ERR_INVALID_ARGUMENT,
                    format!("facet index {index} out of {}", catalog.inner.facets().len()),
                );
            }
            match catalog.inner.apply_facet(index) {
                Ok(()) => cb_status::CB_OK,
                Err(e) => crate::catalog::opds_failure(e),
            }
        })
    })
}

/// Sign in to the catalog that refused: store the credential by the
/// refused URL's origin — through the app's credential store, never by
/// the URL — and fetch it again without moving a crumb. A store that
/// cannot keep it still signs this session in. `CB_ERR_UNAVAILABLE` when
/// the state is not `CB_BROWSE_LOGIN`. Blocking.
#[no_mangle]
pub unsafe extern "C" fn cb_catalog_sign_in(
    catalog: *mut crate::catalog::cb_catalog,
    username: *const c_char,
    password: *const c_char,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_opds!((catalog, username, password) {
            clear_last_error();
            let catalog = catalog_mut!(catalog);
            // SAFETY: the header's contract for both.
            let (Some(username), Some(password)) = (
                unsafe { str_in(username, "username") },
                unsafe { str_in(password, "password") },
            ) else {
                return cb_status::CB_ERR_NULL_ARGUMENT;
            };
            if !matches!(catalog.inner.state(), chapbook_app::BrowseState::Login { .. }) {
                return fail(cb_status::CB_ERR_UNAVAILABLE, "nothing has asked for a login");
            }
            match catalog.inner.sign_in(username, password) {
                Ok(()) => cb_status::CB_OK,
                Err(e) => crate::catalog::opds_failure(e),
            }
        })
    })
}

/// What the screen shows now.
#[no_mangle]
pub unsafe extern "C" fn cb_catalog_state(
    catalog: *const crate::catalog::cb_catalog,
    state: *mut cb_browse_state,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_opds!((catalog, state) {
            use chapbook_app::BrowseState;
            clear_last_error();
            let catalog = catalog_ref!(catalog);
            let now = match catalog.inner.state() {
                BrowseState::Opening => cb_browse_state::CB_BROWSE_OPENING,
                BrowseState::Feed => cb_browse_state::CB_BROWSE_FEED,
                BrowseState::Login { .. } => cb_browse_state::CB_BROWSE_LOGIN,
                BrowseState::Failed { .. } => cb_browse_state::CB_BROWSE_FAILED,
            };
            out!(state, now, "state");
            cb_status::CB_OK
        })
    })
}

/// One of the browse strings. `CB_ERR_UNAVAILABLE` for a field the
/// current state does not carry.
#[no_mangle]
pub unsafe extern "C" fn cb_catalog_browse_text(
    catalog: *const crate::catalog::cb_catalog,
    field: cb_browse_field,
    buf: *mut c_char,
    cap: usize,
    needed: *mut usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_opds!((catalog, field, buf, cap, needed) {
            use chapbook_app::BrowseState;
            clear_last_error();
            let catalog = catalog_ref!(catalog);
            let value: Option<String> = match (field, catalog.inner.state()) {
                (cb_browse_field::CB_BROWSE_TITLE, _) => Some(catalog.inner.title()),
                (cb_browse_field::CB_BROWSE_URL, _) => Some(catalog.inner.base().to_string()),
                (cb_browse_field::CB_BROWSE_LOGIN_TITLE, BrowseState::Login { title, .. }) => {
                    Some(title.clone())
                }
                (cb_browse_field::CB_BROWSE_RETRY_URL, BrowseState::Login { retry, .. }) => {
                    Some(retry.clone())
                }
                (cb_browse_field::CB_BROWSE_FAILURE_URL, BrowseState::Failed { url, .. }) => {
                    Some(url.clone())
                }
                (cb_browse_field::CB_BROWSE_FAILURE_REASON, BrowseState::Failed { reason, .. }) => {
                    Some(reason.clone())
                }
                _ => None,
            };
            let Some(value) = value else {
                return fail(
                    cb_status::CB_ERR_UNAVAILABLE,
                    "the current state carries no such value",
                );
            };
            // SAFETY: the header's contract for the buffer triple.
            unsafe { str_out(&value, buf, cap, needed) }
        })
    })
}

// ---- Downloads ----

/// How a platform transfer's HTTP status is read.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum cb_download_outcome {
    /// The file is the book. Land it with [`cb_app_land_download`].
    CB_DOWNLOAD_LANDED = 0,
    /// 401 or 403: the credential the job carried is wrong or gone. Not
    /// worth retrying with the same one; worth a sign-in.
    CB_DOWNLOAD_REFUSED = 1,
    /// 404, 410, or any other client-side answer: the catalog no longer
    /// offers this. Retrying will not change its mind.
    CB_DOWNLOAD_GONE = 2,
    /// A 5xx, or no response at all: retry with backoff, which is what a
    /// job system does when told.
    CB_DOWNLOAD_AGAIN = 3,
}

/// Read a transfer's status the way every front end reads it. Zero — no
/// response — is `CB_DOWNLOAD_AGAIN`: a transfer that never reached the
/// service is a network condition, not a verdict.
#[no_mangle]
pub extern "C" fn cb_download_outcome_of_status(status: u16) -> cb_download_outcome {
    guard(cb_download_outcome::CB_DOWNLOAD_AGAIN, || {
        #[cfg(feature = "opds")]
        {
            use chapbook_app::DownloadOutcome;
            match DownloadOutcome::of_status(status) {
                DownloadOutcome::Landed => cb_download_outcome::CB_DOWNLOAD_LANDED,
                DownloadOutcome::Refused => cb_download_outcome::CB_DOWNLOAD_REFUSED,
                DownloadOutcome::Gone => cb_download_outcome::CB_DOWNLOAD_GONE,
                DownloadOutcome::Again => cb_download_outcome::CB_DOWNLOAD_AGAIN,
            }
        }
        #[cfg(not(feature = "opds"))]
        {
            // The same table, so a build without catalogs still reads a
            // status a host somehow has the way the others do.
            match status {
                0 | 500..=599 => cb_download_outcome::CB_DOWNLOAD_AGAIN,
                200..=299 => cb_download_outcome::CB_DOWNLOAD_LANDED,
                401 | 403 => cb_download_outcome::CB_DOWNLOAD_REFUSED,
                _ => cb_download_outcome::CB_DOWNLOAD_GONE,
            }
        }
    })
}

/// Everything a landed download does: import the file the platform's
/// transfer produced and record the sync services the catalog entry
/// advertised — `CB_ENTRY_PROGRESSION_URL` and
/// `CB_ENTRY_ANNOTATION_CONTAINER`, read before the transfer, either or
/// both null. The file is the caller's and is left where it was.
/// Landing the same bytes twice answers with the row they already have,
/// so a retried job needs no bookkeeping of its own.
#[no_mangle]
pub unsafe extern "C" fn cb_app_land_download(
    app: *mut cb_app,
    file: *const c_char,
    progression_url: *const c_char,
    annotation_container: *const c_char,
    book: *mut i64,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_opds!((app, file, progression_url, annotation_container, book) {
            clear_last_error();
            let app = app_mut!(app);
            // SAFETY: the header's contract.
            let Some(file) = (unsafe { str_in(file, "file") }) else {
                return cb_status::CB_ERR_NULL_ARGUMENT;
            };
            // Null means none, for these two.
            let progression = if progression_url.is_null() {
                None
            } else {
                // SAFETY: the header's contract.
                match unsafe { str_in(progression_url, "progression_url") } {
                    Some(value) => Some(value),
                    None => return cb_status::CB_ERR_INVALID_UTF8,
                }
            };
            let container = if annotation_container.is_null() {
                None
            } else {
                // SAFETY: the header's contract.
                match unsafe { str_in(annotation_container, "annotation_container") } {
                    Some(value) => Some(value),
                    None => return cb_status::CB_ERR_INVALID_UTF8,
                }
            };
            match app
                .inner
                .land_download(std::path::Path::new(file), progression, container)
            {
                Ok(id) => {
                    out!(book, id.0, "book");
                    cb_status::CB_OK
                }
                Err(e) => from_error(&e),
            }
        })
    })
}

// ---- Sync ----

/// Ask for every book with a service to reconcile, starting the app's
/// sync driver on first use. `started` receives false — and nothing is
/// asked — when no book on the shelf has a service, which is a fact to
/// tell the reader rather than a spinner to show them.
///
/// The driver reconciles over the app's transport, authorizing each book
/// from the app's credential store by the origin of its service, and
/// speaks as the device named at `cb_app_open`, under an identity minted
/// once and kept beside the library. `wake` follows
/// [`cb_session_set_waker`](crate::cb_session_set_waker)'s contract: it
/// fires on the driver's thread, once per report, and must only nudge
/// the host to come call [`cb_app_sync_next`]. The wake given the first
/// time the driver starts is the one it keeps.
#[no_mangle]
pub unsafe extern "C" fn cb_app_sync_all(
    app: *mut cb_app,
    wake: cb_wake_fn,
    wake_user: *mut c_void,
    started: *mut bool,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_opds!((app, wake, wake_user, started) {
            clear_last_error();
            let app = app_mut!(app);
            match app.inner.sync_all(waker(wake, wake_user)) {
                Ok(did) => {
                    out!(started, did, "started");
                    cb_status::CB_OK
                }
                Err(e) => from_error(&e),
            }
        })
    })
}

/// Ask for one book to reconcile. A book with no service reports
/// `CB_SYNC_BOOK_FAILED` rather than being silently skipped.
#[no_mangle]
pub unsafe extern "C" fn cb_app_sync_book(
    app: *mut cb_app,
    book: i64,
    wake: cb_wake_fn,
    wake_user: *mut c_void,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_opds!((app, book, wake, wake_user) {
            clear_last_error();
            let app = app_mut!(app);
            match app.inner.sync_book(
                chapbook_reader::chapbook_library::BookId(book),
                waker(wake, wake_user),
            ) {
                Ok(()) => cb_status::CB_OK,
                Err(e) => from_error(&e),
            }
        })
    })
}

/// Take the next sync report, oldest first, on
/// [`cb_sync_next`](crate::cb_sync_next)'s terms: `CB_ERR_UNAVAILABLE`
/// when none is waiting, strings valid until the next call. One kind
/// more than a worker reports: `CB_SYNC_BROKEN`, a batch that could not
/// start, with `detail` saying why.
#[no_mangle]
pub unsafe extern "C" fn cb_app_sync_next(app: *mut cb_app, out: *mut cb_sync_report) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        with_opds!((app, out) {
            use chapbook_app::SyncStatus;
            clear_last_error();
            let app = app_mut!(app);
            if out.is_null() {
                return fail(cb_status::CB_ERR_NULL_ARGUMENT, "out is null");
            }
            if app.pending.is_empty() {
                app.pending.extend(app.inner.sync_events());
            }
            let Some(status) = app.pending.pop_front() else {
                return fail(cb_status::CB_ERR_UNAVAILABLE, "no report waiting");
            };
            let report = match status {
                SyncStatus::Book(report) => {
                    crate::sync::report_of(
                        chapbook_app::chapbook_sync::SyncEvent::Book(report),
                        &mut app.strings,
                    )
                }
                SyncStatus::Failed { book, reason } => crate::sync::report_of(
                    chapbook_app::chapbook_sync::SyncEvent::Failed { book, reason },
                    &mut app.strings,
                ),
                SyncStatus::Finished { books } => crate::sync::report_of(
                    chapbook_app::chapbook_sync::SyncEvent::Finished { books },
                    &mut app.strings,
                ),
                SyncStatus::Broken(reason) => {
                    app.strings.clear();
                    let mut report = crate::sync::blank_report();
                    report.kind = crate::sync::cb_sync_kind::CB_SYNC_BROKEN;
                    report.detail = crate::sync::keep(&mut app.strings, &reason);
                    report
                }
            };
            // SAFETY: checked non-null above.
            unsafe { *out = report };
            cb_status::CB_OK
        })
    })
}

#[cfg(feature = "opds")]
fn waker(wake: cb_wake_fn, user: *mut c_void) -> std::sync::Arc<dyn Fn() + Send + Sync> {
    let user = user as usize;
    match wake {
        Some(wake) => std::sync::Arc::new(move || wake(user as *mut c_void)),
        None => std::sync::Arc::new(|| {}),
    }
}

/// Borrow `len` bytes at `ptr`; null with a nonzero length is reported.
/// A zero-length grant is legal — a host whose token is "the path the
/// row already has" may say so with nothing.
#[cfg(feature = "app")]
unsafe fn bytes_in<'a>(ptr: *const u8, len: usize, what: &str) -> Option<&'a [u8]> {
    if len == 0 {
        return Some(&[]);
    }
    if ptr.is_null() {
        fail(cb_status::CB_ERR_NULL_ARGUMENT, format!("{what} is null"));
        return None;
    }
    // SAFETY: the header's contract — `len` readable bytes at `ptr`,
    // valid for the duration of the call.
    Some(unsafe { std::slice::from_raw_parts(ptr, len) })
}
