//! Event-driven tracking of the Wayland clipboard selection.
//!
//! The data-control protocols (`ext_data_control_v1` / `wlr_data_control_v1`)
//! announce every selection change as a `selection` event carrying a fresh
//! offer object. [`SelectionWatcher`] keeps one Wayland connection open for
//! the adapter's lifetime, dispatches those events on a dedicated thread, and
//! numbers each one with a monotonic *generation*. The generation is the
//! adapter's clipboard sequence: reading it never touches the clipboard
//! owner, so an unchanged clipboard costs nothing per poll, a re-copy of
//! identical bytes still reads as a change (it is a new offer), and the body
//! is only requested — through the same offer the generation names — when the
//! capture loop decides to read a new clip.
//!
//! Polling the bodies instead would ask the owner to serve its data every
//! tick, which consumes "paste once" offers (`wl-copy --paste-once`) and makes
//! owners that encode on demand (GIMP, Krita) re-encode images continuously.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::io::{self, PipeReader, PipeWriter, Read, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;

use wayland_client::backend::WaylandError;
use wayland_client::globals::{GlobalError, GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{
    ConnectError, Connection, Dispatch, EventQueue, Proxy, QueueHandle, event_created_child,
};
use wayland_protocols::ext::data_control::v1::client::{
    ext_data_control_device_v1::{self, ExtDataControlDeviceV1},
    ext_data_control_manager_v1::ExtDataControlManagerV1,
    ext_data_control_offer_v1::{self, ExtDataControlOfferV1},
};
use wayland_protocols_wlr::data_control::v1::client::{
    zwlr_data_control_device_v1::{self, ZwlrDataControlDeviceV1},
    zwlr_data_control_manager_v1::ZwlrDataControlManagerV1,
    zwlr_data_control_offer_v1::{self, ZwlrDataControlOfferV1},
};

/// Why the watcher could not be started.
#[derive(Debug)]
pub enum WatchError {
    /// No compositor reachable (`WAYLAND_DISPLAY` unset, dead socket, …).
    Connect(ConnectError),
    /// The compositor exposes neither data-control manager.
    MissingProtocol,
    /// The connection came up but the initial exchange failed.
    Communication(String),
}

impl std::fmt::Display for WatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connect(err) => write!(f, "{err}"),
            Self::MissingProtocol => f.write_str("ext-data-control v1 or wlr-data-control v1"),
            Self::Communication(message) => f.write_str(message),
        }
    }
}

/// A data-control offer from either protocol flavour.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Offer {
    Ext(ExtDataControlOfferV1),
    Wlr(ZwlrDataControlOfferV1),
}

impl Offer {
    fn receive(&self, mime_type: String, fd: BorrowedFd<'_>) {
        match self {
            Self::Ext(offer) => offer.receive(mime_type, fd),
            Self::Wlr(offer) => offer.receive(mime_type, fd),
        }
    }

    fn destroy(&self) {
        match self {
            Self::Ext(offer) => offer.destroy(),
            Self::Wlr(offer) => offer.destroy(),
        }
    }
}

enum Manager {
    Ext(ExtDataControlManagerV1),
    Wlr(ZwlrDataControlManagerV1),
}

enum Device {
    Ext(ExtDataControlDeviceV1),
    Wlr(ZwlrDataControlDeviceV1),
}

impl Device {
    fn destroy(&self) {
        match self {
            Self::Ext(device) => device.destroy(),
            Self::Wlr(device) => device.destroy(),
        }
    }
}

/// The selection as of one generation: `offer` is `None` while the clipboard
/// is empty, otherwise the offer object together with the MIME types its
/// owner advertised.
#[derive(Clone, Debug)]
pub struct SelectionState<K> {
    pub generation: u64,
    pub offer: Option<(K, Arc<HashSet<String>>)>,
}

/// Protocol-independent bookkeeping for the selection events.
///
/// Every `data_offer` introduces a new offer object, its `offer` events list
/// the MIME types, and the `selection` event that follows makes it (or no
/// offer) the clipboard. Each `selection` bumps the generation, including one
/// that re-announces identical bytes, because a re-copy is a new offer. Split
/// out of the dispatch code (generic over the offer handle) so the
/// state machine is unit-testable without a compositor.
#[derive(Debug)]
pub struct SelectionTracker<K> {
    pending: HashMap<K, Vec<String>>,
    current: SelectionState<K>,
}

impl<K: Clone + Eq + Hash> SelectionTracker<K> {
    pub fn new(generation: u64) -> Self {
        Self {
            pending: HashMap::new(),
            current: SelectionState {
                generation,
                offer: None,
            },
        }
    }

    pub const fn generation(&self) -> u64 {
        self.current.generation
    }

    pub fn current(&self) -> SelectionState<K> {
        self.current.clone()
    }

    /// `data_offer`: a new offer object whose MIME types follow.
    pub fn introduce(&mut self, offer: K) {
        self.pending.insert(offer, Vec::new());
    }

    /// `offer`: one MIME type of an introduced offer.
    pub fn advertise(&mut self, offer: &K, mime_type: String) {
        if let Some(mime_types) = self.pending.get_mut(offer) {
            mime_types.push(mime_type);
        }
    }

    /// `selection`: `offer` is now the clipboard (`None` = cleared). Returns
    /// the offer it replaced, which the caller must destroy.
    pub fn select(&mut self, offer: Option<K>) -> Option<K> {
        if let (Some(new), Some((current, _))) = (&offer, &self.current.offer)
            && new == current
        {
            // The same object re-announced: nothing changed hands.
            return None;
        }
        let next = offer.map(|offer| {
            let mime_types = self.pending.remove(&offer).unwrap_or_default();
            (offer, Arc::new(mime_types.into_iter().collect()))
        });
        self.current.generation = self.current.generation.wrapping_add(1);
        std::mem::replace(&mut self.current.offer, next).map(|(previous, _)| previous)
    }

    /// An introduced offer the watcher will never read (the primary
    /// selection). Returns it for the caller to destroy.
    pub fn discard(&mut self, offer: K) -> K {
        self.pending.remove(&offer);
        offer
    }
}

struct Shared {
    tracker: SelectionTracker<Offer>,
    /// Set when the dispatch thread exits; the selection is no longer
    /// tracked and the adapter must start a new watcher.
    failure: Option<String>,
}

/// Dispatch state owned by the watcher thread.
struct WatchState {
    manager: Manager,
    /// Registry name and proxy of the seat whose selection we follow.
    seat: Option<(u32, WlSeat)>,
    device: Option<Device>,
    shared: Arc<Mutex<Shared>>,
    /// Set by the data device's `finished` event: the device can no longer
    /// report selections, so the dispatch loop exits and the adapter starts
    /// a fresh watcher.
    finished: bool,
}

impl WatchState {
    fn lock(&self) -> MutexGuard<'_, Shared> {
        lock_shared(&self.shared)
    }

    /// Follow the seat advertised as registry global `name`. The first seat
    /// wins; like `wl-paste` without `--seat`, a multi-seat session tracks a
    /// single seat.
    fn attach_seat(&mut self, registry: &WlRegistry, name: u32, qh: &QueueHandle<Self>) {
        let seat: WlSeat = registry.bind(name, 1, qh, ());
        let device = match &self.manager {
            Manager::Ext(manager) => Device::Ext(manager.get_data_device(&seat, qh, ())),
            Manager::Wlr(manager) => Device::Wlr(manager.get_data_device(&seat, qh, ())),
        };
        self.seat = Some((name, seat));
        self.device = Some(device);
    }

    /// The tracked seat (or its device) went away: the clipboard is no longer
    /// observable, which reads as "cleared" until another seat is attached.
    fn detach_seat(&mut self) {
        if let Some(device) = self.device.take() {
            device.destroy();
        }
        self.seat = None;
        let previous = self.lock().tracker.select(None);
        if let Some(offer) = previous {
            offer.destroy();
        }
    }

    fn on_data_offer(&self, offer: Offer) {
        self.lock().tracker.introduce(offer);
    }

    fn on_selection(&self, offer: Option<Offer>) {
        let previous = self.lock().tracker.select(offer);
        if let Some(offer) = previous {
            offer.destroy();
        }
    }

    fn on_primary_selection(&self, offer: Option<Offer>) {
        if let Some(offer) = offer {
            self.lock().tracker.discard(offer).destroy();
        }
    }

    fn on_mime_type(&self, offer: &Offer, mime_type: String) {
        self.lock().tracker.advertise(offer, mime_type);
    }
}

/// The lowest-numbered seat global other than `except`.
fn first_seat(list: &[wayland_client::globals::Global], except: Option<u32>) -> Option<u32> {
    list.iter()
        .filter(|global| global.interface == WlSeat::interface().name)
        .map(|global| global.name)
        .filter(|name| Some(*name) != except)
        .min()
}

fn lock_shared(shared: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    // The critical sections only update plain bookkeeping, so a panic in one
    // cannot leave the tracker half-written in a way later readers could
    // trip over; keep serving it rather than propagating the poison.
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Dispatch<WlRegistry, GlobalListContents> for WatchState {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        data: &GlobalListContents,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name, interface, ..
            } if interface == WlSeat::interface().name && state.seat.is_none() => {
                state.attach_seat(registry, name, qh);
            }
            wl_registry::Event::GlobalRemove { name }
                if state.seat.as_ref().is_some_and(|(seat, _)| *seat == name) =>
            {
                state.detach_seat();
                // Remaining seats were announced long ago and produce no new
                // `global` event, so follow the next one from the list.
                let next = data.with_list(|list| first_seat(list, Some(name)));
                if let Some(next) = next {
                    state.attach_seat(registry, next, qh);
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<WlSeat, ()> for WatchState {
    fn event(
        _state: &mut Self,
        _seat: &WlSeat,
        _event: <WlSeat as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtDataControlManagerV1, ()> for WatchState {
    fn event(
        _state: &mut Self,
        _manager: &ExtDataControlManagerV1,
        _event: <ExtDataControlManagerV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrDataControlManagerV1, ()> for WatchState {
    fn event(
        _state: &mut Self,
        _manager: &ZwlrDataControlManagerV1,
        _event: <ZwlrDataControlManagerV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtDataControlDeviceV1, ()> for WatchState {
    fn event(
        state: &mut Self,
        _device: &ExtDataControlDeviceV1,
        event: ext_data_control_device_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        use ext_data_control_device_v1::Event;
        match event {
            Event::DataOffer { id } => state.on_data_offer(Offer::Ext(id)),
            Event::Selection { id } => state.on_selection(id.map(Offer::Ext)),
            Event::PrimarySelection { id } => state.on_primary_selection(id.map(Offer::Ext)),
            Event::Finished => {
                state.detach_seat();
                state.finished = true;
            }
            _ => {}
        }
    }

    event_created_child!(WatchState, ExtDataControlDeviceV1, [
        ext_data_control_device_v1::EVT_DATA_OFFER_OPCODE => (ExtDataControlOfferV1, ()),
    ]);
}

impl Dispatch<ZwlrDataControlDeviceV1, ()> for WatchState {
    fn event(
        state: &mut Self,
        _device: &ZwlrDataControlDeviceV1,
        event: zwlr_data_control_device_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        use zwlr_data_control_device_v1::Event;
        match event {
            Event::DataOffer { id } => state.on_data_offer(Offer::Wlr(id)),
            Event::Selection { id } => state.on_selection(id.map(Offer::Wlr)),
            Event::PrimarySelection { id } => state.on_primary_selection(id.map(Offer::Wlr)),
            Event::Finished => {
                state.detach_seat();
                state.finished = true;
            }
            _ => {}
        }
    }

    event_created_child!(WatchState, ZwlrDataControlDeviceV1, [
        zwlr_data_control_device_v1::EVT_DATA_OFFER_OPCODE => (ZwlrDataControlOfferV1, ()),
    ]);
}

impl Dispatch<ExtDataControlOfferV1, ()> for WatchState {
    fn event(
        state: &mut Self,
        offer: &ExtDataControlOfferV1,
        event: ext_data_control_offer_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let ext_data_control_offer_v1::Event::Offer { mime_type } = event {
            state.on_mime_type(&Offer::Ext(offer.clone()), mime_type);
        }
    }
}

impl Dispatch<ZwlrDataControlOfferV1, ()> for WatchState {
    fn event(
        state: &mut Self,
        offer: &ZwlrDataControlOfferV1,
        event: zwlr_data_control_offer_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let zwlr_data_control_offer_v1::Event::Offer { mime_type } = event {
            state.on_mime_type(&Offer::Wlr(offer.clone()), mime_type);
        }
    }
}

/// Owns the persistent Wayland connection and the thread dispatching its
/// selection events. Dropping it stops and joins the thread.
pub struct SelectionWatcher {
    conn: Connection,
    shared: Arc<Mutex<Shared>>,
    /// Closing the write end wakes the dispatch thread's `poll` to exit.
    stop: Option<PipeWriter>,
    /// A byte written here makes the dispatch thread retry its flush.
    wake: PipeWriter,
    thread: Option<JoinHandle<()>>,
}

impl SelectionWatcher {
    /// Connect, bind the data-control manager and the first seat, load the
    /// current selection, then hand the connection to the dispatch thread.
    ///
    /// Generations continue from `start_generation`, so a watcher started to
    /// replace a failed one never re-issues a value the capture loop already
    /// anchored. Blocking: the initial exchange is a Wayland roundtrip.
    pub fn spawn(start_generation: u64) -> Result<Self, WatchError> {
        let conn = Connection::connect_to_env().map_err(WatchError::Connect)?;
        let (globals, mut queue) =
            registry_queue_init::<WatchState>(&conn).map_err(|err| match err {
                GlobalError::Backend(err) => WatchError::Communication(err.to_string()),
                GlobalError::InvalidId(err) => WatchError::Communication(err.to_string()),
            })?;
        let qh = queue.handle();
        // ext-data-control is the standardised successor; prefer it and fall
        // back to the wlroots protocol. v1 of either is enough for the
        // regular selection.
        let manager = globals
            .bind::<ExtDataControlManagerV1, _, _>(&qh, 1..=1, ())
            .map(Manager::Ext)
            .or_else(|_| {
                globals
                    .bind::<ZwlrDataControlManagerV1, _, _>(&qh, 1..=1, ())
                    .map(Manager::Wlr)
            })
            .map_err(|_| WatchError::MissingProtocol)?;

        let shared = Arc::new(Mutex::new(Shared {
            tracker: SelectionTracker::new(start_generation),
            failure: None,
        }));
        let mut state = WatchState {
            manager,
            seat: None,
            device: None,
            shared: Arc::clone(&shared),
            finished: false,
        };
        if let Some(name) = globals.contents().with_list(|list| first_seat(list, None)) {
            state.attach_seat(globals.registry(), name, &qh);
        }
        // Receive the selection that is already on the clipboard so the
        // first generation the capture loop sees describes it.
        queue
            .roundtrip(&mut state)
            .map_err(|err| WatchError::Communication(err.to_string()))?;

        let pipe_error = |err: io::Error| WatchError::Communication(err.to_string());
        let (stop_rx, stop_tx) = std::io::pipe().map_err(pipe_error)?;
        let (wake_rx, wake_tx) = std::io::pipe().map_err(pipe_error)?;
        set_nonblocking(wake_rx.as_fd()).map_err(pipe_error)?;
        set_nonblocking(wake_tx.as_fd()).map_err(pipe_error)?;
        let thread_shared = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("nagori-wl-selection".to_owned())
            .spawn(move || {
                let wakers = Wakers {
                    stop: stop_rx,
                    wake: wake_rx,
                };
                if let Err(message) = dispatch_until_stopped(&mut queue, &mut state, &wakers) {
                    tracing::warn!(error = %message, "clipboard_selection_watcher_stopped");
                    lock_shared(&thread_shared).failure = Some(message);
                }
            })
            .map_err(|err| WatchError::Communication(err.to_string()))?;
        Ok(Self {
            conn,
            shared,
            stop: Some(stop_tx),
            wake: wake_tx,
            thread: Some(thread),
        })
    }

    /// The current generation and offer, or the reason the watcher stopped.
    pub fn current(&self) -> Result<SelectionState<Offer>, String> {
        let shared = lock_shared(&self.shared);
        match &shared.failure {
            Some(message) => Err(message.clone()),
            None => Ok(shared.tracker.current()),
        }
    }

    pub fn generation(&self) -> Result<u64, String> {
        let shared = lock_shared(&self.shared);
        match &shared.failure {
            Some(message) => Err(message.clone()),
            None => Ok(shared.tracker.generation()),
        }
    }

    /// The last generation issued, whether or not the watcher is still
    /// running.
    pub fn last_generation(&self) -> u64 {
        lock_shared(&self.shared).tracker.generation()
    }

    pub fn is_stopped(&self) -> bool {
        lock_shared(&self.shared).failure.is_some()
    }

    /// Ask the owner of `offer` to write its `mime_type` body into a new
    /// pipe and return the read end.
    ///
    /// If the offer has been replaced (and destroyed) in the meantime the
    /// request is dropped and the pipe reads as empty; callers compare the
    /// generation after reading to discard such a result.
    pub fn receive(&self, offer: &Offer, mime_type: &str) -> io::Result<PipeReader> {
        let (reader, writer) = std::io::pipe()?;
        // The request keeps its own duplicate of the fd until it is sent,
        // so closing ours right away leaves the owner as the only writer
        // and the read end sees EOF once it finishes.
        offer.receive(mime_type.to_owned(), writer.as_fd());
        drop(writer);
        match self.conn.flush() {
            Ok(()) => Ok(reader),
            // A full socket buffer keeps the request queued. The dispatch
            // thread may be parked waiting for input only, so wake it: it
            // retries the flush and keeps waiting for the socket to become
            // writable until the request is out.
            Err(WaylandError::Io(err)) if err.kind() == io::ErrorKind::WouldBlock => {
                // A full wake pipe already holds a pending wake-up.
                let _ = (&self.wake).write(&[0]);
                Ok(reader)
            }
            Err(err) => Err(io::Error::other(err.to_string())),
        }
    }
}

impl Drop for SelectionWatcher {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The dispatch thread's non-Wayland wake-up sources.
struct Wakers {
    /// Readable (hang-up) once the owner drops the watcher.
    stop: PipeReader,
    /// Readable when a reader queued a request it could not flush.
    wake: PipeReader,
}

/// Dispatch events until the stop pipe closes (`Ok`) or the connection fails
/// (`Err` with the reason).
fn dispatch_until_stopped(
    queue: &mut EventQueue<WatchState>,
    state: &mut WatchState,
    wakers: &Wakers,
) -> Result<(), String> {
    loop {
        queue
            .dispatch_pending(state)
            .map_err(|err| err.to_string())?;
        if state.finished {
            return Err("clipboard data device finished".to_owned());
        }
        // Requests from readers (`receive`) and from the dispatch above
        // (offer destroys) may still be queued when the socket was full;
        // keep waiting for it to become writable until they are out.
        let output_pending = match queue.flush() {
            Ok(()) => false,
            Err(WaylandError::Io(err)) if err.kind() == io::ErrorKind::WouldBlock => true,
            Err(err) => return Err(err.to_string()),
        };
        // `None` means events were queued since the dispatch above.
        let Some(guard) = queue.prepare_read() else {
            continue;
        };
        match wait_for_events(guard.connection_fd(), output_pending, wakers)
            .map_err(|err| err.to_string())?
        {
            Wake::Stop => return Ok(()),
            Wake::Input => match guard.read() {
                Ok(_) => {}
                Err(WaylandError::Io(err)) if err.kind() == io::ErrorKind::WouldBlock => {}
                Err(err) => return Err(err.to_string()),
            },
            // Drop the read guard and go round again to flush.
            Wake::Flush => {}
        }
    }
}

enum Wake {
    /// The socket has events (or hung up; `read()` reports that).
    Input,
    /// The socket became writable, or a reader asked for a flush.
    Flush,
    Stop,
}

/// Block until the Wayland socket has something to read (or, with
/// `output_pending`, room to write), a reader asks for a flush, or the stop
/// pipe closes. No timeout: the thread has nothing else to do, and a wedged
/// compositor only delays events, never a caller — readers get the last known
/// generation without waiting on this thread.
fn wait_for_events(
    connection: BorrowedFd<'_>,
    output_pending: bool,
    wakers: &Wakers,
) -> io::Result<Wake> {
    let connection_events = if output_pending {
        libc::POLLIN | libc::POLLOUT
    } else {
        libc::POLLIN
    };
    loop {
        let mut fds = [
            libc::pollfd {
                fd: connection.as_raw_fd(),
                events: connection_events,
                revents: 0,
            },
            libc::pollfd {
                fd: wakers.stop.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: wakers.wake.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // SAFETY: `fds` is a fully-initialised three-element array whose fds
        // are borrowed for the duration of the call; `poll` only writes the
        // `revents` fields, which we read afterwards.
        let rc = unsafe { libc::poll(fds.as_mut_ptr(), 3, -1) };
        if rc < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        // Any readiness on the stop pipe (data, hang-up, error) means the
        // owner dropped the watcher.
        if fds[1].revents != 0 {
            return Ok(Wake::Stop);
        }
        let connection = fds[0].revents;
        if connection & !libc::POLLOUT != 0 {
            return Ok(Wake::Input);
        }
        if fds[2].revents != 0 {
            drain(&wakers.wake);
            return Ok(Wake::Flush);
        }
        if connection != 0 {
            return Ok(Wake::Flush);
        }
    }
}

/// Empty the (non-blocking) wake pipe so it does not stay readable.
fn drain(mut pipe: &PipeReader) {
    let mut buf = [0u8; 64];
    while matches!(pipe.read(&mut buf), Ok(n) if n > 0) {}
}

fn set_nonblocking(fd: BorrowedFd<'_>) -> io::Result<()> {
    let raw = fd.as_raw_fd();
    // SAFETY: `fcntl` with `F_GETFL` / `F_SETFL` only reads and updates the
    // status flags of `raw`, which stays open for the call via the borrow.
    let flags = unsafe { libc::fcntl(raw, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: as above.
    if unsafe { libc::fcntl(raw, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::SelectionTracker;

    fn mime_types(tracker: &SelectionTracker<u32>) -> Vec<String> {
        let mut types: Vec<String> = tracker
            .current()
            .offer
            .map(|(_, types)| types.iter().cloned().collect())
            .unwrap_or_default();
        types.sort();
        types
    }

    #[test]
    fn selection_bumps_the_generation_and_records_the_offer_types() {
        let mut tracker = SelectionTracker::new(0);
        tracker.introduce(1);
        tracker.advertise(&1, "text/plain".to_owned());
        tracker.advertise(&1, "UTF8_STRING".to_owned());
        assert_eq!(tracker.select(Some(1)), None);

        let current = tracker.current();
        assert_eq!(current.generation, 1);
        assert_eq!(current.offer.as_ref().map(|(offer, _)| *offer), Some(1));
        assert_eq!(mime_types(&tracker), ["UTF8_STRING", "text/plain"]);
    }

    #[test]
    fn a_new_offer_with_identical_types_is_still_a_new_generation() {
        // Re-copying the same bytes publishes a new offer; the capture loop
        // must see it as a change even though nothing about the offer's
        // advertised types differs.
        let mut tracker = SelectionTracker::new(0);
        for offer in [1, 2] {
            tracker.introduce(offer);
            tracker.advertise(&offer, "text/plain".to_owned());
        }
        tracker.select(Some(1));
        let replaced = tracker.select(Some(2));

        assert_eq!(
            replaced,
            Some(1),
            "the replaced offer is handed back to destroy"
        );
        assert_eq!(tracker.generation(), 2);
    }

    #[test]
    fn clearing_the_selection_is_a_generation_without_an_offer() {
        let mut tracker = SelectionTracker::new(5);
        tracker.introduce(1);
        tracker.select(Some(1));
        assert_eq!(tracker.select(None), Some(1));

        let current = tracker.current();
        assert_eq!(current.generation, 7);
        assert!(current.offer.is_none());
    }

    #[test]
    fn re_announcing_the_current_offer_changes_nothing() {
        let mut tracker = SelectionTracker::new(0);
        tracker.introduce(1);
        tracker.advertise(&1, "text/plain".to_owned());
        tracker.select(Some(1));
        assert_eq!(tracker.select(Some(1)), None);

        assert_eq!(tracker.generation(), 1);
        assert_eq!(mime_types(&tracker), ["text/plain"]);
    }

    #[test]
    fn discarded_offers_never_become_the_selection_types() {
        // A primary-selection offer is introduced like any other; discarding
        // it must not leak its types into a later selection of another offer.
        let mut tracker = SelectionTracker::new(0);
        tracker.introduce(1);
        tracker.advertise(&1, "text/html".to_owned());
        assert_eq!(tracker.discard(1), 1);
        tracker.advertise(&1, "text/plain".to_owned());

        tracker.introduce(2);
        tracker.advertise(&2, "image/png".to_owned());
        tracker.select(Some(2));

        assert_eq!(tracker.generation(), 1);
        assert_eq!(mime_types(&tracker), ["image/png"]);
    }
}
