//! The host's HTTP transport, crossing C.
//!
//! The bundled `ureq` transport is right for a desktop process and wrong
//! everywhere a platform owns the networking: an iOS app that opens its
//! own sockets gives up background transfer, the system trust store, App
//! Transport Security, per-app VPN and the cellular-data toggle; an
//! Android app gives up WorkManager. `SessionConfig::with_transport` is
//! the Rust-side answer, and this module is that seam crossing the ABI —
//! [`cb_config_set_http_transport`] installs host callbacks that fetch,
//! and every catalog request the session makes goes through them instead
//! of `ureq`.
//!
//! The response comes back through a builder rather than a struct the
//! host fills in, for the same reason the config does: a response is
//! assembled from parts that arrive at different times (a status, maybe a
//! content type, body chunks), and each setter can check its argument and
//! say what is wrong with it.
//!
//! What the callbacks must do is the [`HttpClient`] contract, restated on
//! [`cb_http_get_fn`] where cbindgen carries it into the header: real
//! statuses (a 401 is a response, not a failure), redirects followed,
//! headers sent verbatim, no retries of its own.

use std::ffi::{c_char, c_void};

use crate::config::cb_config;
use crate::error::{cb_status, fail, guard};

/// One request header, borrowed for the duration of the callback.
#[repr(C)]
pub struct cb_http_header {
    /// NUL-terminated, valid for the call only.
    pub name: *const c_char,
    /// NUL-terminated, valid for the call only.
    pub value: *const c_char,
}

/// One outgoing GET — the only method catalog browsing needs.
///
/// Everything in it is borrowed and dies when the callback returns; a
/// host that fetches asynchronously must copy first.
#[repr(C)]
pub struct cb_http_request {
    /// NUL-terminated, absolute, `http://` or `https://`.
    pub url: *const c_char,
    /// `header_count` entries, or null when there are none. Send them
    /// exactly as given: `Accept` carries one media type and no q-value
    /// because real catalog servers negotiate by naive substring match,
    /// and a transport that rewrites or merges headers breaks them.
    pub headers: *const cb_http_header,
    pub header_count: usize,
}

/// The response under construction. Opaque; the engine hands one to the
/// callback, the callback feeds it through `cb_http_response_*`, and it
/// is dead when the callback returns.
#[derive(Default)]
pub struct cb_http_response {
    pub(crate) status: Option<u16>,
    pub(crate) content_type: Option<String>,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Vec<u8>,
    pub(crate) error: Option<String>,
}

/// Performs one blocking GET.
///
/// Before returning, the callback must either report a response —
/// [`cb_http_response_set_status`], then optionally
/// [`cb_http_response_set_content_type`] and
/// [`cb_http_response_append_body`] — or report that the request never
/// produced one with [`cb_http_response_fail`]. Returning with neither is
/// reported to the reader as a transport bug, by name.
///
/// The contract is `HttpClient`'s, and each rule exists because a real
/// catalog server depends on it:
///
/// - **4xx and 5xx are responses, not failures.** A 401's body is the
///   OPDS Authentication Document — what lets a shell put up a native
///   login instead of a generic error. `cb_http_response_fail` is only
///   for a request that produced no response at all.
/// - **Follow redirects, including cross-host ones** — catalogs relocate
///   acquisitions onto CDNs.
/// - **Send the given headers unaltered.**
/// - **Do not retry.** Auth retry is the engine's flow, one level up.
///
/// **It may fire on any thread**, including the engine's loader thread,
/// and must block until the transfer settles. It must not call back into
/// this ABI — the `cb_http_response_*` builders being the stated
/// exception, made from inside the callback on the response it was
/// handed.
pub type cb_http_get_fn = Option<
    unsafe extern "C" fn(
        request: *const cb_http_request,
        response: *mut cb_http_response,
        user: *mut c_void,
    ),
>;

/// Fetches straight to a file — optional, for a host whose own
/// fetch-to-file beats streaming through the get callback: the system
/// trust store and cookie jar, a temp file the platform already manages,
/// resume within a session. Null means the engine streams through the get
/// callback and writes the file itself.
///
/// Not background transfer. Like every callback in this ABI it must block
/// until the transfer settles, which is the one thing a transfer
/// outliving its process does not do — an iOS background `URLSession`
/// wants a delegate and refuses completion-handler tasks, and
/// `WorkManager` is a job scheduler. A download meant to survive
/// suspension is the host's to own end to end: it has an identity, it
/// reports progress, and it finishes by waking the app rather than by
/// returning. This callback cannot express any of that, and a host that
/// needs it should not reach for this.
///
/// The promise an implementation must keep: `dest` either ends up
/// complete or is not created — no partial file under the final name. On
/// a non-2xx status, report the status and write nothing. Report the
/// status with [`cb_http_response_set_status`]; the body builders are
/// ignored here, the bytes belong in `dest`.
pub type cb_http_download_fn = Option<
    unsafe extern "C" fn(
        request: *const cb_http_request,
        dest: *const c_char,
        response: *mut cb_http_response,
        user: *mut c_void,
    ),
>;

/// Performs one blocking request that is not a GET — the write half a
/// sync transport must have, because reconciling marks means POST, PUT
/// and DELETE against a Web Annotation container and a position PUT
/// against a progression service.
///
/// `method` is the verb as an uppercase token: `"POST"`, `"PUT"` or
/// `"DELETE"` — nothing else is ever sent. `body` is `body_len` bytes to
/// send, or null when the request has none (a DELETE); it dies when the
/// callback returns.
///
/// Everything [`cb_http_get_fn`] promises applies here too, plus one
/// duty of its own: **report the response headers** through
/// [`cb_http_response_add_header`], at least `ETag` and `Location` when
/// present. A Web Annotation container carries its whole concurrency
/// story in `ETag` and says where it put a new mark in `Location`; a
/// transport that discards them makes safe concurrent editing
/// impossible, and the failure looks like sync quietly forgetting marks.
pub type cb_http_send_fn = Option<
    unsafe extern "C" fn(
        method: *const c_char,
        request: *const cb_http_request,
        body: *const u8,
        body_len: usize,
        response: *mut cb_http_response,
        user: *mut c_void,
    ),
>;

/// Releases whatever `user` points at, once, when the transport is
/// dropped — the config freed unopened, or the last session holding it
/// closed. This is what lets a host hand over a reference-counted object
/// (a Swift class instance, a JNI global ref) without guessing at the
/// engine's lifetimes.
pub type cb_http_finalize_fn = Option<unsafe extern "C" fn(user: *mut c_void)>;

/// The status the server actually answered with, 4xx and 5xx included.
#[no_mangle]
pub unsafe extern "C" fn cb_http_response_set_status(
    response: *mut cb_http_response,
    status: u16,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        // SAFETY: the pointer the callback was handed, within the call.
        let Some(response) = (unsafe { response.as_mut() }) else {
            return fail(cb_status::CB_ERR_NULL_ARGUMENT, "response is null");
        };
        if status == 0 {
            return fail(
                cb_status::CB_ERR_INVALID_ARGUMENT,
                "status 0 is not an HTTP status; a request that produced no \
                 response reports through cb_http_response_fail",
            );
        }
        response.status = Some(status);
        cb_status::CB_OK
    })
}

/// The `Content-Type` header, verbatim, parameters and all. Skip the call
/// when the server sent none. This value is authoritative over whatever
/// the request asked for or a catalog link advertised.
#[no_mangle]
pub unsafe extern "C" fn cb_http_response_set_content_type(
    response: *mut cb_http_response,
    content_type: *const c_char,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        // SAFETY: the pointer the callback was handed, within the call.
        let Some(response) = (unsafe { response.as_mut() }) else {
            return fail(cb_status::CB_ERR_NULL_ARGUMENT, "response is null");
        };
        // SAFETY: the header's contract.
        let Some(content_type) = (unsafe { crate::abi::str_in(content_type, "content_type") })
        else {
            return cb_status::CB_ERR_NULL_ARGUMENT;
        };
        response.content_type = Some(content_type.to_string());
        cb_status::CB_OK
    })
}

/// Report one response header, name and value as received. Call once per
/// header, duplicates in order. `Content-Type` still goes through
/// [`cb_http_response_set_content_type`], which stays authoritative;
/// everything else — `ETag` and `Location` above all, see
/// [`cb_http_send_fn`] — arrives here. A transport may pass all headers
/// or only the ones it can cheaply enumerate.
#[no_mangle]
pub unsafe extern "C" fn cb_http_response_add_header(
    response: *mut cb_http_response,
    name: *const c_char,
    value: *const c_char,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        // SAFETY: the pointer the callback was handed, within the call.
        let Some(response) = (unsafe { response.as_mut() }) else {
            return fail(cb_status::CB_ERR_NULL_ARGUMENT, "response is null");
        };
        // SAFETY: the header's contract for both strings.
        let (Some(name), Some(value)) = (unsafe { crate::abi::str_in(name, "name") }, unsafe {
            crate::abi::str_in(value, "value")
        }) else {
            return cb_status::CB_ERR_NULL_ARGUMENT;
        };
        response.headers.push((name.to_string(), value.to_string()));
        cb_status::CB_OK
    })
}

/// Append body bytes, in order; call as many times as chunks arrive. The
/// bytes are copied.
#[no_mangle]
pub unsafe extern "C" fn cb_http_response_append_body(
    response: *mut cb_http_response,
    bytes: *const u8,
    len: usize,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        // SAFETY: the pointer the callback was handed, within the call.
        let Some(response) = (unsafe { response.as_mut() }) else {
            return fail(cb_status::CB_ERR_NULL_ARGUMENT, "response is null");
        };
        if len == 0 {
            return cb_status::CB_OK;
        }
        if bytes.is_null() {
            return fail(cb_status::CB_ERR_NULL_ARGUMENT, "bytes is null");
        }
        // SAFETY: the header's contract — `len` readable bytes at `bytes`,
        // valid for the duration of this call.
        response
            .body
            .extend_from_slice(unsafe { std::slice::from_raw_parts(bytes, len) });
        cb_status::CB_OK
    })
}

/// The request never produced a response: DNS failed, the connection
/// dropped, TLS would not verify. `message` reaches the reader through
/// the open error, so make it a sentence — the platform error's own
/// description is usually right.
///
/// Not for 4xx or 5xx, which are responses; see [`cb_http_get_fn`].
#[no_mangle]
pub unsafe extern "C" fn cb_http_response_fail(
    response: *mut cb_http_response,
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
        response.error = Some(message.to_string());
        cb_status::CB_OK
    })
}

/// Fetch through the host's networking instead of the bundled transport.
///
/// `get` is required; `download` and `finalize` may be null. `user` is
/// handed back to every callback untouched. **The transport owns `user`
/// from this call on**: `finalize` runs exactly once — when the config is
/// freed unopened, when the last session holding the transport closes,
/// or before this call returns a failure — so a host can hand over a
/// retained object and forget it.
///
/// The callbacks may fire on any thread and two may be in flight at once
/// (a streamed comic fetches pages while the shell fetches a cover), so
/// what `user` points at must tolerate both.
///
/// They have all finished by the time [`cb_session_close`] returns, which
/// is what makes "forget it" safe: a host may free whatever `user` pointed
/// at as soon as `finalize` runs, and never has to wonder whether a
/// loader thread is still inside a callback.
///
/// In a build without OPDS (`cb_capabilities()` lacks `CB_CAP_OPDS`)
/// there is nothing to fetch and this reports
/// `CB_ERR_FORMAT_NOT_BUILT` — after running `finalize`, keeping the
/// ownership rule true.
#[no_mangle]
pub unsafe extern "C" fn cb_config_set_http_transport(
    config: *mut cb_config,
    get: cb_http_get_fn,
    download: cb_http_download_fn,
    finalize: cb_http_finalize_fn,
    user: *mut c_void,
) -> cb_status {
    guard(cb_status::CB_ERR_PANIC, || {
        // Failing before taking ownership would split the contract in two;
        // run the finalizer on every path that does not store it.
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
                "get callback is null; a transport that cannot fetch is not one",
            );
        };
        #[cfg(feature = "opds")]
        {
            config.inner.transport = Some(std::sync::Arc::new(host::HostTransport {
                get,
                download,
                finalize,
                user: user as usize,
            }));
            cb_status::CB_OK
        }
        #[cfg(not(feature = "opds"))]
        {
            let (_, _, _) = (config, get, download);
            decline(
                cb_status::CB_ERR_FORMAT_NOT_BUILT,
                "this build has no OPDS support, so a transport would have \
                 nothing to carry",
            )
        }
    })
}

/// The `HttpClient` the callbacks become. Only meaningful with `opds` —
/// without it `SessionConfig` has no transport field and the entry point
/// above declines honestly. `pub(crate)` because the sync module builds
/// its transport out of the same parts.
#[cfg(feature = "opds")]
pub(crate) mod host {
    use std::ffi::{c_void, CString};
    use std::io::Cursor;
    use std::path::Path;

    use chapbook_reader::{HttpClient, HttpError, HttpRequest, HttpResponse};

    use super::{cb_http_download_fn, cb_http_finalize_fn, cb_http_header, cb_http_request};

    pub(crate) struct HostTransport {
        pub get:
            unsafe extern "C" fn(*const cb_http_request, *mut super::cb_http_response, *mut c_void),
        pub download: cb_http_download_fn,
        pub finalize: cb_http_finalize_fn,
        /// The host's pointer, stored as an address so the compiler does
        /// not have to take our word for `Send + Sync` field by field.
        /// The ABI contract carries the real requirement: the callbacks
        /// may fire on any thread, concurrently.
        pub user: usize,
    }

    impl HostTransport {
        pub(crate) fn user(&self) -> *mut c_void {
            self.user as *mut c_void
        }
    }

    impl Drop for HostTransport {
        fn drop(&mut self) {
            if let Some(finalize) = self.finalize {
                // SAFETY: the host's own finalizer with the host's own
                // pointer, called exactly once — nothing else ever calls it.
                unsafe { finalize(self.user()) };
            }
        }
    }

    /// Marshal one request into C shapes that live exactly as long as the
    /// callback invocation `f` makes.
    pub(crate) fn with_c_request<T>(
        request: &HttpRequest,
        f: impl FnOnce(*const cb_http_request) -> T,
    ) -> Result<T, HttpError> {
        let no_nul = |what: &str| HttpError::new(format!("{what} contains an interior NUL"));
        let url = CString::new(request.url.as_str()).map_err(|_| no_nul("url"))?;
        // The CStrings own the bytes the header pointers borrow; moving a
        // CString into the vec does not move its heap buffer.
        let mut owned: Vec<(CString, CString)> = Vec::with_capacity(request.headers.len());
        for (name, value) in &request.headers {
            owned.push((
                CString::new(name.as_str()).map_err(|_| no_nul("a header name"))?,
                CString::new(value.as_str()).map_err(|_| no_nul("a header value"))?,
            ));
        }
        let headers: Vec<cb_http_header> = owned
            .iter()
            .map(|(name, value)| cb_http_header {
                name: name.as_ptr(),
                value: value.as_ptr(),
            })
            .collect();
        let raw = cb_http_request {
            url: url.as_ptr(),
            headers: if headers.is_empty() {
                std::ptr::null()
            } else {
                headers.as_ptr()
            },
            header_count: headers.len(),
        };
        Ok(f(&raw))
    }

    impl super::cb_http_response {
        /// What the callback left behind, judged: a failure message wins,
        /// then a response, and silence is reported as the transport bug
        /// it is rather than surfacing as a parse error on an empty body.
        pub(crate) fn settle(self) -> Result<HttpResponse, HttpError> {
            if let Some(message) = self.error {
                return Err(HttpError::new(message));
            }
            match self.status {
                Some(status) => Ok(HttpResponse {
                    status,
                    content_type: self.content_type,
                    headers: self.headers,
                    body: Box::new(Cursor::new(self.body)),
                }),
                None => Err(HttpError::new(
                    "the host transport returned without reporting a status or a failure",
                )),
            }
        }
    }

    impl HttpClient for HostTransport {
        fn get(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
            let mut response = super::cb_http_response::default();
            with_c_request(&request, |raw| {
                // SAFETY: the callback the host installed, with pointers
                // valid for exactly this call.
                unsafe { (self.get)(raw, &mut response, self.user()) }
            })?;
            response.settle()
        }

        fn download(&self, request: HttpRequest, dest: &Path) -> Result<u16, HttpError> {
            let Some(download) = self.download else {
                // No host facility: the trait's own default — get, sibling
                // temp file, rename — reached through a shim that only
                // knows `get`, so the logic lives in one place.
                struct ViaGet<'a>(&'a HostTransport);
                impl HttpClient for ViaGet<'_> {
                    fn get(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
                        self.0.get(request)
                    }
                }
                return ViaGet(self).download(request, dest);
            };
            let dest: &str = dest
                .to_str()
                .ok_or_else(|| HttpError::new("destination path is not UTF-8"))?;
            let dest =
                CString::new(dest).map_err(|_| HttpError::new("destination path has a NUL"))?;
            let mut response = super::cb_http_response::default();
            with_c_request(&request, |raw| {
                // SAFETY: as for `get`.
                unsafe { download(raw, dest.as_ptr(), &mut response, self.user()) }
            })?;
            Ok(response.settle()?.status)
        }
    }
}
