use std::{
    ops::{Deref, DerefMut},
    sync::{Condvar, Mutex, PoisonError},
};

use nagori_core::Result;
use rusqlite::Connection;

use super::convert::lock_err;

/// Number of physical `SQLite` connections we keep around for file-backed
/// stores.
///
/// The previous design held a single `Mutex<Connection>` and serialised every
/// read against every write. With WAL mode, `SQLite` already supports many
/// concurrent readers plus one writer on separate connections — so a small
/// pool lets the search fan-out (substring/FTS/ngram), preview hydration,
/// and capture writes proceed in parallel instead of queueing on one
/// process-wide mutex. Four is enough to soak up the hybrid search fan-out
/// (3 reads) plus an in-flight write without blocking, while keeping the
/// per-process file-descriptor cost bounded.
pub(crate) const POOL_CAPACITY: usize = 4;

/// Bounded pool of `SQLite` connections.
///
/// `slots` holds whichever connections are currently idle. Acquirers pop the
/// front of the vector and return the connection on guard drop, notifying
/// any thread waiting in `available`. A pool with `capacity == 1` collapses
/// to today's single-`Mutex<Connection>` semantics — used for in-memory test
/// stores where each `Connection::open_in_memory` would create an entirely
/// separate database.
pub(crate) struct ConnPool {
    pub(crate) slots: Mutex<Vec<Connection>>,
    pub(crate) available: Condvar,
}

impl ConnPool {
    pub(crate) fn acquire(&self) -> Result<PooledConn<'_>> {
        let mut slots = self.slots.lock().map_err(|err| lock_err(&err))?;
        while slots.is_empty() {
            slots = self.available.wait(slots).map_err(|err| lock_err(&err))?;
        }
        let conn = slots.pop().expect("non-empty after wait");
        Ok(PooledConn {
            conn: Some(conn),
            pool: self,
        })
    }

    fn release(&self, conn: Connection) {
        // The slots mutex only ever guards push/pop of idle connection handles
        // — no fallible work runs under it — so a poisoned lock can only mean an
        // unrelated thread panicked while holding the guard, not that the pool
        // invariant is broken. Recover the guard with `into_inner` rather than
        // dropping the connection on the floor: silently discarding it would
        // shrink the pool by one for the rest of the process, and skipping
        // `notify_one` would leave an `acquire` waiter parked forever (the bug a
        // bare `if let Ok(..)` introduced).
        let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        slots.push(conn);
        self.available.notify_one();
    }
}

/// RAII guard for a connection borrowed from a [`ConnPool`].
///
/// Drop returns the connection so callers don't need to release manually,
/// even on panic. The `Deref`/`DerefMut` impls make `PooledConn` a drop-in
/// replacement for the previous `MutexGuard<Connection>` callsites.
pub(crate) struct PooledConn<'a> {
    conn: Option<Connection>,
    pool: &'a ConnPool,
}

impl Deref for PooledConn<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.conn.as_ref().expect("connection live")
    }
}

impl DerefMut for PooledConn<'_> {
    fn deref_mut(&mut self) -> &mut Connection {
        self.conn.as_mut().expect("connection live")
    }
}

impl Drop for PooledConn<'_> {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            self.pool.release(conn);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool_of(capacity: usize) -> ConnPool {
        let slots = (0..capacity)
            .map(|_| Connection::open_in_memory().expect("open in-memory conn"))
            .collect();
        ConnPool {
            slots: Mutex::new(slots),
            available: Condvar::new(),
        }
    }

    #[test]
    fn release_returns_connections_so_repeated_acquire_never_deadlocks() {
        let pool = pool_of(2);
        // Acquire to capacity, then drop, more times than the pool is deep.
        // Each guard drop must return its connection so the next round finds
        // a free slot — a leak here would deadlock the second iteration.
        for _ in 0..10 {
            let a = pool.acquire().unwrap();
            let b = pool.acquire().unwrap();
            assert!(
                pool.slots.lock().unwrap().is_empty(),
                "both connections are checked out"
            );
            drop(a);
            drop(b);
            assert_eq!(
                pool.slots.lock().unwrap().len(),
                2,
                "every connection must return to the pool on drop"
            );
        }
    }

    #[test]
    fn acquire_parks_until_a_connection_is_released_then_wakes_the_waiter() {
        use std::sync::mpsc;
        use std::time::Duration;

        // Leak the pool to `'static` and run the waiter on a *detached* thread
        // (not `thread::scope`): if `release`/`notify_one` regresses, the
        // waiter parks forever, and a scoped join would then hang the whole
        // suite. Detached + `recv_timeout` turns that regression into a clean
        // test *failure* instead — the abandoned thread is never joined.
        let pool: &'static ConnPool = Box::leak(Box::new(pool_of(1)));

        // Hold the sole connection: a second acquirer has no slot and must
        // park on the condvar rather than spin or fail.
        let held = pool.acquire().unwrap();

        let (acquired_tx, acquired_rx) = mpsc::channel();
        std::thread::spawn(move || {
            // Blocks inside `acquire` until the held connection is released.
            let _conn = pool.acquire().unwrap();
            let _ = acquired_tx.send(());
        });

        // Give the waiter ample time to reach `Condvar::wait` and park. The
        // duration is not asserted on — it only ensures the waiter is parked
        // *before* the release, so the wakeup genuinely depends on
        // `notify_one` rather than the waiter arriving late to a free slot.
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            acquired_rx.try_recv().is_err(),
            "waiter must park while the only connection is held"
        );

        // Releasing notifies the condvar and hands the connection over. A
        // broken `release`/`notify_one` leaves the waiter parked, so the
        // `recv_timeout` fails the test rather than blocking forever.
        drop(held);
        acquired_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("releasing a connection must wake the parked waiter");
    }

    #[test]
    fn release_recovers_a_poisoned_mutex_instead_of_dropping_the_connection() {
        use std::panic::{AssertUnwindSafe, catch_unwind};

        let pool = pool_of(1);
        // Check the sole connection out, then poison the slots mutex by
        // panicking while a separate guard is held. The guard is dropped
        // mid-unwind, so the mutex is now poisoned but the held `PooledConn`
        // still owns the connection.
        let held = pool.acquire().unwrap();
        let _ = catch_unwind(AssertUnwindSafe(|| {
            let _guard = pool.slots.lock().unwrap();
            panic!("poison the slots mutex");
        }));

        // Returning the connection must still push it back even though the
        // mutex is poisoned — a bare `if let Ok(..)` would have dropped it on
        // the floor and never notified, permanently shrinking the pool.
        drop(held);
        let recovered = pool.slots.lock().unwrap_or_else(PoisonError::into_inner);
        assert_eq!(
            recovered.len(),
            1,
            "release must recover the poisoned guard and return the connection",
        );
    }
}
