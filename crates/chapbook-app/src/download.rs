//! What a download that the platform ran comes to.
//!
//! The transfer itself is the platform's — `WorkManager`, a background
//! `URLSession`, a thread on a desk — because a transfer that must
//! outlive its screen is a job, and only the platform has jobs. What is
//! the same everywhere is how to read the result: which statuses mean
//! the reader's login stopped working, which mean the book is gone, which
//! mean try later, and what to do with the file once it has landed
//! ([`App::land_download`](crate::App::land_download)).

/// How a transfer's HTTP status is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadOutcome {
    /// The file is the book. Land it.
    Landed,
    /// 401 or 403: the credential the job carried is wrong or gone. Not
    /// worth retrying with the same one; worth a sign-in.
    Refused,
    /// 404, 410, or any other client-side answer: the catalog no longer
    /// offers this. Retrying will not change its mind.
    Gone,
    /// A 5xx: the service is having a bad day. Retry with backoff, which
    /// is what a job system does when told.
    Again,
}

impl DownloadOutcome {
    /// Read a status the way every front end reads it. A status of zero
    /// — no response at all — is `Again`, because a transfer that never
    /// reached the service is a network condition, not a verdict.
    pub fn of_status(status: u16) -> DownloadOutcome {
        match status {
            0 => DownloadOutcome::Again,
            200..=299 => DownloadOutcome::Landed,
            401 | 403 => DownloadOutcome::Refused,
            500..=599 => DownloadOutcome::Again,
            _ => DownloadOutcome::Gone,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_read_the_same_everywhere() {
        assert_eq!(DownloadOutcome::of_status(200), DownloadOutcome::Landed);
        assert_eq!(DownloadOutcome::of_status(206), DownloadOutcome::Landed);
        assert_eq!(DownloadOutcome::of_status(401), DownloadOutcome::Refused);
        assert_eq!(DownloadOutcome::of_status(403), DownloadOutcome::Refused);
        assert_eq!(DownloadOutcome::of_status(404), DownloadOutcome::Gone);
        assert_eq!(DownloadOutcome::of_status(410), DownloadOutcome::Gone);
        assert_eq!(DownloadOutcome::of_status(418), DownloadOutcome::Gone);
        assert_eq!(DownloadOutcome::of_status(503), DownloadOutcome::Again);
        assert_eq!(DownloadOutcome::of_status(0), DownloadOutcome::Again);
    }
}
