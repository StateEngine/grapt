// Copyright 2025 OpenAI
// Licensed under the Apache License, Version 2.0.
// Modified by Grapt: serial channel deadline accounting, bounded nested
// requests and existing form cancellation; transport shares the scoped pause.
// (active_time_timeout), commit 092d3acd6bec3e3a14bdc7e7a2810ab628ab759d.
// Modified further: startup-only human lifetime/request cap, cumulative phase
// pause accounting and connection-revision propagation. Timeout/cancellation
// adaptation also inspected at bb64b894fd67158be0f874872e6d7bfe717e7368.

//! Bounded held connection ownership, retirement and observed wire status.
use grapt_mcp::session::Unusable;

/// Application-owned resources closed before a connection retires.
pub trait ConnectionResources: Default {
    fn close(&self);
}

/// A connection handle a caller can hold across a wait, and an owner can
/// RETIRE without waiting behind that caller's I/O.
///
/// Retirement is an atomic flag rather than state inside the mutex on purpose:
/// the point is to mark a connection dead while somebody else holds its lock
/// for up to the call budget. An owner that had to take the lock to retire a
/// connection could not retire the one that most needs it.
pub struct HeldIo<T, R: ConnectionResources> {
    pub resources: std::sync::Arc<R>,
    retired: std::sync::atomic::AtomicBool,
    retirement: tokio::sync::Notify,
    /// How many callers are waiting on this connection right now.
    ///
    /// A deadline bounds how LONG one waits, not how MANY do. Without this a
    /// connection can accumulate callers without limit — each holding an Arc,
    /// so each also keeps the child alive past any eviction. Admission is
    /// refused at the bound rather than queued behind it.
    waiting: std::sync::atomic::AtomicUsize,
    /// What a caller last OBSERVED of this connection: `None` until something
    /// goes wrong on the wire. Stored outside the mutex for the same reason
    /// retirement is — a status read must be able to see it without waiting
    /// behind the call that observed it.
    observed: std::sync::Mutex<Option<Unusable>>,
    io: tokio::sync::Mutex<T>,
}

impl<T, R: ConnectionResources> std::fmt::Debug for HeldIo<T, R> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HeldIo")
            .field("retired", &self.is_retired())
            .field("waiting", &self.waiting())
            .finish_non_exhaustive()
    }
}

impl<T, R: ConnectionResources> HeldIo<T, R> {
    pub fn new(io: T) -> Self {
        Self {
            resources: std::sync::Arc::default(),
            retired: std::sync::atomic::AtomicBool::new(false),
            retirement: tokio::sync::Notify::new(),
            waiting: std::sync::atomic::AtomicUsize::new(0),
            observed: std::sync::Mutex::new(None),
            io: tokio::sync::Mutex::new(io),
        }
    }

    /// Mark this connection dead. A caller already queued on it refuses rather
    /// than dispatching when it finally acquires it.
    pub fn retire(&self) {
        self.resources.close();
        self.retired
            .store(true, std::sync::atomic::Ordering::Release);
        self.retirement.notify_waiters();
    }

    pub async fn retired(&self) {
        loop {
            let notified = self.retirement.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.is_retired() {
                return;
            }
            notified.await;
        }
    }

    pub fn is_retired(&self) -> bool {
        self.retired.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Record what a caller observed. A DISCONNECT is durable knowledge about
    /// the connection; a timeout is not — it says nothing about whether the
    /// server is alive, only that it did not answer in time, so it is
    /// deliberately NOT recorded here.
    pub fn observe(&self, unusable: Unusable) {
        if !matches!(unusable, Unusable::Disconnected) {
            return;
        }
        match self.observed.lock() {
            Ok(mut slot) => *slot = Some(unusable),
            Err(poisoned) => *poisoned.into_inner() = Some(unusable),
        }
    }

    pub fn observed_unusable(&self) -> Option<Unusable> {
        match self.observed.lock() {
            Ok(slot) => slot.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// Take a place in the queue, or refuse. Released when the returned guard
    /// drops, however the caller leaves — including by timing out.
    pub fn enqueue(&self, bound: usize) -> Option<WaitTicket<'_>> {
        let mut current = self.waiting.load(std::sync::atomic::Ordering::Relaxed);
        loop {
            if current >= bound {
                return None;
            }
            match self.waiting.compare_exchange_weak(
                current,
                current + 1,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Relaxed,
            ) {
                Ok(_) => {
                    return Some(WaitTicket {
                        waiting: &self.waiting,
                    })
                }
                Err(observed) => current = observed,
            }
        }
    }

    pub fn waiting(&self) -> usize {
        self.waiting.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn io(&self) -> &tokio::sync::Mutex<T> {
        &self.io
    }
}

/// One admitted connection's claim on the aggregate bound.
///
/// Held BY the connection, so the claim is released exactly when the last
/// holder drops it — not when an owner removes it from a map. That is the
/// distinction the map bound could not make: an evicted connection a caller
/// still holds is still a process, and its slot must still be spent.
#[derive(Debug)]
pub struct ConnectionSlot {
    counter: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl ConnectionSlot {
    /// Take a slot, or refuse. The compare-exchange is what makes this a bound
    /// under concurrency rather than a check two callers can both pass.
    pub fn admit(
        counter: &std::sync::Arc<std::sync::atomic::AtomicUsize>,
        bound: usize,
    ) -> Option<Self> {
        let mut current = counter.load(std::sync::atomic::Ordering::Relaxed);
        loop {
            if current >= bound {
                return None;
            }
            match counter.compare_exchange_weak(
                current,
                current + 1,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Relaxed,
            ) {
                Ok(_) => {
                    return Some(Self {
                        counter: std::sync::Arc::clone(counter),
                    })
                }
                Err(observed) => current = observed,
            }
        }
    }
}

impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        self.counter
            .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

/// One caller's place in a connection's queue, released on drop.
#[derive(Debug)]
pub struct WaitTicket<'a> {
    waiting: &'a std::sync::atomic::AtomicUsize,
}

impl Drop for WaitTicket<'_> {
    fn drop(&mut self) {
        self.waiting
            .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}
