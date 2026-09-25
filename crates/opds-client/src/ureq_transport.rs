//! The `ureq` implementation of [`HttpClient`], behind the default `ureq`
//! feature.
//!
//! This is the desktop answer and nothing more. It brings `ureq`, `rustls`
//! and a bundled root store with it; a caller that wants the host's
//! networking instead turns the feature off and passes its own transport,
//! and nothing else in this crate changes.
//!
//! It is also nearly nothing: `ureq` 3 speaks the `http` crate's types
//! natively, so the request goes in as it is and the response comes out
//! with only its body re-boxed.
//!
//! (The module is `ureq_transport`, not `ureq`, because a crate-root
//! `mod ureq` would make `use ureq::...` ambiguous with the extern crate.)

use crate::http::{Body, HttpClient, HttpError, HttpRequest, HttpResponse};

/// [`HttpClient`] over a blocking `ureq` agent.
pub struct UreqHttp {
    agent: ureq::Agent,
}

impl Default for UreqHttp {
    fn default() -> Self {
        Self::new()
    }
}

impl UreqHttp {
    pub fn new() -> Self {
        let config = ureq::Agent::config_builder()
            // Hand 4xx/5xx back as responses: the trait requires it, and a
            // 401 body is the Authentication Document.
            .http_status_as_error(false)
            .build();
        UreqHttp {
            agent: config.into(),
        }
    }

    /// Wrap an agent the caller configured — proxy, timeouts, a different
    /// TLS backend or root store. `http_status_as_error(false)` is required
    /// of it, per [`HttpClient`]'s contract.
    pub fn with_agent(agent: ureq::Agent) -> Self {
        UreqHttp { agent }
    }
}

impl HttpClient for UreqHttp {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        // An empty body is no body: a GET must not go out with
        // `Content-Length: 0`, which some servers answer with 400.
        let response = if request.body().is_empty() {
            self.agent.run(request.map(|_| ()))
        } else {
            self.agent.run(request)
        }
        .map_err(HttpError::new)?;
        // `into_reader()` rather than a borrowed reader: the body outlives
        // this function inside the response.
        Ok(response.map(|body| Box::new(body.into_reader()) as Body))
    }
}
