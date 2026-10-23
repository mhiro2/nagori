//! The palette session: one showing of the palette, together with the
//! frontmost app captured when it opened, so a paste re-focuses the user's
//! prior app before synthesising the keystroke. macOS / Windows capture a real
//! restore target; Linux Wayland records `None` (no portable foreground query).
//!
//! A paste awaits settings, the clipboard lease and the copy before it hides
//! the palette, and the user can dismiss (or dismiss and reopen) the palette
//! at any of those points. Each showing therefore gets a generation number: a
//! palette action takes a [`PaletteTicket`] when it starts and only acts while
//! that showing is still the open one, so a request that outlives its palette
//! is dropped instead of pasting into whatever the user moved on to.

use nagori_platform::RestoreTarget;
#[cfg(target_os = "macos")]
use nagori_platform_macos::MacosWindowBehavior;
#[cfg(target_os = "windows")]
use nagori_platform_windows::WindowsWindowBehavior;

use super::AppState;

/// Whether this platform records a restore target when the palette opens. On
/// macOS and Windows a missing target means the capture failed, and the
/// synthesised paste must not go out blind; Linux Wayland never has one, and
/// relies on the compositor handing focus back once the palette hides.
pub const RESTORE_TARGET_EXPECTED: bool = cfg!(any(target_os = "macos", target_os = "windows"));

/// Which showing of the palette a palette action was started in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaletteTicket(u64);

/// The showing a [`PaletteTicket`] belongs to is no longer the open one: the
/// palette was dismissed, or dismissed and shown again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaletteSessionEnded;

/// State of the palette's current (or last) showing.
#[derive(Debug, Default)]
pub struct PaletteSession {
    generation: u64,
    open: bool,
    target: Option<RestoreTarget>,
}

impl PaletteSession {
    /// A new showing begins with `target` as the app to paste back into.
    fn begin(&mut self, target: Option<RestoreTarget>) {
        self.generation += 1;
        self.open = true;
        self.target = target;
    }

    /// The showing ends without handing its target to an action.
    fn end(&mut self) {
        self.open = false;
        self.target = None;
    }

    fn ticket(&self) -> Option<PaletteTicket> {
        self.open.then_some(PaletteTicket(self.generation))
    }

    const fn is_current(&self, ticket: PaletteTicket) -> bool {
        self.open && self.generation == ticket.0
    }

    /// End the ticket's showing on behalf of the action holding it and hand
    /// over the restore target, or refuse when that showing already ended.
    const fn finish(
        &mut self,
        ticket: PaletteTicket,
    ) -> Result<Option<RestoreTarget>, PaletteSessionEnded> {
        if !self.is_current(ticket) {
            return Err(PaletteSessionEnded);
        }
        self.open = false;
        Ok(self.target.take())
    }

    const fn shown_since(&self, ticket: PaletteTicket) -> bool {
        self.generation != ticket.0
    }
}

impl AppState {
    fn with_palette_session<T>(&self, f: impl FnOnce(&mut PaletteSession) -> T) -> T {
        let mut session = self
            .palette_session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f(&mut session)
    }

    /// Start a new palette showing, snapshotting the current frontmost app as
    /// its restore target — call this immediately *before* showing the palette
    /// so the snapshot reflects the source the user copied from / wants to
    /// paste back into. macOS uses `AppKit`, Windows uses
    /// `GetForegroundWindow` (and stamps the HWND into `native_handle`
    /// so `SetForegroundWindow` can re-foreground the *original* window
    /// at paste time), Linux Wayland records `None` because the
    /// compositor does not expose a portable foreground-surface query.
    pub fn begin_palette_session(&self) {
        let snapshot = capture_restore_target_blocking();
        self.with_palette_session(|session| session.begin(snapshot));
    }

    /// The palette is being hidden without an action: discard its restore
    /// target and stop any action still in flight for this showing.
    pub fn end_palette_session(&self) {
        self.with_palette_session(PaletteSession::end);
    }

    /// The showing a palette action starts in, or `None` when the palette is
    /// not open (the request arrived after it was dismissed).
    pub fn palette_ticket(&self) -> Option<PaletteTicket> {
        self.with_palette_session(|session| session.ticket())
    }

    /// Whether the ticket's showing is still the open one.
    pub fn palette_session_is_current(&self, ticket: PaletteTicket) -> bool {
        self.with_palette_session(|session| session.is_current(ticket))
    }

    /// Close the ticket's showing for the action about to hide the palette and
    /// take its restore target. Fails when the palette was dismissed (or
    /// reopened) after the action started, in which case the action must stop
    /// short of hiding, re-focusing, or pasting.
    pub fn finish_palette_session(
        &self,
        ticket: PaletteTicket,
    ) -> Result<Option<RestoreTarget>, PaletteSessionEnded> {
        self.with_palette_session(|session| session.finish(ticket))
    }

    /// Whether the palette was shown again after the ticket's showing. A paste
    /// that already hid the palette checks this before posting its keystroke:
    /// a reopened palette holds focus, so the keys would land in it.
    pub fn palette_shown_since(&self, ticket: PaletteTicket) -> bool {
        self.with_palette_session(|session| session.shown_since(ticket))
    }
}

/// Cross-platform synchronous restore-target probe used to seed the palette
/// session. The helper avoids dragging a `tokio` runtime
/// into Tauri command callbacks (some are sync, e.g. `open_palette`) by
/// going through each platform crate's `_blocking` accessor. Linux
/// Wayland has no portable equivalent, so the helper returns `None`
/// without erroring — see `LinuxWindowBehavior` for the trade-off.
#[cfg(target_os = "macos")]
fn capture_restore_target_blocking() -> Option<RestoreTarget> {
    MacosWindowBehavior::capture_restore_target_blocking()
}

#[cfg(target_os = "windows")]
fn capture_restore_target_blocking() -> Option<RestoreTarget> {
    WindowsWindowBehavior::capture_restore_target_blocking()
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const fn capture_restore_target_blocking() -> Option<RestoreTarget> {
    None
}

#[cfg(test)]
mod tests {
    use nagori_core::SourceApp;

    use super::*;

    fn target(bundle_id: &str) -> RestoreTarget {
        RestoreTarget {
            source: SourceApp {
                bundle_id: Some(bundle_id.to_owned()),
                name: None,
                executable_path: None,
            },
            native_handle: None,
            snapshot_pid: None,
        }
    }

    fn bundle(target: Option<RestoreTarget>) -> Option<String> {
        target.and_then(|t| t.source.bundle_id)
    }

    #[test]
    fn no_ticket_before_the_palette_opens_or_after_it_hides() {
        let mut session = PaletteSession::default();
        assert_eq!(session.ticket(), None);
        session.begin(Some(target("com.example.editor")));
        assert!(session.ticket().is_some());
        session.end();
        assert_eq!(session.ticket(), None);
    }

    #[test]
    fn the_action_finishing_its_showing_takes_the_target() {
        let mut session = PaletteSession::default();
        session.begin(Some(target("com.example.editor")));
        let ticket = session.ticket().expect("open palette");
        assert_eq!(
            bundle(session.finish(ticket).expect("still open")),
            Some("com.example.editor".to_owned())
        );
        // The showing is over: a second action from it gets nothing.
        assert_eq!(session.finish(ticket), Err(PaletteSessionEnded));
        assert!(!session.shown_since(ticket));
    }

    #[test]
    fn a_dismissal_while_the_action_waits_stops_it() {
        let mut session = PaletteSession::default();
        session.begin(Some(target("com.example.editor")));
        let ticket = session.ticket().expect("open palette");
        session.end();
        assert!(!session.is_current(ticket));
        assert_eq!(session.finish(ticket), Err(PaletteSessionEnded));
    }

    #[test]
    fn a_reopened_palette_is_a_different_showing() {
        let mut session = PaletteSession::default();
        session.begin(Some(target("com.example.editor")));
        let stale = session.ticket().expect("open palette");
        session.end();
        session.begin(Some(target("com.example.browser")));
        // The old request must not take the new showing's target.
        assert!(!session.is_current(stale));
        assert_eq!(session.finish(stale), Err(PaletteSessionEnded));
        let fresh = session.ticket().expect("reopened palette");
        assert_eq!(
            bundle(session.finish(fresh).expect("current showing")),
            Some("com.example.browser".to_owned())
        );
    }

    #[test]
    fn reopening_after_a_paste_hid_the_palette_is_detected() {
        let mut session = PaletteSession::default();
        session.begin(None);
        let ticket = session.ticket().expect("open palette");
        // Linux: the showing carries no target on purpose, which is not a
        // dismissal.
        assert_eq!(session.finish(ticket), Ok(None));
        assert!(!session.shown_since(ticket));
        session.begin(None);
        assert!(session.shown_since(ticket));
    }
}
