//! The reconcile loop, and the rules about who wins.
//!
//! # Who wins
//!
//! Push is easy and is not really our decision: both protocols arbitrate
//! it themselves. A progression service refuses a stale point with 409,
//! and a container refuses a stale edit with 412 on the entity tag. So a
//! push offers what it has and reports what the service said; it never
//! compares timestamps to decide whether to try.
//!
//! Pull is where a rule is needed, and it is the conservative one: **a
//! remote position is adopted only when the local one is clean.** If the
//! local position still owes the service a write, the two have diverged
//! independently and overwriting would throw away where this reader
//! actually is — so it is reported as a conflict and left alone. Nothing
//! here silently picks a winner between two readers.
//!
//! # What a pulled position is
//!
//! Not a locator this device captured, and it must not pretend to be one.
//! A [`RemotePosition`](chapbook_opds::progression::RemotePosition) has a
//! fraction, and — when the peer sent a text fragment — a spine item and a
//! quote. It has no `char_offset`, because that never leaves the device
//! that took it.
//!
//! So it is stored as a `LayeredLocator` with `locator_version` **0**,
//! which is not a version any extraction ever produced. `resolve_in_text`
//! trusts `char_offset` only when the version matches the current build,
//! so a zero falls straight through to the quote layer and then the
//! fraction — which is exactly the resolve chain docs/LOCATORS.md
//! describes, reached by saying the honest thing about the offset rather
//! than by adding a code path.

use chapbook_annotations::{
    from_annotation, to_annotation, Annotation, AnnotationContainer, ContainerError, Mark,
};
use chapbook_core::{LayeredLocator, Quote};
use chapbook_library::{AnnotationSync, BookId, Library, PositionPush, SyncTargets};
use chapbook_opds::progression::{
    from_progression, to_progression, Device, ProgressionUpdate, RefusalReason,
};
use chapbook_opds::{HttpClient, OpdsClient, OpdsError};

/// What went wrong at a level the caller has to act on. A service that
/// declined is not here — that is an outcome, and lives in the reports.
#[derive(Debug)]
pub enum SyncError {
    Library(String),
    Progression(String),
    Container(String),
    /// The book has no services to talk to, is not in the library, or has
    /// been removed from the shelf — three ways of having nothing to
    /// reconcile, and none of them a failure of this run.
    NotSyncable(BookId),
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SyncError::Library(message) => write!(f, "library: {message}"),
            SyncError::Progression(message) => write!(f, "progression service: {message}"),
            SyncError::Container(message) => write!(f, "annotation container: {message}"),
            SyncError::NotSyncable(book) => {
                write!(f, "book {} has no service to sync with", book.0)
            }
        }
    }
}

impl std::error::Error for SyncError {}

impl From<chapbook_core::ChapbookError> for SyncError {
    fn from(e: chapbook_core::ChapbookError) -> Self {
        SyncError::Library(e.to_string())
    }
}

/// What happened to a book's position.
#[derive(Debug, Clone, PartialEq)]
pub enum PositionReport {
    /// Nothing to do: no service, or nothing had changed on either side.
    Idle,
    /// This device's position reached the service.
    Pushed,
    /// The service declined; what it holds is newer. The next pull will
    /// bring it down, if the local copy is clean by then.
    Refused(String),
    /// The service's position was adopted locally.
    Pulled,
    /// Both sides moved since they last agreed. Nothing was overwritten.
    Conflict,
    /// The service could not be reached, or answered something we could
    /// not use. Nothing local changed.
    Failed(String),
}

/// What happened to a book's marks.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AnnotationReport {
    pub created: usize,
    pub updated: usize,
    pub deleted: usize,
    /// Pulled from the container as marks this device had not seen.
    pub adopted: usize,
    /// Marks this device already had, brought up to date with what another
    /// device wrote. Distinct from `adopted`: nothing new arrived, an
    /// existing mark now says something else.
    pub refreshed: usize,
    /// Marks another device deleted, taken off this shelf to match.
    /// Distinct from `deleted`, which is this device's own deletions
    /// reaching the container.
    pub withdrawn: usize,
    /// The container had more pages than the walk was allowed. The marks
    /// reported are a prefix of it, and no deletion was inferred, because
    /// absence from a listing that stopped early is not absence.
    pub truncated: bool,
    /// Conflicts settled by re-reading the container and writing again —
    /// see [`SyncEngine::merge_edit`] for what "settled" costs each side.
    pub merged: usize,
    /// Edits still outstanding after a merge was attempted: a third write
    /// landed between the re-read and the retry. Nothing was overwritten;
    /// the local edit still owes a write, and the next pass tries again.
    pub conflicts: usize,
    /// The container could not be reached. Whatever had already been
    /// pushed when it failed stands; the rest still owes a write.
    pub failed: Option<String>,
}

/// What one book's reconcile came to: a report, or the reason there is
/// none. Named because a batch is a list of them.
pub type SyncResult = Result<BookReport, SyncError>;

/// One book's reconcile, end to end.
#[derive(Debug, Clone, PartialEq)]
pub struct BookReport {
    pub book: BookId,
    pub position: PositionReport,
    pub annotations: AnnotationReport,
}

/// The reconcile loop over one library.
pub struct SyncEngine {
    library: Library,
    catalog: OpdsClient,
    container: AnnotationContainer,
    device: Device,
}

impl SyncEngine {
    /// `device` is the host's, minted once and kept — see
    /// [`Device`]'s docs. This crate neither generates nor persists one.
    ///
    /// One transport serves both protocols, which is the point of it being
    /// `Arc`: a host owns one of these.
    pub fn new(
        library: Library,
        http: std::sync::Arc<dyn HttpClient>,
        device: Device,
    ) -> SyncEngine {
        SyncEngine {
            library,
            catalog: OpdsClient::new(http.clone()),
            container: AnnotationContainer::new(http),
            device,
        }
    }

    /// The opaque `Authorization` both services are reached with.
    ///
    /// One value, because a book's progression service and annotation
    /// container come off the same catalog entry and so from the same
    /// origin. A shell whose services need different credentials wants two
    /// engines, which is cheap.
    pub fn set_authorization(&mut self, value: impl Into<String>) {
        let value = value.into();
        self.catalog.set_authorization(value.clone());
        self.container.set_authorization(value);
    }

    /// Drop the credential, so the next book's services are reached
    /// with none — what a driver does between books whose origins differ.
    pub fn clear_authorization(&mut self) {
        self.catalog.clear_authorization();
        self.container.clear_authorization();
    }

    /// The library this engine owns, for a caller that wants to look
    /// something up on this thread rather than open a third connection.
    pub fn library(&self) -> &Library {
        &self.library
    }

    /// Reconcile one book: position first, then marks.
    pub fn sync_book(&mut self, book: BookId) -> Result<BookReport, SyncError> {
        // A book that is not on the shelf does not sync, whichever door
        // asked. [`Self::sync_all`] never offers one — its query filters
        // removed books — and a caller naming an id directly has to get the
        // same answer, or the two doors disagree about what the library
        // holds. `book` reports `None` for a removed row as well as an
        // absent one, which is exactly the distinction that matters here.
        if self.library.book(book)?.is_none() {
            return Err(SyncError::NotSyncable(book));
        }
        let targets = self.library.sync_targets(book)?;
        if targets.progression_url.is_none() && targets.annotation_container.is_none() {
            return Err(SyncError::NotSyncable(book));
        }
        // The two halves are independent services, possibly on different
        // hosts. A progression service that is down must not stop a mark
        // from reaching its container, so each reports its own outcome and
        // only a library failure — a broken database, not a broken network
        // — aborts the book.
        let position = match self.sync_position(book, &targets) {
            Ok(report) => report,
            Err(SyncError::Library(message)) => return Err(SyncError::Library(message)),
            Err(e) => PositionReport::Failed(e.to_string()),
        };
        let annotations = match self.sync_annotations(book, &targets) {
            Ok(report) => report,
            Err(SyncError::Library(message)) => return Err(SyncError::Library(message)),
            Err(e) => AnnotationReport {
                failed: Some(e.to_string()),
                ..AnnotationReport::default()
            },
        };
        Ok(BookReport {
            book,
            position,
            annotations,
        })
    }

    /// Every book with a service to talk to.
    ///
    /// A book whose sync fails does not stop the others: the error is
    /// returned beside its book and the loop goes on, because one dead
    /// host should not leave the rest of a shelf unsynced.
    pub fn sync_all(&mut self) -> Result<Vec<(BookId, SyncResult)>, SyncError> {
        let books = self.library.books_with_sync_targets()?;
        Ok(books
            .into_iter()
            .map(|book| (book, self.sync_book(book)))
            .collect())
    }

    // ---- position ----

    fn sync_position(
        &mut self,
        book: BookId,
        targets: &SyncTargets,
    ) -> Result<PositionReport, SyncError> {
        let Some(url) = targets.progression_url.as_deref() else {
            return Ok(PositionReport::Idle);
        };

        // Push first. The service arbitrates: if what we hold is older it
        // says so, and the pull below brings its copy down.
        let pending = self
            .library
            .positions_needing_push()?
            .into_iter()
            .find(|p| p.book == book);
        if let Some(push) = pending {
            return self.push_position(book, url, push);
        }

        self.pull_position(book, url, targets)
    }

    fn push_position(
        &mut self,
        book: BookId,
        url: &str,
        push: PositionPush,
    ) -> Result<PositionReport, SyncError> {
        let Some(stored) = self.library.position(book)? else {
            return Ok(PositionReport::Idle);
        };
        let title = self
            .library
            .book(book)?
            .map(|record| record.title)
            .filter(|title| !title.is_empty());
        let document = to_progression(
            &stored.locator,
            self.device.clone(),
            iso8601(stored.updated_at),
            title,
        );
        match self.catalog.put_progression(url, &document) {
            Ok(ProgressionUpdate::Stored(_)) | Ok(ProgressionUpdate::Created(_)) => {
                self.library
                    .mark_position_synced(book, push.revision, &document.modified)?;
                Ok(PositionReport::Pushed)
            }
            Ok(ProgressionUpdate::Refused(refusal)) => {
                // A stale point is the ordinary outcome of two devices, not
                // a failure. Leave the local position dirty: it still owes
                // a write, and a later push may win.
                let reason = refusal
                    .title
                    .clone()
                    .unwrap_or_else(|| format!("{:?}", refusal.reason));
                if refusal.reason == RefusalReason::Stale {
                    return Ok(PositionReport::Refused(reason));
                }
                Ok(PositionReport::Refused(reason))
            }
            Err(e) => Err(SyncError::Progression(describe(e))),
        }
    }

    fn pull_position(
        &mut self,
        book: BookId,
        url: &str,
        targets: &SyncTargets,
    ) -> Result<PositionReport, SyncError> {
        let remote = match self.catalog.fetch_progression(url) {
            Ok(Some(remote)) => remote,
            // 200 with an empty body: nothing recorded yet.
            Ok(None) => return Ok(PositionReport::Idle),
            Err(e) => return Err(SyncError::Progression(describe(e))),
        };
        // Equality, never ordering — the string is compared as it arrived.
        if targets.remote_modified.as_deref() == Some(remote.modified.as_str()) {
            return Ok(PositionReport::Idle);
        }
        if self.library.position_needs_push(book)? {
            // Both moved since they last agreed. Overwriting would throw
            // away where this reader is.
            //
            // A guard rather than the ordinary path, and worth saying which:
            // `sync_position` pushes first whenever the local position is
            // dirty, so a genuine two-sided disagreement is answered by the
            // service and comes back as `Refused`, not from here. This fires
            // only if the two dirty predicates ever disagree —
            // `positions_needing_push` filters removed books and
            // `position_needs_push` does not, which is why `sync_book`
            // refuses a removed book before either is asked.
            return Ok(PositionReport::Conflict);
        }

        let position = from_progression(&remote);
        let locator = degraded_locator(
            position.spine_href.unwrap_or_default(),
            position.quote.unwrap_or_default(),
            position.progression,
        );
        self.library.set_position(book, &locator)?;
        // Adopted, so it agrees with the service by definition — stamp the
        // revision the adoption just produced.
        let revision = self
            .library
            .positions_needing_push()?
            .into_iter()
            .find(|p| p.book == book)
            .map(|p| p.revision)
            .unwrap_or(0);
        self.library
            .mark_position_synced(book, revision, &remote.modified)?;
        Ok(PositionReport::Pulled)
    }

    // ---- annotations ----

    fn sync_annotations(
        &mut self,
        book: BookId,
        targets: &SyncTargets,
    ) -> Result<AnnotationReport, SyncError> {
        let Some(container_url) = targets.annotation_container.as_deref() else {
            return Ok(AnnotationReport::default());
        };
        let source = self.source_iri(book, container_url)?;
        let mut report = AnnotationReport::default();

        // What the container was known to hold before this pass wrote
        // anything. Only these can be marks it has since dropped: one
        // created a moment ago and not yet listed is a container that has
        // not caught up, not a container that deleted it, and withdrawing
        // on that evidence would throw away a mark this device had just
        // made.
        let known_before = self.library.synced_annotations(book)?;
        // IRIs this pass wrote to, or tried to. A container that answered
        // a write is a container that still has the mark, whatever its
        // listing gets round to saying — so this outranks absence.
        let mut touched: std::collections::HashSet<String> = std::collections::HashSet::new();

        for pending in self.library.annotations_needing_push(book)? {
            match (pending.deleted, pending.remote_iri.clone()) {
                // Deleted here and known there: tell the container, then
                // let the row go.
                (true, Some(iri)) => {
                    touched.insert(iri.clone());
                    match self.container.delete(&iri, pending.remote_etag.as_deref()) {
                        Ok(()) | Err(ContainerError::Gone) => {
                            self.library.purge_annotation(pending.id)?;
                            report.deleted += 1;
                        }
                        Err(ContainerError::Conflict { .. }) => {
                            if self.merge_delete(&iri, &pending)? {
                                report.deleted += 1;
                                report.merged += 1;
                            } else {
                                report.conflicts += 1;
                            }
                        }
                        Err(e) => return Err(SyncError::Container(e.to_string())),
                    }
                }
                // Deleted here and never pushed: nothing owes anything.
                (true, None) => {
                    self.library.purge_annotation(pending.id)?;
                }
                (false, remote_iri) => {
                    let Some(mark) = self.mark_of(book, pending.id)? else {
                        continue;
                    };
                    let document = to_annotation(&mark, &source);
                    let result = match &remote_iri {
                        Some(iri) => {
                            touched.insert(iri.clone());
                            self.container
                                .update(iri, &document, pending.remote_etag.as_deref())
                        }
                        None => self.container.create(container_url, &document),
                    };
                    match result {
                        Ok(stored) => {
                            self.library.mark_annotation_synced(
                                pending.id,
                                pending.revision,
                                &stored.iri,
                                stored.etag.as_deref(),
                            )?;
                            if remote_iri.is_some() {
                                report.updated += 1;
                            } else {
                                report.created += 1;
                            }
                        }
                        // Its copy moved. Settle it rather than counting it
                        // — the refusal is the container telling us to look
                        // again, and it hands back its copy for exactly that.
                        Err(ContainerError::Conflict { .. }) => {
                            let iri = remote_iri
                                .as_deref()
                                .expect("only a write to a known IRI can be refused");
                            match self.merge_edit(book, iri, &pending, &mark, &document)? {
                                // Nothing went over the wire, so nothing is
                                // counted as written — only as settled.
                                EditMerge::Agreed | EditMerge::Vanished => report.merged += 1,
                                EditMerge::Rewrote => {
                                    report.updated += 1;
                                    report.merged += 1;
                                }
                                EditMerge::StillRefused => report.conflicts += 1,
                            }
                        }
                        // Deleted out from under us: the local row is
                        // pointing at an IRI that will never exist again,
                        // so let it go rather than retry forever.
                        Err(ContainerError::Gone) => {
                            self.library.delete_annotation(pending.id)?;
                            self.library.purge_annotation(pending.id)?;
                        }
                        Err(e) => return Err(SyncError::Container(e.to_string())),
                    }
                }
            }
        }

        // What the push half could not settle, read once: the pull walk
        // must not adopt over an edit that still owes a write, and the set
        // changes as the walk adds rows.
        let owed = self.library.annotations_needing_push(book)?;

        // Pull: anything in the container this device has not seen, and
        // any change to what it has.
        let listing = self
            .container
            .all(container_url, Some(MAX_CONTAINER_PAGES))
            .map_err(|e| SyncError::Container(e.to_string()))?;
        report.truncated = !listing.complete;
        // Every IRI the container still has for this book. What a complete
        // listing does *not* mention, another device deleted.
        let mut present = std::collections::HashSet::new();
        for stored in listing.items {
            // A container is a container: the Web Annotation Protocol
            // defines no way to ask one for "the annotations on this
            // book", so what comes back is everything in it, for every
            // publication this reader has ever marked. Filtering on the
            // target is the client's job and cannot be skipped — adopting
            // unfiltered would file another book's highlights against this
            // one, and the next push would send them back anchored to it.
            //
            // A catalog may advertise the container with a `?target=`
            // query. That is not a filter: nothing in the protocol makes
            // it one, and a server is free to ignore it — which the ones
            // this was tested against do.
            if stored.annotation.target.source != source {
                continue;
            }
            present.insert(stored.iri.clone());
            let mark = from_annotation(&stored.annotation);

            // Known here already: this is an edit another device made, not
            // a mark arriving. Skipping these is what made a container's
            // changes invisible until this device happened to write into
            // one and be refused.
            if let Some(id) = self.library.annotation_by_remote_iri(&stored.iri)? {
                // Deleted here — including a delete the container has not
                // been told about yet. A pull must not put it back.
                let Some(ours) = self.mark_of(book, id)? else {
                    continue;
                };
                if same_content(&mark, &ours) {
                    continue;
                }
                // Ours still owes a write, so both moved. The push half
                // already had its say about this row *and* already counted
                // it; adopting now would drop the local edit and counting
                // again would report one disagreement as two.
                if owed.iter().any(|pending| pending.id == id) {
                    continue;
                }
                self.library.update_annotation(
                    id,
                    mark.kind,
                    &mark.start,
                    mark.end.as_ref(),
                    mark.text.as_deref(),
                    mark.color.as_deref(),
                )?;
                let revision = self
                    .library
                    .annotations_needing_push(book)?
                    .into_iter()
                    .find(|a| a.id == id)
                    .map(|a| a.revision)
                    .unwrap_or(0);
                self.library.mark_annotation_synced(
                    id,
                    revision,
                    &stored.iri,
                    stored.etag.as_deref(),
                )?;
                report.refreshed += 1;
                continue;
            }

            let id = self.library.add_annotation(
                book,
                mark.kind,
                &mark.start,
                mark.end.as_ref(),
                mark.text.as_deref(),
                mark.color.as_deref(),
            )?;
            let revision = self
                .library
                .annotations_needing_push(book)?
                .into_iter()
                .find(|a| a.id == id)
                .map(|a| a.revision)
                .unwrap_or(0);
            self.library.mark_annotation_synced(
                id,
                revision,
                &stored.iri,
                stored.etag.as_deref(),
            )?;
            report.adopted += 1;
        }

        // Deletions the other direction. Only from a listing that reached
        // the end: a walk stopped by the page cap has seen a prefix, and
        // treating what it missed as deleted would take a reader's
        // highlights away on the strength of a container being large.
        if listing.complete {
            for (id, iri) in known_before {
                if present.contains(&iri) || touched.contains(&iri) {
                    continue;
                }
                // The same pair the push half uses for a tombstone: soft
                // first so the row is purgeable, then gone. Nothing is owed
                // to a container that has already dropped it.
                self.library.delete_annotation(id)?;
                self.library.purge_annotation(id)?;
                report.withdrawn += 1;
            }
        }

        Ok(report)
    }

    /// What a mark anchors into. The container is per-publication, so the
    /// book's own identifier is the natural source IRI; the container URL
    /// is the fallback for a book with none, which at least names one
    /// publication uniquely.
    fn source_iri(&self, book: BookId, container_url: &str) -> Result<String, SyncError> {
        Ok(self
            .library
            .book(book)?
            .and_then(|record| record.identifier)
            .unwrap_or_else(|| container_url.to_string()))
    }

    /// Answer a refused edit without discarding either side.
    ///
    /// A 412 says the container's copy moved since this device last read
    /// it. The refusal carries that copy but no entity tag to write
    /// against, so settling one always costs a re-read — and what the read
    /// finds decides the rest.
    ///
    /// Same content: two devices made the same edit, there is nothing to
    /// choose, and the row is simply marked as agreeing with the container.
    ///
    /// Different content: both are words a reader typed, and neither comes
    /// back once dropped, so both survive. The container's copy is filed
    /// here as a mark of its own — it pushes as a create on the next pass,
    /// which is what puts it back within reach of the device that wrote it
    /// — and this device's edit keeps the IRI. The reader ends up with two
    /// marks over the same words, which is visible and reversible. A silent
    /// overwrite is neither.
    ///
    /// Returns whether it settled. One retry is a merge; a refusal on the
    /// retry means a third write landed in between, and looping on that is
    /// a fight rather than a reconcile.
    fn merge_edit(
        &mut self,
        book: BookId,
        iri: &str,
        pending: &AnnotationSync,
        ours: &Mark,
        document: &Annotation,
    ) -> Result<EditMerge, SyncError> {
        let stored = match self.container.get(iri) {
            Ok(stored) => stored,
            // Gone between the refusal and the re-read. The row points at
            // an IRI that will never be minted again, so let it go — the
            // same answer the write path already gives a tombstone.
            Err(ContainerError::Gone) => {
                self.library.delete_annotation(pending.id)?;
                self.library.purge_annotation(pending.id)?;
                return Ok(EditMerge::Vanished);
            }
            Err(e) => return Err(SyncError::Container(e.to_string())),
        };

        let theirs = from_annotation(&stored.annotation);
        if same_content(&theirs, ours) {
            self.library.mark_annotation_synced(
                pending.id,
                pending.revision,
                &stored.iri,
                stored.etag.as_deref(),
            )?;
            return Ok(EditMerge::Agreed);
        }

        self.library.add_annotation(
            book,
            theirs.kind,
            &theirs.start,
            theirs.end.as_ref(),
            theirs.text.as_deref(),
            theirs.color.as_deref(),
        )?;

        match self.container.update(iri, document, stored.etag.as_deref()) {
            Ok(written) => {
                self.library.mark_annotation_synced(
                    pending.id,
                    pending.revision,
                    &written.iri,
                    written.etag.as_deref(),
                )?;
                Ok(EditMerge::Rewrote)
            }
            Err(ContainerError::Conflict { .. }) => Ok(EditMerge::StillRefused),
            Err(e) => Err(SyncError::Container(e.to_string())),
        }
    }

    /// Answer a refused delete by re-reading for a fresh tag and deleting
    /// again.
    ///
    /// Deliberately not symmetric with [`Self::merge_edit`], which keeps
    /// both sides. Removing a mark is a reader's terminal instruction about
    /// it, and a tag that moved is not a reason to keep something they
    /// said to get rid of — the device that edited it and the device that
    /// deleted it belong to the same reader. Preserving the losing edit
    /// here would mean answering "delete this" by putting it back.
    fn merge_delete(&mut self, iri: &str, pending: &AnnotationSync) -> Result<bool, SyncError> {
        let stored = match self.container.get(iri) {
            Ok(stored) => stored,
            Err(ContainerError::Gone) => {
                self.library.purge_annotation(pending.id)?;
                return Ok(true);
            }
            Err(e) => return Err(SyncError::Container(e.to_string())),
        };
        match self.container.delete(iri, stored.etag.as_deref()) {
            Ok(()) | Err(ContainerError::Gone) => {
                self.library.purge_annotation(pending.id)?;
                Ok(true)
            }
            Err(ContainerError::Conflict { .. }) => Ok(false),
            Err(e) => Err(SyncError::Container(e.to_string())),
        }
    }

    fn mark_of(&self, book: BookId, annotation_id: i64) -> Result<Option<Mark>, SyncError> {
        Ok(self
            .library
            .annotations(book)?
            .into_iter()
            .find(|a| a.id == annotation_id)
            .map(|a| Mark {
                kind: a.kind,
                start: a.start,
                end: a.end,
                text: a.text,
                color: a.color,
                created: Some(iso8601(a.created_at)),
                modified: Some(iso8601(a.updated_at)),
            }))
    }
}

/// What settling a refused edit came to. Separate from the report because
/// only some of these put anything on the wire, and a count of writes that
/// includes the ones that did not happen is not a count of anything.
enum EditMerge {
    /// The container already held what we were going to write.
    Agreed,
    /// The two edits differed; the container's was kept here as its own
    /// mark and ours was written over it there.
    Rewrote,
    /// The IRI was gone by the time we looked again.
    Vanished,
    /// A third write landed between the re-read and the retry.
    StillRefused,
}

/// Whether two marks say the same thing.
///
/// Content only: `created` and `modified` are stamped by whoever wrote the
/// document and differ between two devices that made the identical edit,
/// which is precisely the case this exists to recognise.
fn same_content(a: &Mark, b: &Mark) -> bool {
    a.kind == b.kind
        && a.start == b.start
        && a.end == b.end
        && a.text == b.text
        && a.color == b.color
}

/// A container is somebody else's, and a `next` chain has no promised end.
const MAX_CONTAINER_PAGES: usize = 64;

/// A position that came from elsewhere, in the only shape it honestly has.
///
/// `locator_version` 0 is the load-bearing part: no extraction ever
/// produced it, so `resolve_in_text` cannot trust the offset and falls
/// through to the quote and then the fraction. The spine index is 0
/// because a peer's index is meaningless here — the href is what locates
/// the item, and `restore_position` matches on it.
fn degraded_locator(spine_href: String, quote: Quote, progression: f64) -> LayeredLocator {
    LayeredLocator {
        spine_href,
        spine_index: 0,
        char_offset: 0,
        locator_version: 0,
        quote,
        // Within-chapter position is unknown; the whole-book fraction is
        // what the peer actually told us.
        spine_fraction: progression,
        book_progression: progression,
    }
}

/// Unix seconds as the ISO 8601 both protocols write.
///
/// Hand-rolled because the workspace has no date type and this is the only
/// place that needs one — `opds_client` says as much about `modified`
/// staying a `String`. Civil-from-days is Howard Hinnant's algorithm.
fn iso8601(unix_seconds: i64) -> String {
    let days = unix_seconds.div_euclid(86_400);
    let seconds = unix_seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second) = (seconds / 3600, (seconds % 3600) / 60, seconds % 60);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (year + i64::from(month <= 2), month, day)
}

fn describe(e: OpdsError) -> String {
    match e {
        OpdsError::AuthRequired(_) => "authentication required".to_string(),
        other => other.to_string(),
    }
}
