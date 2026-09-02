//! The seam that makes the reentrancy deadlock **unrepresentable** from Java.
//!
//! # What it is
//!
//! A request/response channel pair. [`WorldPort`] is the only thing a JNI
//! callback is ever handed; [`PortServicer`] is the tick side that answers it.
//! Neither carries a `World`, an `EcsHandle`, or a lock guard.
//!
//! # The problem this exists for
//!
//! `lodestone_ecs::EcsHandle` is `Arc<parking_lot::RwLock<World>>` and is **not
//! reentrant**. A second guard taken on one thread while the first is held
//! cannot make progress — three of the four combinations deadlock always, and
//! the fourth deadlocks whenever a writer is queued. That is not hypothetical
//! here: it froze this client outright on the first tick of the first block
//! dig, with no panic, no error and no log line, because a system called a
//! read-model accessor from inside the schedule's write guard.
//!
//! A Bukkit plugin is the easiest possible way to reintroduce it. Plugins
//! assume one main thread and call `world.getBlockAt()` freely from inside an
//! event handler — so a design that dispatches to Java *while holding the
//! guard* and lets the handler call back re-creates the exact shape, one JNI
//! frame deeper, where no Rust stack trace will show it.
//!
//! # How it works — the three properties, and what enforces each
//!
//! **1. A Java handler runs on its own thread, never on the tick thread.**
//! The tick thread's role during dispatch is to *service* the port, not to run
//! the handler. This is what makes the rest possible: the guard is only ever
//! taken by the servicing thread, and that thread is by construction not inside
//! one when it dispatches.
//!
//! **2. The Java side has no route to the lock — enforced by the type, not by
//! discipline.** [`WorldPort`] holds a `SyncSender` and a `Duration`. There is
//! no field from which a `World`, an `EcsHandle` or a guard can be reached, and
//! no constructor that takes one ([`channel`] is the only way to make one, and
//! it takes neither). So the worst a misbehaving handler can do is send a
//! request nobody answers — which is a *timeout*, reported, not a hang. This is
//! the same reasoning `docs/plugin-api.md` records for the sanctioned plugin
//! surface ("candidate 1: never place `EcsHandle` on the surface a plugin
//! depends on") and for `AsyncTaskPool::spawn`'s parameterless closure: give
//! the callee no argument capable of reaching the lock and reentrancy stops
//! being a discipline problem.
//!
//! **3. Wiring the servicer inside a guard is a loud panic, not a hang.**
//! [`service_with_world`] takes the guard *itself*, once per request, through
//! `lodestone_ecs::hold_write`. If a host ever calls it from inside an existing
//! guard, `hold_write`'s thread-local ledger fires and panics naming **both**
//! call sites, instead of wedging. That backstop already exists and is
//! deliberately reused rather than reimplemented — a second mechanism for one
//! invariant is how the two independent discoveries of the `thread::scope`
//! hazard happened.
//!
//! Taking the guard per request rather than once around the whole dispatch is
//! also what keeps `EcsHandle`'s own safety argument intact: its bound is
//! *duration* — "no guard spans a frame" — and a guard held across a JNI round
//! trip would span an unbounded one.
//!
//! # Why generic over the request type
//!
//! The concrete request set is a function of the census
//! (`docs/java-plugin-bridge.md`), which measures roughly seven thousand
//! members. Writing a speculative enum of them now would be a guess wearing an
//! API's clothes, and this repo's rule is to delete code nothing uses. What is
//! *designed* here is the mechanism; `Req`/`Rsp` are the hole the measured
//! surface drops into.
//!
//! # How to change it
//!
//! - **Do not add a `World`, `EcsHandle`, or guard field to [`WorldPort`].**
//!   That single edit re-opens the deadlock, and nothing else in this file
//!   would need to change for it to compile. `tests/reentrancy.rs` greps for
//!   exactly that and fails; it lives in a different file from this one on
//!   purpose, because a source-grep gate placed inside the file it greps
//!   matches its own assertion string and passes with the real line deleted.
//! - **Do not remove the deadline.** An infinite `recv` on the Java side turns
//!   a servicer that died into the hang this module exists to prevent.
//!
//! # Configuration
//!
//! [`DEFAULT_REQUEST_DEADLINE`] — how long a Java-side call waits for the tick
//! thread before giving up.
//!
//! # Dependencies
//!
//! `lodestone-ecs` for `EcsHandle`/`hold_write`, and `std::sync::mpsc`.
//! Deliberately not `parking_lot` directly: this module must not be able to
//! name a lock type at all.

use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::time::Duration;

use lodestone_ecs::ecs::world::World;
use lodestone_ecs::{EcsHandle, hold_write};

/// How long a Java-side request waits for the tick thread to answer it.
///
/// Generous relative to a 50 ms tick — a request issued just as a long tick
/// begins must not fail spuriously — while still being an upper bound rather
/// than an infinity. A handler that hits this is looking at a servicer that
/// stopped, which is a bug to report, not to wait out.
pub const DEFAULT_REQUEST_DEADLINE: Duration = Duration::from_secs(2);

/// How many requests may be in flight before a sender blocks.
///
/// Bounded rather than unbounded so that a runaway handler applies
/// backpressure instead of growing a queue without limit. One is enough for the
/// common case (a handler is synchronous and waits for each answer); the extra
/// slack covers several plugin threads issuing concurrently.
const QUEUE_DEPTH: usize = 64;

/// One request, plus the private channel its answer comes back on.
struct Envelope<Req, Rsp> {
    request: Req,
    reply: SyncSender<Rsp>,
}

/// The Java side's only route to the world.
///
/// **Contains no lock, no guard, no `World` and no `EcsHandle`** — see this
/// module's doc. That absence is the design, not an implementation detail.
///
/// Cloneable and `Send`, so each attached JVM thread can hold one (Bukkit's
/// scheduler has async tasks, and JNI requires `AttachCurrentThread`; those
/// threads need a port of their own rather than sharing one by reference).
pub struct WorldPort<Req, Rsp> {
    requests: SyncSender<Envelope<Req, Rsp>>,
    deadline: Duration,
}

impl<Req, Rsp> Clone for WorldPort<Req, Rsp> {
    fn clone(&self) -> Self {
        Self {
            requests: self.requests.clone(),
            deadline: self.deadline,
        }
    }
}

impl<Req, Rsp> std::fmt::Debug for WorldPort<Req, Rsp> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorldPort")
            .field("deadline", &self.deadline)
            .finish_non_exhaustive()
    }
}

impl<Req, Rsp> WorldPort<Req, Rsp> {
    /// Ask the tick thread for something and wait for the answer.
    ///
    /// # Errors
    ///
    /// [`PortError::Closed`] if the servicer is gone, [`PortError::TimedOut`]
    /// if it did not answer within the deadline. Never blocks forever, which is
    /// the whole point: a Java handler that outlives its servicer gets an
    /// exception it can report, not a wedged JVM.
    pub fn request(&self, request: Req) -> Result<Rsp, PortError> {
        let (reply, answer) = sync_channel(1);
        let envelope = Envelope { request, reply };
        match self.requests.try_send(envelope) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => return Err(PortError::Saturated),
            Err(TrySendError::Disconnected(_)) => return Err(PortError::Closed),
        }
        match answer.recv_timeout(self.deadline) {
            Ok(response) => Ok(response),
            Err(RecvTimeoutError::Timeout) => Err(PortError::TimedOut(self.deadline)),
            // The servicer dropped the reply sender without answering — it
            // panicked, or shut down mid-request.
            Err(RecvTimeoutError::Disconnected) => Err(PortError::Closed),
        }
    }

    /// This port's request deadline.
    #[must_use]
    pub const fn deadline(&self) -> Duration {
        self.deadline
    }
}

/// The tick side: answers what [`WorldPort`] asks.
///
/// Not `Clone`: there is exactly one servicer, on the thread that owns the
/// tick, and duplicating it would let two threads take world guards for the
/// same port.
pub struct PortServicer<Req, Rsp> {
    requests: Receiver<Envelope<Req, Rsp>>,
}

impl<Req, Rsp> std::fmt::Debug for PortServicer<Req, Rsp> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PortServicer").finish_non_exhaustive()
    }
}

/// Create a connected [`WorldPort`]/[`PortServicer`] pair.
///
/// Takes no handle and no world, which is what makes the port's lack of a route
/// to the lock a property of the *type* rather than of how a caller happened to
/// build it.
#[must_use]
pub fn channel<Req, Rsp>(deadline: Duration) -> (WorldPort<Req, Rsp>, PortServicer<Req, Rsp>) {
    let (requests, rx) = sync_channel(QUEUE_DEPTH);
    (
        WorldPort { requests, deadline },
        PortServicer { requests: rx },
    )
}

impl<Req, Rsp> PortServicer<Req, Rsp> {
    /// Answer one request if one is waiting, using `f`.
    ///
    /// Returns `false` when nothing was pending. Does not block.
    ///
    /// Prefer [`service_with_world`] where an `EcsHandle` is involved: this
    /// entry point cannot check the caller's guard state, so a host that calls
    /// it from inside a guard and then takes a second one inside `f` is back to
    /// the hazard.
    pub fn service_pending(&self, f: impl FnOnce(Req) -> Rsp) -> bool {
        match self.requests.try_recv() {
            Ok(envelope) => {
                let response = f(envelope.request);
                // A closed reply channel means the requester timed out and
                // walked away. Dropping the answer is correct; it already has
                // its `PortError::TimedOut`.
                let _ = envelope.reply.send(response);
                true
            }
            Err(_) => false,
        }
    }

    /// Answer every request currently queued, returning how many were served.
    ///
    /// Bounded by `max` so that a handler issuing requests in a tight loop
    /// cannot hold the tick thread in this function indefinitely — the same
    /// reasoning as bounding any per-tick drain.
    pub fn service_all_pending(&self, max: usize, mut f: impl FnMut(Req) -> Rsp) -> usize {
        let mut served = 0;
        while served < max && self.service_pending(&mut f) {
            served += 1;
        }
        served
    }
}

/// Service pending requests against a real `World`, taking a **short write
/// guard per request**.
///
/// This is the entry point a host should use, and the reason is enforcement
/// rather than convenience: `hold_write` consults `lodestone_ecs`'s
/// thread-local guard ledger, so calling this from inside an existing guard
/// **panics naming both call sites** instead of deadlocking. The rule "dispatch
/// to Java outside any guard" therefore stops being a comment and becomes
/// something the process checks every time it runs.
///
/// One guard per request, never one around the batch: `EcsHandle`'s safety
/// argument is that no guard spans a frame, and a guard held across a JNI round
/// trip would span an unbounded one.
///
/// Returns how many requests were answered.
///
/// # Panics
///
/// If the calling thread already holds a guard on `handle` — deliberately. See
/// above.
pub fn service_with_world<Req, Rsp>(
    servicer: &PortServicer<Req, Rsp>,
    handle: &EcsHandle,
    max: usize,
    mut f: impl FnMut(&mut World, Req) -> Rsp,
) -> usize {
    servicer.service_all_pending(max, |request| {
        hold_write(handle, |world| f(world, request))
    })
}

/// Why a [`WorldPort::request`] did not produce an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortError {
    /// The servicer is gone — the tick loop shut down, or the bridge was
    /// unloaded while a plugin thread was still running.
    Closed,
    /// More requests are in flight than the queue allows. Backpressure, not a
    /// fault: the caller should retry or fail the plugin call.
    Saturated,
    /// The servicer did not answer within the deadline. Distinguished from
    /// [`Self::Closed`] because they mean different things — a live but stalled
    /// tick thread versus a dead one — and conflating them would report a
    /// server hang as a shutdown.
    TimedOut(Duration),
}

impl std::fmt::Display for PortError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Closed => f.write_str("the world servicer is no longer running"),
            Self::Saturated => f.write_str("too many world requests in flight"),
            Self::TimedOut(d) => write!(f, "the world servicer did not answer within {d:?}"),
        }
    }
}

impl std::error::Error for PortError {}

#[cfg(test)]
mod tests {
    use super::*;

    /// A request with no servicer must fail fast rather than block a plugin
    /// thread forever — the property the deadline exists for.
    #[test]
    fn a_request_with_no_servicer_reports_closed() {
        let (port, servicer) = channel::<u32, u32>(Duration::from_millis(50));
        drop(servicer);
        assert_eq!(port.request(7), Err(PortError::Closed));
    }

    /// A live servicer that never answers must time out, and must be
    /// distinguishable from a dead one.
    #[test]
    fn a_silent_servicer_times_out_rather_than_hanging() {
        let deadline = Duration::from_millis(80);
        let (port, servicer) = channel::<u32, u32>(deadline);
        // Held, so the channel is open — but never serviced.
        let err = port.request(7).expect_err("must not answer");
        assert_eq!(err, PortError::TimedOut(deadline));
        drop(servicer);
    }

    /// The ordinary path, and the control on the two failure tests above: with
    /// a servicer actually running, the same call succeeds. Without this, both
    /// tests above would pass against a port that could never work at all.
    #[test]
    fn a_serviced_request_returns_the_answer() {
        let (port, servicer) = channel::<u32, u32>(Duration::from_secs(1));
        let worker = std::thread::spawn(move || {
            // The value is deliberately not an identity or a doubling: a
            // transform that shares a fixed point with the input would let a
            // servicer that echoed its request pass.
            while !servicer.service_pending(|req| req * 3 + 1) {
                std::thread::yield_now();
            }
            servicer
        });
        assert_eq!(port.request(11), Ok(34));
        drop(worker.join().expect("servicer thread"));
    }
}
