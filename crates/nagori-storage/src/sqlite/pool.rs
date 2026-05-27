use std::{
    ops::{Deref, DerefMut},
    sync::{Condvar, Mutex},
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
        if let Ok(mut slots) = self.slots.lock() {
            slots.push(conn);
            self.available.notify_one();
        }
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
