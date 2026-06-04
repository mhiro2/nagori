//! The GitHub release-version probe behind `nagori doctor`'s "update
//! available?" line: a cached, rate-limited, fail-soft lookup.

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Minimum interval between two successful or failed GitHub probes. A
/// new release lands at most every few days; a 24h floor keeps the
/// daemon from hammering `api.github.com` when an operator scripts
/// `nagori doctor` in a loop (or when a network flap fails every
/// request within the rate-limit window).
const UPDATE_PROBE_MIN_INTERVAL: Duration = Duration::from_hours(24);

/// Consecutive failure count after which the probe hard-disables for the
/// remainder of the daemon's lifetime. Five strikes covers the typical
/// transient-failure window (DNS flap, captive portal) without leaving
/// the probe running forever against a permanently-broken environment.
const UPDATE_PROBE_MAX_CONSECUTIVE_FAILURES: u32 = 5;

/// Caches the latest `fetch_latest_release_version` outcome and gates
/// re-attempts behind a 24h floor + hard-disable on repeated failure.
///
/// The state lives on `NagoriRuntime` (wrapped in `Arc` to keep `Clone`
/// cheap) so every IPC `Doctor` call shares the same cache — without
/// this the previous implementation made an HTTP request on every
/// doctor invocation, which is fine for an interactive operator but
/// pathological for monitoring jobs that poll the endpoint.
pub(crate) struct UpdateProbeState {
    inner: Mutex<UpdateProbeInner>,
}

impl Default for UpdateProbeState {
    fn default() -> Self {
        Self {
            inner: Mutex::new(UpdateProbeInner::default()),
        }
    }
}

#[derive(Default)]
struct UpdateProbeInner {
    /// Last time we *attempted* the probe (success or failure). `None`
    /// means we have not probed since the daemon started.
    last_attempt: Option<Instant>,
    /// Cached tag from the most recent successful probe. Stays valid
    /// until the next successful probe overwrites it; failures do not
    /// invalidate the cache so a flake doesn't downgrade doctor from
    /// "you're behind" to "(unknown)" on the next call.
    cached_version: Option<String>,
    /// Count of consecutive probe failures since the last success.
    /// Reset to zero on every successful probe.
    consecutive_failures: u32,
    /// Once `consecutive_failures` crosses
    /// [`UPDATE_PROBE_MAX_CONSECUTIVE_FAILURES`] we stop probing for
    /// the rest of the daemon's lifetime. Cleared on a restart, which
    /// is the appropriate recovery boundary — a daemon that keeps
    /// failing for hours is not going to recover within the same
    /// process.
    hard_disabled: bool,
}

impl UpdateProbeState {
    /// Return the cached tag if a fresh probe is not due, or perform a
    /// probe and cache the result. Always `Some(_)` once a successful
    /// probe has landed; `None` while uninitialised, during a probe
    /// failure, or after the hard-disable threshold is crossed.
    pub(crate) async fn fetch_if_due(&self) -> Option<String> {
        // Reserve the probe slot under the lock before dropping it: a
        // bare snapshot would let several concurrent doctor IPCs all
        // observe a stale `last_attempt` and burst-call GitHub in
        // parallel, defeating the 24h rate limit and stacking
        // `consecutive_failures` per call rather than per window. By
        // bumping `last_attempt` *before* the HTTP await, parallel
        // callers see a recent attempt and return the cached value
        // (possibly slightly stale) instead of starting their own
        // probe; the lock is still released across the network call
        // so a slow probe never blocks an unrelated doctor caller.
        let now = Instant::now();
        {
            let mut inner = match self.inner.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            if inner.hard_disabled {
                return inner.cached_version.clone();
            }
            if let Some(last) = inner.last_attempt
                && now.duration_since(last) < UPDATE_PROBE_MIN_INTERVAL
            {
                return inner.cached_version.clone();
            }
            inner.last_attempt = Some(now);
        }

        let result = fetch_latest_release_version().await;
        let mut inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(version) = result {
            inner.cached_version = Some(version);
            inner.consecutive_failures = 0;
        } else {
            inner.consecutive_failures = inner.consecutive_failures.saturating_add(1);
            if inner.consecutive_failures >= UPDATE_PROBE_MAX_CONSECUTIVE_FAILURES {
                inner.hard_disabled = true;
                tracing::warn!(
                    consecutive_failures = inner.consecutive_failures,
                    "update_probe_hard_disabled",
                );
            }
        }
        inner.cached_version.clone()
    }
}

/// Best-effort lookup of the latest released `nagori` tag on GitHub.
///
/// The doctor handler calls this through [`UpdateProbeState::fetch_if_due`]
/// so the bare function only handles the network round-trip; gating and
/// caching live one level up. Strict timeout, no retries: if GitHub is
/// unreachable, rate-limiting us, or returns an unexpected payload, we
/// return `None` and doctor renders "(unknown)" rather than failing the
/// whole report.
async fn fetch_latest_release_version() -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Release {
        tag_name: String,
    }
    let client = reqwest::Client::builder()
        .user_agent(concat!("nagori/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .ok()?;
    let release: Release = client
        .get("https://api.github.com/repos/mhiro2/nagori/releases/latest")
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .json()
        .await
        .ok()?;
    Some(release.tag_name.trim_start_matches('v').to_owned())
}
