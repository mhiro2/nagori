//! Settings read/write, onboarding-marker bookkeeping, and the OS
//! permission probes that stamp those markers.

use nagori_core::{AppError, AppSettings, OnboardingSettings, Result, SettingsRepository};
use nagori_platform::{PermissionCheckContext, PermissionKind, PermissionState, PermissionStatus};
use time::OffsetDateTime;
use tracing::error;

use super::NagoriRuntime;

impl NagoriRuntime {
    fn publish_settings(&self, settings: AppSettings) {
        // `watch::Sender::send` only fails when *every* receiver has been
        // dropped — i.e. the daemon is mid-teardown or every subscriber
        // (capture loop, maintenance, IPC) has crashed. There is no
        // "stale config" downstream in that case because there is no
        // downstream left, but the absence of subscribers itself is the
        // signal: the daemon's settings fanout has effectively shut down
        // while the runtime keeps accepting writes. Surface it loudly
        // instead of silently swallowing it so this is visible in logs
        // rather than discovered when reload-after-restart "fixes"
        // things.
        if let Err(err) = self.settings_tx.send(settings) {
            error!(error = %err, "settings_broadcast_failed reason=no_receivers");
        }
    }

    pub async fn refresh_settings_from_store(&self) -> Result<AppSettings> {
        let settings = self.store.get_settings().await?;
        self.publish_settings(settings.clone());
        Ok(settings)
    }

    /// Returns the current OS permission status as a list. When no
    /// `PermissionChecker` is wired (e.g. headless tests, non-macOS desktop
    /// builds), returns an empty list rather than erroring so the UI can
    /// still render an "unsupported" hint.
    ///
    /// Side effect: when the checker reports `Accessibility = Granted`
    /// for the first time on this install, the runtime stamps
    /// `settings.onboarding.accessibility_first_granted_at`. The marker
    /// is sticky (a later revoke does not clear it) so the Setup card
    /// can distinguish `RevokedAfterGranted` from a fresh
    /// `PromptShownNotGranted` state.
    pub async fn permission_check(&self) -> Result<Vec<PermissionStatus>> {
        let Some(checker) = self.permissions.clone() else {
            return Ok(Vec::new());
        };
        let current = self.current_settings();
        let ctx = PermissionCheckContext {
            accessibility_prompted_at: current.onboarding.accessibility_prompted_at,
        };
        let statuses = checker.check(&ctx).await?;
        self.stamp_first_grant_if_observed(&statuses).await;
        Ok(statuses)
    }

    /// Idempotently set `onboarding.accessibility_first_granted_at` the
    /// first time we observe `Accessibility = Granted`. Persistence
    /// failures are logged rather than propagated: the marker is
    /// best-effort UX bookkeeping, and the checker's primary contract
    /// is to return the current permission state.
    async fn stamp_first_grant_if_observed(&self, statuses: &[PermissionStatus]) {
        // Cheap guard against re-acquiring the write lock when the
        // marker is already set — the authoritative re-check happens
        // inside `mutate_onboarding`, but skipping the lock entirely on
        // the steady-state hot path keeps the doctor / permission_check
        // poll from serialising behind unrelated settings updates.
        if self
            .current_settings()
            .onboarding
            .accessibility_first_granted_at
            .is_some()
        {
            return;
        }
        let observed_grant = statuses.iter().any(|s| {
            s.kind == PermissionKind::Accessibility && s.state == PermissionState::Granted
        });
        if !observed_grant {
            return;
        }
        let result = self
            .mutate_onboarding(|onboarding| {
                // Re-check inside the lock: another writer may have set
                // the marker between the guard above and now.
                if onboarding.accessibility_first_granted_at.is_none() {
                    onboarding.accessibility_first_granted_at = Some(OffsetDateTime::now_utc());
                }
            })
            .await;
        if let Err(err) = result {
            tracing::warn!(error = %err, "onboarding_first_grant_persist_failed");
        }
    }

    /// Trigger the host's accessibility prompt and report the resulting
    /// status. When `prompt = true` the runtime stamps
    /// `onboarding.accessibility_prompted_at` so subsequent
    /// `permission_check` calls discriminate `Denied` from
    /// `NotDetermined`. A `Granted` result also stamps
    /// `accessibility_first_granted_at` (sticky marker).
    pub async fn request_accessibility(&self, prompt: bool) -> Result<PermissionStatus> {
        let checker = self.permissions.clone().ok_or_else(|| {
            AppError::Unsupported("no permission checker is wired in this runtime".to_owned())
        })?;
        let status = checker.request_accessibility(prompt).await?;
        if prompt {
            // Always refresh the timestamp so dashboards can see "we
            // most recently asked at <t>" rather than the first-ever ask.
            // The UI's NotRequested vs PromptShownNotGranted branch only
            // cares about presence, so overwriting is safe.
            self.mutate_onboarding(|onboarding| {
                onboarding.accessibility_prompted_at = Some(OffsetDateTime::now_utc());
            })
            .await?;
        }
        if status.state == PermissionState::Granted {
            self.stamp_first_grant_if_observed(std::slice::from_ref(&status))
                .await;
        }
        Ok(status)
    }

    pub async fn get_settings(&self) -> Result<AppSettings> {
        self.store.get_settings().await
    }

    /// Persist updated settings *and* re-publish them on the watch channel
    /// so the capture loop and other subscribers pick up the change without
    /// the caller having to remember the second step.
    ///
    /// The runtime owns the `onboarding` markers: any value the caller
    /// passes in that field is silently replaced with the currently
    /// persisted state inside the write lock, so an `update_settings`
    /// from the desktop shell can never wipe an `accessibility_*` marker
    /// it didn't know about. Marker writes themselves go through
    /// [`Self::mutate_onboarding`], which acquires the same lock.
    pub async fn save_settings(&self, settings: AppSettings) -> Result<()> {
        let _guard = self.settings_write_lock.lock().await;
        let persisted = self.store.get_settings().await?;
        let mut merged = settings;
        merged.onboarding = persisted.onboarding;
        self.store.save_settings(merged.clone()).await?;
        self.publish_settings(merged);
        Ok(())
    }

    /// Read-modify-write the persisted settings under the settings write
    /// lock, returning the post-update snapshot.
    ///
    /// The read happens *inside* the critical section, so `f` always
    /// observes — and the follow-up save always carries — every other
    /// field's latest value. That is the invariant a plain `get_settings`
    /// → mutate → `save_settings` *outside* the lock breaks: a concurrent
    /// write landing between the read and the save is silently rolled back
    /// by the stale snapshot. Routing single-field toggles through here
    /// (rather than round-tripping a full blob the caller read earlier)
    /// keeps the tray's pause/resume from clobbering a `global_hotkey`
    /// edit the desktop shell made in parallel, and vice versa.
    async fn mutate_settings<F>(&self, f: F) -> Result<AppSettings>
    where
        F: FnOnce(&mut AppSettings),
    {
        let _guard = self.settings_write_lock.lock().await;
        let mut settings = self.store.get_settings().await?;
        f(&mut settings);
        self.store.save_settings(settings.clone()).await?;
        self.publish_settings(settings.clone());
        Ok(settings)
    }

    /// Apply `f` to the `onboarding` namespace under the settings write
    /// lock, reading the latest persisted state inside the critical
    /// section so a concurrent [`Self::save_settings`] cannot lose the
    /// marker. The other settings fields are left untouched.
    async fn mutate_onboarding<F>(&self, f: F) -> Result<()>
    where
        F: FnOnce(&mut OnboardingSettings),
    {
        self.mutate_settings(|settings| f(&mut settings.onboarding))
            .await
            .map(|_| ())
    }

    /// Toggle `capture_enabled` without round-tripping the entire settings
    /// blob — used by the tray menu and the `set_capture_enabled` Tauri
    /// command. Returns the post-update settings.
    ///
    /// The read-modify-write runs inside [`Self::mutate_settings`], so a
    /// concurrent `update_settings` can neither be rolled back by this
    /// toggle nor leave the returned snapshot stale.
    pub async fn set_capture_enabled(&self, enabled: bool) -> Result<AppSettings> {
        self.mutate_settings(|settings| settings.capture_enabled = enabled)
            .await
    }
}
