//! Runtime-wide serialisation of clipboard publishes and the synthetic pastes
//! that follow them.
//!
//! A "paste an entry" action is two side effects on shared OS state: write the
//! entry to the system clipboard, then synthesise ⌘/Ctrl+V into whichever app
//! now holds focus. The front-ends deliberately split them — the palette hides
//! its window, re-focuses the source app, and waits `paste_delay_ms` in
//! between — and several requests can be in flight at once (the IPC server
//! handles connections concurrently, and a hotkey press can land while an IPC
//! `PasteEntry` is mid-flight).
//!
//! Interleaved, those two side effects paste the wrong content: request A
//! publishes, request B publishes over it, then A synthesises the keystroke
//! and B's clip — possibly a secret — lands in A's target app. This module
//! makes the pair one serialised operation:
//!
//! * [`ClipboardLease`] is a process-wide mutual-exclusion token. Every
//!   clipboard write in the runtime goes through it, so no second publish can
//!   interleave between a publish and its paste.
//! * [`ClipboardPublish`] identifies the publish that a paste is allowed to
//!   act on. `paste_frontmost` refuses a token that is not the lease's most
//!   recent publish, so a front-end that copies under one lease and pastes
//!   under another cannot resurrect the interleaving the lease prevents.
//! * Right before the keystroke, the lease re-reads the OS clipboard sequence
//!   and confirms it is still the app's own write. That is the only way to
//!   catch the third writer the lease cannot lock out — the user copying in
//!   another app during the hide → re-focus → delay window.
//!
//! A failed check is not a fallback-to-paste-anyway: the copy already
//! happened, so the lease stops with [`PasteFailureReason::ClipboardChanged`]
//! and the user re-copies rather than having unknown content typed into their
//! editor.
//!
//! The hold is short and bounded: one clipboard write, at most
//! `paste_delay_ms` (capped at [`nagori_core::MAX_PASTE_DELAY_MS`], 1s) of
//! focus-handoff wait, and one synthesis call. Requests that arrive inside it
//! queue rather than racing — which is the point, and no worse than the
//! per-adapter clipboard mutex they already queued on.
//!
//! **Cancellation.** Dropping the lease has to mean "no side effect of mine is
//! still in flight", and an IPC handler future *is* dropped on peer disconnect
//! or the server deadline (`run_handler_bounded`). Database reads and adapter
//! preparation stay on that caller future, so cancellation during a Windows
//! image decode cannot publish stale content later. Only a
//! [`PreparedClipboardWrite`](nagori_platform::PreparedClipboardWrite) whose
//! next step touches the OS crosses into the task that owns the guard
//! ([`ClipboardLease::guarded`]). If the caller then goes away mid-write or
//! mid-keystroke, that task keeps the lease until the OS call returns, so the
//! next request cannot publish underneath either side effect.
//!
//! **What the sequence check cannot see.** Three windows stay open by
//! construction, and the check is documented as a mitigation rather than a
//! guarantee because of them:
//!
//! 1. No OS exposes an atomic write-and-return-sequence, so an adapter records
//!    the sequence a few instructions *after* its write lands (see
//!    `nagori_platform::SelfWriteTracker`). A foreign write inside that window
//!    is recorded as ours and the later check reports `Confirmed`.
//! 2. The probe and the keystroke are separate calls. The probe runs on the
//!    same task as the synthesis, immediately before it, so the gap is one
//!    `spawn_blocking` hop — but it is not zero.
//! 3. `CGEventPost` / `SendInput` only *post* the keystroke; the target app
//!    reads the clipboard when it processes it. The lease is therefore held a
//!    further [`PASTE_CONSUMPTION_GRACE`] so a queued publish cannot overwrite
//!    the clip in that gap — which shrinks it, since nothing acknowledges the
//!    read.
//!
//! What the check buys is moving the exposure out of the whole hide → refocus →
//! delay window (up to ~1s of wall clock, entirely under the user's fingers)
//! and into windows measured in instructions and milliseconds. The first race
//! is already accepted on the capture side; closing the third needs an OS
//! primitive that does not exist.

use std::collections::HashSet;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use nagori_core::{
    AppError, ClipboardContent, ClipboardEntry, EntryId, EntryRepository,
    MAX_COMBINED_COPY_ENTRIES, PasteFailureReason, PasteFormat, Result, Sensitivity,
    SettingsRepository, StoredClipboardRepresentation, is_text_safe_for_default_output,
    select_representation,
};
use nagori_platform::{PreparedClipboardWrite, SelfWriteTracking};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard, oneshot};

use crate::capture_loop::RESYNC_GAP_THRESHOLD;
use crate::ipc_handler::result_code;

use super::{NagoriRuntime, elapsed_ms};

/// Separator between the bodies a combined copy joins.
const COMBINED_COPY_SEPARATOR: &str = "\n";

/// How long the lease is held after the synthesis call returns.
///
/// `CGEventPost` / `SendInput` post the keystroke into the OS input stream;
/// the target app reads the clipboard whenever it processes that keystroke,
/// which is after the call returns. 60ms is the same settle the palette already
/// waits on the other side of the handoff (the empirical macOS value the Maccy
/// / Paste community reports, and enough for the Windows
/// `SetForegroundWindow` → IME path).
const PASTE_CONSUMPTION_GRACE: Duration = Duration::from_millis(60);

/// A publish with every database read already done, ready to hand to the
/// adapter. Adapter-specific preparation still runs in the caller after this
/// value is built; only its resulting OS-only write crosses into the guarded
/// task.
///
/// Storage preparation stays with the caller so a cancelled request stops
/// instead of finishing its queries long afterwards.
enum PreparedPublish {
    /// Preserve copy-back. An empty set means the entry has no stored
    /// representations (older history, or a synthesised `add_text` row), so the
    /// primary-only `write_entry` path applies.
    Preserve {
        entry: ClipboardEntry,
        representations: Vec<StoredClipboardRepresentation>,
    },
    /// `PlainText` copy-back: the plain fallback the capture pipeline normalised
    /// on insert.
    Plain(ClipboardEntry),
    /// Exactly one chosen representation, for the "paste as <format>" picker.
    Exact(StoredClipboardRepresentation),
}

/// Process-wide serialiser for clipboard publishes. Cheap to clone — every
/// [`NagoriRuntime`] clone shares one ledger, which is what makes the
/// exclusion process-wide rather than per-handle.
#[derive(Debug, Clone, Default)]
pub(crate) struct ClipboardCoordinator {
    ledger: Arc<AsyncMutex<PublishLedger>>,
}

impl ClipboardCoordinator {
    /// Wait for the lease and hand out the ledger guard.
    async fn acquire(&self) -> OwnedMutexGuard<PublishLedger> {
        Arc::clone(&self.ledger).lock_owned().await
    }
}

/// The publish history the lease guards: monotonic counters of the leases
/// handed out and the publishes made. Only the identity of the newest lease /
/// publish matters, so neither counter needs to be persisted or bounded.
#[derive(Debug, Default)]
struct PublishLedger {
    leases: u64,
    generation: u64,
}

/// Identity of one clipboard publish, minted by the [`ClipboardLease`] method
/// that performed it and consumed by [`ClipboardLease::paste_frontmost`].
///
/// Carrying the identity in the type is what stops a "copy now, paste later"
/// front-end from pasting a clip it did not publish. The token names both the
/// lease that minted it and the publish within that lease, so neither a
/// superseding publish nor a re-acquired lease can authorise the keystroke —
/// the paste is refused instead of typing whatever the newest publish left on
/// the clipboard. (The lease half matters on its own: without it, dropping the
/// lease and taking a fresh one with no publish in between would leave the
/// stale token valid, and the front-end contract this type enforces is exactly
/// "one lease across both steps".)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub struct ClipboardPublish {
    lease: u64,
    generation: u64,
    /// Wall-clock instant immediately before the prepared OS write began.
    /// Used only for adapters whose sequence may lap across suspend/resume.
    write_started_at: SystemTime,
}

#[cfg(test)]
impl ClipboardPublish {
    pub(super) fn with_write_started_at(mut self, write_started_at: SystemTime) -> Self {
        self.write_started_at = write_started_at;
        self
    }
}

/// An exclusive hold on the system clipboard for the duration of one
/// publish-then-paste operation.
///
/// Obtained from [`NagoriRuntime::clipboard_lease`] and released on drop. Hold
/// it across the *whole* operation — including a front-end's window hide,
/// focus restore, and paste delay — so nothing else in the process can publish
/// in the gap. Every other clipboard entry point in the runtime takes the same
/// lease for the length of its own publish, so a caller that only needs a copy
/// does not have to reach for this type.
pub struct ClipboardLease {
    runtime: NagoriRuntime,
    /// Identity of this lease, stamped into every [`ClipboardPublish`] it
    /// mints.
    lease: u64,
    /// The held guard. `None` only while an OS-side step owns it on its own
    /// task (see [`Self::guarded`]) and after a step's task died with it — a
    /// lost guard fails every later step closed rather than proceeding
    /// unserialised.
    ledger: Option<OwnedMutexGuard<PublishLedger>>,
}

impl std::fmt::Debug for ClipboardLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClipboardLease")
            .field("lease", &self.lease)
            .field("held", &self.ledger.is_some())
            .finish_non_exhaustive()
    }
}

/// What the pre-paste re-read of the OS clipboard could establish.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PublishVerification {
    /// The clipboard still holds this process's most recent write.
    Confirmed,
    /// Something else wrote to the clipboard after the publish.
    Changed,
    /// The adapter's counter may have lapped while the host was paused, so an
    /// equality match can no longer prove that this is still our clip.
    PossibleHostPause,
    /// The host can normally answer, but this probe failed. Treated as a
    /// refusal rather than as `Unverifiable`: a clipboard too wedged to report
    /// its sequence is exactly where an unnoticed foreign write is plausible.
    ProbeFailed,
    /// The host cannot answer the question at all — no clipboard reader is
    /// wired, or the adapter does not track its own writes (see
    /// `ClipboardReader::self_write_tracking`).
    Unverifiable,
}

impl ClipboardLease {
    /// Copy an entry back to the clipboard, re-offering every stored
    /// representation ([`PasteFormat::Preserve`]) or just its plain text.
    ///
    /// See [`NagoriRuntime::copy_entry_with_format`] for the representation
    /// contract; the difference here is only that the publish is recorded on
    /// the lease so a paste can prove it is acting on this clip.
    pub async fn copy_entry_with_format(
        &mut self,
        id: EntryId,
        format: PasteFormat,
    ) -> Result<ClipboardPublish> {
        // Prepare (every database read) in the caller, where a cancelled
        // request simply stops. Only the adapter call crosses onto the guarded
        // task, so what survives a cancellation is one clipboard write already
        // under way — never a request that times out mid-query and publishes
        // over the user's clipboard much later.
        let prepared = self.runtime.prepare_entry_publish(id, format).await?;
        let write = self.runtime.prepare_clipboard_write(prepared).await?;
        let write_started_at = SystemTime::now();
        self.guarded(move |_, _| write.publish()).await?;
        let publish = self.record_publish(write_started_at)?;
        self.runtime.record_reuse(id).await?;
        Ok(publish)
    }

    /// Publish exactly one stored representation of an entry, with no fallback
    /// to its primary content. See
    /// [`NagoriRuntime::copy_entry_representation`].
    pub async fn copy_entry_representation(
        &mut self,
        id: EntryId,
        mime: &str,
    ) -> Result<ClipboardPublish> {
        let prepared = self
            .runtime
            .prepare_representation_publish(id, mime)
            .await?;
        let write = self.runtime.prepare_clipboard_write(prepared).await?;
        let write_started_at = SystemTime::now();
        self.guarded(move |_, _| write.publish()).await?;
        let publish = self.record_publish(write_started_at)?;
        self.runtime.record_reuse(id).await?;
        Ok(publish)
    }

    /// Join the text of several entries into one clip and publish it. See
    /// [`NagoriRuntime::copy_entries_combined`] for the admission rules.
    pub async fn copy_entries_combined(&mut self, ids: &[EntryId]) -> Result<ClipboardPublish> {
        let combined = self.runtime.build_combined_copy(ids).await?;
        // A retention sweep or IPC clear can race between the insert and the
        // publish and remove the just-inserted row. Retry once before giving
        // up — the user pressed bulk-copy expecting the OS clipboard to
        // actually contain the combined text. The clone is bounded by
        // `max_entry_size_bytes` (`build_combined_copy` refuses anything
        // larger), so it costs one buffer, not one per selected entry.
        let id = self.runtime.add_text(combined.clone()).await?;
        match self.copy_entry_with_format(id, PasteFormat::Preserve).await {
            Err(AppError::NotFound) => {
                let id = self.runtime.add_text(combined).await?;
                self.copy_entry_with_format(id, PasteFormat::Preserve).await
            }
            other => other,
        }
    }

    /// Synthesise the paste keystroke for `publish`.
    ///
    /// Refuses — without synthesising anything — when the clip `publish`
    /// identifies is no longer what the OS would paste, either because a later
    /// publish superseded it or because the OS clipboard moved on under an
    /// external writer. The copy has already landed in that case, so the
    /// caller surfaces "the clipboard changed, nothing was pasted" and the
    /// user re-copies; typing the new owner's content into their app is the
    /// one outcome this must never produce.
    pub async fn paste_frontmost(&mut self, publish: ClipboardPublish) -> Result<()> {
        // Same completion-event wrap as `NagoriRuntime::paste_entry`: the
        // palette drives synthesis through here after its own copy step, and
        // ARCHITECTURE §17's grep recipes need a success signal too.
        // `result_code` collapses to a static label, so the event never
        // carries clipboard content.
        let started = Instant::now();
        let result = self.paste_frontmost_inner(publish).await;
        tracing::debug!(
            result_code = result_code(&result),
            elapsed_ms = elapsed_ms(started),
            "paste_frontmost"
        );
        result
    }

    async fn paste_frontmost_inner(&mut self, publish: ClipboardPublish) -> Result<()> {
        let ledger = self.held()?;
        if publish.lease != self.lease || publish.generation != ledger.generation {
            return Err(clipboard_changed(
                "this copy is no longer the clipboard's most recent publish; nothing was pasted",
            ));
        }
        self.guarded(move |runtime, mut caller| async move {
            // Verify inside the task that owns the lease, immediately before
            // the adapter call, so the probe and the keystroke sit as close
            // together as the platform API allows.
            match runtime.verify_own_publish(publish.write_started_at).await {
                PublishVerification::Changed => {
                    return Err(clipboard_changed(
                        "the system clipboard changed after the copy; nothing was pasted",
                    ));
                }
                PublishVerification::ProbeFailed => {
                    // Fail closed on a host that *can* answer but didn't: a
                    // wedged clipboard is exactly the state where an unnoticed
                    // foreign write is plausible, and the copy has already
                    // landed, so "copy again" costs the user far less than
                    // typing someone else's clip. Reuses the
                    // `ClipboardChanged` classification — its hint tells the
                    // user to re-copy, which is right either way, where "paste
                    // manually" would not be.
                    return Err(clipboard_changed(
                        "could not verify that the clipboard still holds this copy; nothing was \
                         pasted",
                    ));
                }
                PublishVerification::PossibleHostPause => {
                    return Err(clipboard_changed(
                        "the paste request crossed a possible sleep/wake gap; nothing was pasted",
                    ));
                }
                PublishVerification::Confirmed | PublishVerification::Unverifiable => {}
            }
            // The probe is an await, so the request can go away during it.
            // Nothing has been synthesised yet, and the window that was in
            // front when the request arrived may not be in front now — so stop
            // rather than type into whatever holds focus. (The write step needs
            // no such check: its side effect is the first thing it does.)
            if !caller.still_waiting() {
                return Err(AppError::Paste {
                    reason: PasteFailureReason::Unknown,
                    message: "the paste request was abandoned before the keystroke was \
                              synthesised"
                        .to_owned(),
                });
            }
            let outcome = runtime
                .paste
                .paste_frontmost()
                .await
                .and_then(ensure_pasted);
            // Keep the lease a beat past the synthesis call, *whatever* it
            // returned. `CGEventPost` / `SendInput` only post the keystroke —
            // the target app reads the clipboard when it gets round to
            // processing it — and a synthesis that fails part-way (a released
            // modifier, a partially inserted `SendInput` batch) may already
            // have posted enough to trigger a paste. Releasing the lease the
            // instant the call returns would let a queued publish overwrite the
            // clip before it is read. This does not close that window —
            // nothing acknowledges the read — it keeps the queue out of its
            // first `PASTE_CONSUMPTION_GRACE`.
            tokio::time::sleep(PASTE_CONSUMPTION_GRACE).await;
            outcome
        })
        .await
    }

    /// Stamp a completed publish and hand back its identity.
    fn record_publish(&mut self, write_started_at: SystemTime) -> Result<ClipboardPublish> {
        let lease = self.lease;
        let ledger = self.held_mut()?;
        // Wrapping is unreachable in practice (one publish per user action)
        // and harmless if reached: only equality against the newest token
        // matters, and a stale token would have to survive 2^64 publishes to
        // collide.
        ledger.generation = ledger.generation.wrapping_add(1);
        Ok(ClipboardPublish {
            lease,
            generation: ledger.generation,
            write_started_at,
        })
    }

    /// Run one OS-side step on a task that owns the lease guard.
    ///
    /// The caller's future may be dropped at any await point — an IPC handler
    /// is cancelled on peer disconnect and on the server deadline — while the
    /// step it is awaiting has already handed uncancellable work to the
    /// blocking pool. Owning the guard on the step's own task is what keeps
    /// "the lease is held" and "my side effect is still in flight" the same
    /// statement: if the caller goes away, the guard travels to the task and is
    /// released only once the OS call returns, so the next request cannot
    /// publish underneath it.
    async fn guarded<T, F, Fut>(&mut self, step: F) -> Result<T>
    where
        F: FnOnce(NagoriRuntime, CallerWaiting) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T>> + Send,
        T: Send + 'static,
    {
        let guard = self.ledger.take().ok_or_else(lost_lease)?;
        let runtime = self.runtime.clone();
        let (tx, rx) = oneshot::channel();
        // `alive` is dropped with this future — including when the caller is
        // cancelled — which is how the step learns its request went away. Held
        // explicitly until after the step has reported back.
        let (alive, waiting) = CallerWaiting::new();
        tokio::spawn(async move {
            let outcome = step(runtime, waiting).await;
            // A closed receiver means the caller was dropped; the guard then
            // falls out of scope here — after the step finished, never during
            // it.
            drop(tx.send((guard, outcome)));
        });
        let received = rx.await;
        drop(alive);
        let (guard, outcome) = received.map_err(|_| lost_lease())?;
        self.ledger = Some(guard);
        outcome
    }

    fn held(&self) -> Result<&PublishLedger> {
        self.ledger.as_deref().ok_or_else(lost_lease)
    }

    fn held_mut(&mut self) -> Result<&mut PublishLedger> {
        self.ledger.as_deref_mut().ok_or_else(lost_lease)
    }
}

impl NagoriRuntime {
    /// Take the process-wide clipboard lease, waiting for any publish-then-
    /// paste operation already in flight.
    ///
    /// Front-ends that split the copy from the paste (the palette hides its
    /// window and restores focus in between) must hold one lease across both
    /// steps. Callers that only copy can use the convenience methods on
    /// [`NagoriRuntime`], which take and release the lease themselves.
    pub async fn clipboard_lease(&self) -> ClipboardLease {
        let mut ledger = self.clipboard_coordinator.acquire().await;
        ledger.leases = ledger.leases.wrapping_add(1);
        let lease = ledger.leases;
        ClipboardLease {
            runtime: self.clone(),
            lease,
            ledger: Some(ledger),
        }
    }

    /// Read everything a format copy-back needs out of storage, leaving only
    /// the adapter call. Only reachable under a held lease, via
    /// [`ClipboardLease::copy_entry_with_format`].
    async fn prepare_entry_publish(
        &self,
        id: EntryId,
        format: PasteFormat,
    ) -> Result<PreparedPublish> {
        let entry = self.load_publishable_entry(id).await?;
        match format {
            // Re-offer every stored representation so a receiver that
            // understands HTML / RTF / image bytes can pick the richest
            // representation the source originally advertised, while a
            // plain-text target still finds the matching `text/plain`
            // fallback. Adapters whose
            // `clipboard_multi_representation_write` capability is
            // `Unsupported` (e.g. `MemoryClipboard`, or any host adapter not
            // built into this binary) inherit the trait's default impl, which
            // delegates to `write_entry`.
            PasteFormat::Preserve => Ok(PreparedPublish::Preserve {
                entry,
                representations: self.store.list_representations(id).await?,
            }),
            PasteFormat::PlainText => Ok(PreparedPublish::Plain(entry)),
        }
    }

    /// Resolve a requested MIME to the single representation to publish. Only
    /// reachable under a held lease, via
    /// [`ClipboardLease::copy_entry_representation`].
    async fn prepare_representation_publish(
        &self,
        id: EntryId,
        mime: &str,
    ) -> Result<PreparedPublish> {
        let entry = self.store.get(id).await?.ok_or(AppError::NotFound)?;
        refuse_blocked(&entry)?;
        let representations = self.store.list_representations(id).await?;
        let representation = select_representation(&representations, mime).ok_or_else(|| {
            // Deliberately MIME- and payload-free: the error reaches the UI
            // toast, and the requested format is the only safe detail.
            AppError::InvalidInput(
                "the requested clipboard format is not available for this entry".to_owned(),
            )
        })?;
        Ok(PreparedPublish::Exact(representation.clone()))
    }

    /// Complete adapter-specific, non-OS preparation for one publish.
    ///
    /// This stays on the caller future, alongside the database reads above,
    /// so cancellation during an image decode cannot later overwrite the
    /// clipboard. Only the returned OS write crosses into the guarded task.
    async fn prepare_clipboard_write(
        &self,
        prepared: PreparedPublish,
    ) -> Result<PreparedClipboardWrite> {
        match prepared {
            PreparedPublish::Preserve {
                entry,
                representations,
            } => {
                if representations.is_empty() {
                    Arc::clone(&self.clipboard).prepare_entry(entry).await
                } else {
                    Arc::clone(&self.clipboard)
                        .prepare_representations(entry, representations)
                        .await
                }
            }
            PreparedPublish::Plain(entry) => Arc::clone(&self.clipboard).prepare_plain(entry).await,
            PreparedPublish::Exact(representation) => {
                Arc::clone(&self.clipboard)
                    .prepare_representation_exact(representation)
                    .await
            }
        }
    }

    /// Load an entry ready for a copy-back: refused if `Blocked`, with image
    /// bytes hydrated from the payload table.
    async fn load_publishable_entry(&self, id: EntryId) -> Result<ClipboardEntry> {
        let mut entry = self.store.get(id).await?.ok_or(AppError::NotFound)?;
        refuse_blocked(&entry)?;
        // Image bytes survive capture in an `entry_representations` row
        // whose `ImageContent.pending_bytes` is dropped on deserialise, so
        // hydrate the bytes before the platform writer needs them.
        if let ClipboardContent::Image(image) = &mut entry.content
            && image.pending_bytes.is_none()
            && let Some((bytes, mime)) = self.store.get_payload(id).await?
        {
            image.pending_bytes = Some(bytes);
            if image.mime_type.is_none() {
                image.mime_type = Some(mime);
            }
        }
        Ok(entry)
    }

    /// Record that an entry was re-used, so the ranker reflects the copy-back.
    ///
    /// The ranker scores by `metadata.use_count` (see nagori-search), so
    /// bumping it changes which results win — drop cached hits before *and*
    /// after the increment.
    async fn record_reuse(&self, id: EntryId) -> Result<()> {
        self.invalidate_search_cache();
        self.store.increment_use_count(id).await?;
        self.invalidate_search_cache();
        Ok(())
    }

    /// Whether the OS clipboard still holds this process's most recent write.
    ///
    /// Answerable only where a reader is wired *and* the adapter records its
    /// own writes against a native sequence; see
    /// `ClipboardReader::self_write_tracking`. A host that cannot answer at
    /// all reports [`PublishVerification::Unverifiable`] and pastes as before;
    /// a host that can normally answer but whose probe failed reports
    /// [`PublishVerification::ProbeFailed`], which the paste path refuses.
    /// macOS additionally refuses a publish old enough to have crossed the
    /// capture loop's host-pause threshold because `changeCount` may lap over
    /// sleep and collide with the recorded self-write sequence. A backwards
    /// wall-clock step is refused there too because the age is unknowable.
    ///
    /// `Confirmed` means "no write landed since the sequence the adapter
    /// recorded for its own write" — which is not quite "the clipboard holds my
    /// clip", because that recording is not atomic with the write itself. See
    /// the module docs for the instruction-level window that leaves open, and
    /// why it is the best the platform APIs allow.
    async fn verify_own_publish(&self, write_started_at: SystemTime) -> PublishVerification {
        let Some(reader) = self.clipboard_reader.as_ref() else {
            return PublishVerification::Unverifiable;
        };
        let tracking = reader.self_write_tracking();
        if tracking == SelfWriteTracking::Untracked {
            return PublishVerification::Unverifiable;
        }
        match reader.current_sequence().await {
            Ok(sequence) => {
                // Sample after the async probe. If the host slept while that
                // probe was in flight, a pre-probe timestamp could incorrectly
                // authorise a lapped post-wake changeCount.
                if tracking == SelfWriteTracking::MayLapAfterHostPause {
                    match SystemTime::now().duration_since(write_started_at) {
                        Ok(age) if age >= RESYNC_GAP_THRESHOLD => {
                            tracing::warn!(age_secs = age.as_secs(), "clipboard_publish_wake_gap");
                            return PublishVerification::PossibleHostPause;
                        }
                        // A backwards wall-clock step makes the publish's age
                        // unknowable. Fail closed on a lapping sequence rather
                        // than treating it as the freshest possible token.
                        Err(err) => {
                            tracing::warn!(error = %err, "clipboard_publish_clock_rollback");
                            return PublishVerification::PossibleHostPause;
                        }
                        Ok(_) => {}
                    }
                }
                if reader.matches_self_write(&sequence) {
                    PublishVerification::Confirmed
                } else {
                    PublishVerification::Changed
                }
            }
            Err(err) => {
                tracing::warn!(error = %err, "clipboard_publish_verify_failed");
                PublishVerification::ProbeFailed
            }
        }
    }

    /// Build the single joined buffer a combined copy publishes.
    ///
    /// Bounded on three axes *before* anything is written: the de-duplicated
    /// selection may not exceed [`MAX_COMBINED_COPY_ENTRIES`], the joined text
    /// may not exceed the configured `max_entry_size_bytes`, and each body is
    /// appended to one buffer instead of being collected into a `Vec<String>`
    /// and joined afterwards. Reading N bodies into a vector and joining them
    /// allocated the selection twice over, so a large multi-selection of
    /// maximum-size entries reserved hundreds of megabytes only to fail the
    /// same limit at the end.
    ///
    /// Image / file-list entries and any non-`Public`/`Unknown` row are
    /// skipped, as are ids a concurrent sweep already removed — the
    /// multi-select UI surfaces the resulting count difference to the user.
    async fn build_combined_copy(&self, ids: &[EntryId]) -> Result<String> {
        // Fail closed: without settings there is no budget to admit against.
        let settings = self.store.get_settings().await?;
        if ids.is_empty() {
            return Err(AppError::InvalidInput("no entries selected".to_owned()));
        }
        // De-duplicate before counting so a selection that repeats an id (a
        // stale UI set, a hand-built IPC request) neither doubles the body nor
        // eats into the entry budget twice.
        // Cap the count *before* the first body is read, so an over-large
        // selection costs one pass over 16-byte ids instead of N reads of up
        // to `max_entry_size_bytes` each. The capacities are bounded by the cap
        // rather than by `ids.len()` so the caller cannot make the daemon
        // reserve for a selection it is about to refuse.
        let capacity = ids.len().min(MAX_COMBINED_COPY_ENTRIES + 1);
        let mut seen = HashSet::with_capacity(capacity);
        let mut unique = Vec::with_capacity(capacity);
        for id in ids {
            if seen.insert(*id) {
                unique.push(*id);
                if unique.len() > MAX_COMBINED_COPY_ENTRIES {
                    return Err(AppError::InvalidInput(format!(
                        "combined copy accepts at most {MAX_COMBINED_COPY_ENTRIES} entries"
                    )));
                }
            }
        }
        let budget = settings.max_entry_size_bytes;
        let mut combined = String::new();
        for id in &unique {
            // Skip ids that were concurrently swept by retention / another
            // delete path. Aborting the whole copy because one row of a
            // multi-selection raced with the maintenance loop would be
            // worse than producing a slightly shorter joined string.
            let Some(entry) = self.store.get(*id).await? else {
                continue;
            };
            // Only `Public` / `Unknown` text is safe to combine into the
            // clipboard without an explicit opt-in. Skipping `Private` here
            // (alongside `Secret` / `Blocked`) keeps bulk copy from silently
            // concatenating sensitive bodies the single-row path would have
            // dropped to preview-only — see `is_text_safe_for_default_output`.
            if !is_text_safe_for_default_output(entry.sensitivity) {
                continue;
            }
            let Some(text) = combinable_text(&entry) else {
                continue;
            };
            let separator = if combined.is_empty() {
                ""
            } else {
                COMBINED_COPY_SEPARATOR
            };
            // `InvalidInput` rather than the `Policy` that `add_text` raises
            // for the same ceiling: this is the user's selection being too
            // large, and a byte count is safe to show, so the palette can say
            // what to do about it instead of the generic "blocked by policy".
            let projected = combined
                .len()
                .checked_add(separator.len())
                .and_then(|len| len.checked_add(text.len()));
            if projected.is_none_or(|len| len > budget) {
                return Err(AppError::InvalidInput(format!(
                    "the selected entries join to more than the {budget}-byte entry limit; \
                     select fewer entries"
                )));
            }
            combined.push_str(separator);
            combined.push_str(text);
        }
        if combined.is_empty() {
            return Err(AppError::InvalidInput(
                "no copyable text in selection".to_owned(),
            ));
        }
        Ok(combined)
    }
}

/// The plain body a combined copy can append for this entry, borrowed from the
/// entry so the join never clones a body it may end up refusing. Image and
/// file-list entries have nothing to contribute.
const fn combinable_text(entry: &ClipboardEntry) -> Option<&str> {
    match &entry.content {
        ClipboardContent::Text(text) => Some(text.text.as_str()),
        ClipboardContent::Url(url) => Some(url.raw.as_str()),
        ClipboardContent::Code(code) => Some(code.text.as_str()),
        ClipboardContent::RichText(rich) => Some(rich.plain_text.as_str()),
        _ => None,
    }
}

/// Refuse a copy-back of a `Blocked` entry, on every publish path.
fn refuse_blocked(entry: &ClipboardEntry) -> Result<()> {
    if matches!(entry.sensitivity, Sensitivity::Blocked) {
        return Err(AppError::Policy(
            "blocked entries cannot be copied".to_owned(),
        ));
    }
    Ok(())
}

/// A guarded step's link back to the request that started it.
///
/// The step runs on its own task so a cancelled caller cannot release the lease
/// mid-side-effect, which also means the step keeps running when nobody is
/// waiting for it any more. A step with a decision point *before* its side
/// effect — the paste probes the clipboard before synthesising — asks here
/// whether it should still go ahead.
struct CallerWaiting(oneshot::Receiver<()>);

impl CallerWaiting {
    /// The sentinel the caller holds, and the handle the step polls.
    fn new() -> (oneshot::Sender<()>, Self) {
        let (tx, rx) = oneshot::channel();
        (tx, Self(rx))
    }

    /// Whether the request that started this step is still waiting for it.
    fn still_waiting(&mut self) -> bool {
        !matches!(self.0.try_recv(), Err(oneshot::error::TryRecvError::Closed))
    }
}

/// The lease guard went missing: a step's task died holding it (a panic in the
/// clipboard adapter, or a runtime shutting down under it). Every later step on
/// that lease fails with this rather than proceeding unserialised.
fn lost_lease() -> AppError {
    AppError::Platform("the clipboard lease was lost by a failed clipboard task".to_owned())
}

/// The paste-refused error, classified so the UI can say "the clipboard
/// changed" instead of the generic "auto-paste failed — paste manually" hint
/// (which would tell the user to paste the *new* owner's content).
fn clipboard_changed(message: &str) -> AppError {
    AppError::Paste {
        reason: PasteFailureReason::ClipboardChanged,
        message: message.to_owned(),
    }
}

/// Convert a `PasteResult` into an explicit success/failure.
///
/// `PasteController::paste_frontmost` reports OS-level outcomes via
/// `PasteResult { pasted, message }` and historically the daemon discarded
/// `pasted == false` as success. That hid both the unsupported-platform
/// branch (Noop on Linux/Windows) and any future "we tried but the OS
/// blocked it" path. We now treat `pasted=false` as a real failure and
/// promote `message` to the error so it surfaces in IPC / Tauri responses.
fn ensure_pasted(result: nagori_platform::PasteResult) -> Result<()> {
    if result.pasted {
        Ok(())
    } else {
        // `pasted == false` is the no-op controller branch (Noop on a host
        // without a wired paste adapter), i.e. synthetic paste is not
        // available here at all — classify it as such so the UI hint matches.
        Err(AppError::Paste {
            reason: PasteFailureReason::SynthUnsupported,
            message: result.message.unwrap_or_else(|| {
                "auto-paste did not run; OS paste controller reported pasted=false".to_owned()
            }),
        })
    }
}
