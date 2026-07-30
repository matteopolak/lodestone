//! The handle non-driver code uses to reach a live `World`.

use std::cell::RefCell;
use std::panic::Location;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use bevy_ecs::resource::Resource;
use bevy_ecs::world::World;
use parking_lot::RwLock;

/// A cheap, cloneable handle onto a live [`World`], for readers that are not
/// the schedule driver itself — async bot code, `ClientHandle`, anything off
/// the driver's own thread.
///
/// `docs/bevy-migration.md` §4.1(c), as shipped: since the unification the
/// driver holds this too, because there is exactly one `World` and the net
/// thread's ingest writes into it. azalea's `Client` wraps the same shape
/// (`azalea-client/src/client.rs:143`) so bot code can read and write from async
/// context (`azalea/src/bot.rs:85`). `parking_lot` rather than
/// `std::sync::RwLock` per §11, matching azalea's choice for the same lock.
///
/// # Lock discipline — three rules, and the reasons are not style
///
/// 1. **Never hold a guard across a call that might take the same lock.** The
///    driver must not hold one while calling into `NetClient`/`ClientHandle`:
///    most read-model accessors on those lock this same `World`, and
///    `parking_lot::RwLock` is neither reentrant nor upgradable, so
///    `write()` → `…read()` on one thread is an instant deadlock and
///    `read()` → `read()` deadlocks too whenever a writer is already queued
///    (that is what `read_recursive` exists for, and we do not rely on it).
///    Take the lock for one statement, or one `run_schedule`, and let it go.
///
///    **This one is now enforced, not merely documented** — [`hold_read`] and
///    [`hold_write`] panic on a second guard from the same thread instead of
///    hanging. See [`check`]. It was documented before and still shipped a
///    total client freeze the first tick of a block dig (`accb993`), which is
///    what a "don't do this" comment is worth against a silent hang.
///
///    Note the qualifier "most": the **chunk**-backed accessors (`block_at`,
///    `sections_and_light_at`, `world_dimensions`, `loaded_chunks`) take the
///    *chunk* lock, not this one, and are legal from inside a guard. The
///    rule as first written said "every read", which was too strong; the
///    §4.1(c) audit corrected it, and then the correction was over-read as
///    clearing `ClientHandle` generally, which is what shipped the freeze.
/// 2. **Never hold a guard across an `.await`.** `lodestone_client`'s driver
///    already promised this for the scalar read-model
///    (`state.rs`); it now matters for this lock too, because a task parked with
///    the `World` write-locked would stall the frame.
/// 3. **`World` before chunks, never the reverse.** The driver takes this lock
///    and *then* (inside a system) the `ChunkWorld` lock; the net thread takes
///    the chunk lock for `handle_packet` and releases it **before** folding
///    events, so it only ever takes this one afterward. Both orders are
///    `World → chunks`. Reversing either side is an ABBA deadlock, and nothing
///    in the type system stops it.
///
/// # What contention this actually creates
///
/// The net thread takes a short `write()` per folded `ClientEvent`; the driver
/// takes one per `run_schedule` and one per accessor. They now genuinely contend
/// where they did not before, and **§4.1(a)'s promise does not cover this lock**:
/// it says a slow frame "delays *application*, never *receipt*", which is true of
/// the socket→`ClientEvent` channel but false here, because
/// `lodestone_client::state::SharedState::apply` runs *inline in the driver task*
/// before `events.send(event).await`. Blocking on this lock therefore blocks the
/// task that reads the socket.
///
/// What bounds it is rule 1: **no guard spans a frame.** The driver takes many
/// short guards (one per `run_schedule`, one per accessor) rather than one long
/// one, so the worst a packet waits is one guard hold — sub-millisecond against
/// keep-alive timeouts measured in seconds.
///
/// That bound is no longer only structural: every guard taken through
/// [`hold_read`] / [`hold_write`] folds its own duration into the [`LockHolds`]
/// resource, so "the longest hold" is a number a test can read rather than an
/// argument from reading the code. See `docs/world-unification.md`.
pub type EcsHandle = Arc<RwLock<World>>;

/// Cumulative guard-hold statistics for the one `World`, folded in by
/// [`hold_read`] and [`hold_write`].
///
/// # Why this exists
///
/// [`EcsHandle`]'s whole safety argument is *duration*: the driver and the net
/// thread genuinely contend for this lock, and what keeps a packet from waiting a
/// whole frame is that no guard spans one. §4.1(c) shipped that as a claim
/// "counted from the code", which is exactly the shape `CLAUDE.md` calls the
/// *duration* species of vacuous test — a property of the system that no test
/// looks at. This is the counter that looks at it.
///
/// # Why interior atomics rather than `&mut`
///
/// A **read** guard yields `&World`, so a meter that needed `&mut self` could not
/// be updated from inside one — the reads are most of the guards, and a meter
/// that could only see the writes would report a bound that excludes the majority
/// of the holds. `Relaxed` throughout: these are diagnostics, nothing branches on
/// them, and no other memory is published through them.
///
/// # What it does *not* measure
///
/// - **Wait time.** The clock starts *after* acquisition, so this is "how long we
///   held it", which is what bounds another thread's wait. Time spent blocked
///   waiting for the lock is deliberately not in here; it is the other side of
///   the same coin and would make the number un-attributable.
/// - **The net thread's own holds**, unless `lodestone_client`'s
///   `SharedState::apply` is routed through [`hold_write`] too. It is one
///   `run_schedule(NetIngest)` per event.
/// - **The drop.** Recording happens while the guard is still held, so the
///   unlock itself (tens of nanoseconds) is outside the measurement.
#[derive(Resource, Debug, Default)]
pub struct LockHolds {
    holds: AtomicU64,
    total_ns: AtomicU64,
    longest_ns: AtomicU64,
}

/// An owned, `Copy` snapshot of [`LockHolds`] — what a caller outside the guard
/// can look at.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HoldStats {
    /// How many guards have been taken and released.
    pub holds: u64,
    /// Summed hold duration, in nanoseconds.
    pub total_ns: u64,
    /// The single longest hold, in nanoseconds. This is the number that bounds
    /// how long an ingest write can be kept waiting.
    pub longest_ns: u64,
}

impl LockHolds {
    /// Fold one guard hold in. Public so a caller that took its own guard (rather
    /// than going through [`hold_read`]/[`hold_write`]) can still be measured.
    pub fn record(&self, held: Duration) {
        let ns = u64::try_from(held.as_nanos()).unwrap_or(u64::MAX);
        self.holds.fetch_add(1, Ordering::Relaxed);
        self.total_ns.fetch_add(ns, Ordering::Relaxed);
        self.longest_ns.fetch_max(ns, Ordering::Relaxed);
    }

    /// Read the three counters out.
    ///
    /// Not atomic *as a set* — the three loads are independent — which is fine
    /// for a diagnostic and would only matter if another thread were recording
    /// concurrently. A test that wants a clean interval calls [`Self::reset`]
    /// first and measures on one thread.
    #[must_use]
    pub fn snapshot(&self) -> HoldStats {
        HoldStats {
            holds: self.holds.load(Ordering::Relaxed),
            total_ns: self.total_ns.load(Ordering::Relaxed),
            longest_ns: self.longest_ns.load(Ordering::Relaxed),
        }
    }

    /// Zero the counters, so a caller can measure one interval rather than the
    /// process's whole history.
    pub fn reset(&self) {
        self.holds.store(0, Ordering::Relaxed);
        self.total_ns.store(0, Ordering::Relaxed);
        self.longest_ns.store(0, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------
// Rule 1, enforced rather than reviewed
// ---------------------------------------------------------------------------

/// One guard this thread is currently holding: which handle, whether it is a
/// writer, and where it was taken.
#[derive(Debug, Clone, Copy)]
struct Held {
    /// The `RwLock`'s address, so two different `World`s do not alias. Compared,
    /// never dereferenced.
    handle: usize,
    write: bool,
    at: &'static Location<'static>,
}

thread_local! {
    /// The guards *this thread* holds, innermost last.
    ///
    /// Thread-local because reentrancy is a per-thread property: the driver taking
    /// a write guard while the net thread waits for a read is the ordinary
    /// contention [`EcsHandle`] is designed around, and only a *second* take on the
    /// *same* thread is the fatal shape.
    static HELD: RefCell<Vec<Held>> = const { RefCell::new(Vec::new()) };
}

/// Pushes a [`Held`] for as long as the guard lives, checking rule 1 first.
///
/// A `Drop` guard rather than a matching `pop` call, so an unwinding `f` — a bevy
/// system panicking inside `run_schedule`, an `expect` on a missing resource —
/// leaves the thread's ledger clean instead of poisoning every later guard on
/// that thread with a phantom holder.
struct Ledger;

impl Ledger {
    /// # Panics
    ///
    /// If this thread already holds a guard on the same handle in a combination
    /// that cannot make progress. See [`check`](Self::check).
    #[track_caller]
    fn enter(handle: &EcsHandle, write: bool) -> Self {
        let entry = Held {
            handle: Arc::as_ptr(handle) as usize,
            write,
            at: Location::caller(),
        };
        HELD.with_borrow_mut(|held| {
            check(held, &entry);
            held.push(entry);
        });
        Self
    }
}

impl Drop for Ledger {
    fn drop(&mut self) {
        HELD.with_borrow_mut(|held| {
            held.pop();
        });
    }
}

/// Rule 1 as a function: refuse a second guard on a handle this thread is
/// already inside.
///
/// # Why this is a panic and not a comment
///
/// `parking_lot::RwLock` is neither reentrant nor upgradable, so three of the four
/// combinations **cannot make progress at all**:
///
/// | held | requested | outcome |
/// |---|---|---|
/// | write | read | deadlock, always |
/// | write | write | deadlock, always |
/// | read | write | deadlock, always |
/// | read | read | deadlock **whenever a writer is queued** |
///
/// The first three abort in every build. The fourth is a real defect — it is why
/// `read_recursive` exists and why this crate does not rely on it — but it is
/// *conditional*, so making it fatal in a release build would trade an
/// intermittent hang for a certain crash on paths that happen to work today. It
/// aborts under `debug_assertions` (so tests and dev builds see it) and is left
/// alone in release.
///
/// A hang is the worst failure mode this repo has: `accb993` froze the whole
/// client on the first tick of the first block dig, with no panic, no error and no
/// log line, because a `GameTick` system called `ClientHandle::player_menu` — a
/// `World` read — from inside the schedule's write guard. This turns that into a
/// message naming both sites.
fn check(held: &[Held], want: &Held) {
    let Some(outer) = held.iter().rev().find(|h| h.handle == want.handle) else {
        return;
    };
    let fatal = outer.write || want.write;
    if !fatal && !cfg!(debug_assertions) {
        return;
    }
    let outer_kind = if outer.write { "write" } else { "read" };
    let want_kind = if want.write { "write" } else { "read" };
    let why = if fatal {
        "parking_lot's RwLock is not reentrant, so this can never make progress"
    } else {
        "parking_lot's read() queues behind a waiting writer, so this deadlocks \
         intermittently — whenever the net thread happens to want the lock"
    };
    panic!(
        "reentrant World guard: a {want_kind} guard was requested at {want_at} while \
         this thread's {outer_kind} guard from {outer_at} is still held. {why}.\n\
         \n\
         This is EcsHandle's lock rule 1. The usual cause is code inside the guard \
         calling out to `NetClient`/`ClientHandle` — most of its read-model \
         accessors take a read guard on this same World. From a system, read the \
         component out of the World you are already in; there is only one World \
         (see docs/world-unification.md). Chunk-backed reads (`block_at`, \
         `sections_and_light_at`, `world_dimensions`, `loaded_chunks`) take the \
         chunk lock instead and are fine.",
        want_at = want.at,
        outer_at = outer.at,
    );
}

/// Take a **read** guard on `handle`, run `f`, and fold the hold duration into
/// the `World`'s [`LockHolds`] — the measured form of rule 1's "one statement,
/// then let it go".
///
/// A `World` with no [`LockHolds`] (anything built by [`new_handle`] or
/// [`new_ingest_handle`] rather than by [`crate::CorePlugin`]) is simply
/// unmeasured, never a panic.
///
/// # Panics
///
/// If this thread already holds a guard on `handle` — see [`check`]. Prefer this
/// over `handle.read()` for exactly that reason: the bare lock hangs where this
/// reports.
#[track_caller]
pub fn hold_read<R>(handle: &EcsHandle, f: impl FnOnce(&World) -> R) -> R {
    let _ledger = Ledger::enter(handle, false);
    let world = handle.read();
    let started = Instant::now();
    let out = f(&world);
    if let Some(meter) = world.get_resource::<LockHolds>() {
        meter.record(started.elapsed());
    }
    out
}

/// [`hold_read`]'s **write** twin.
///
/// # Panics
///
/// If this thread already holds a guard on `handle` — see [`check`].
#[track_caller]
pub fn hold_write<R>(handle: &EcsHandle, f: impl FnOnce(&mut World) -> R) -> R {
    let _ledger = Ledger::enter(handle, true);
    let mut world = handle.write();
    let started = Instant::now();
    let out = f(&mut world);
    // `f` could in principle have removed the resource; `get_resource` tolerates
    // that rather than turning a diagnostic into a crash.
    if let Some(meter) = world.get_resource::<LockHolds>() {
        meter.record(started.elapsed());
    }
    out
}

/// Builds a fresh, empty [`World`] wrapped as an [`EcsHandle`].
///
/// Carries no resources of its own — see the note on [`crate::CorePlugin`]
/// about why inserting [`crate::WorldTime`] is left to whoever is making this
/// particular `World` authoritative, rather than done unconditionally here.
#[must_use]
pub fn new_handle() -> EcsHandle {
    Arc::new(RwLock::new(World::new()))
}

/// An [`EcsHandle`] onto a `World` that carries [`crate::CorePlugin`]'s
/// schedules, [`crate::ingest::IngestPlugin`]'s entity-ingest systems and
/// [`crate::SessionPlugin`]'s session folds — i.e. a `World` that is
/// *authoritative* over the network read-model and can be handed
/// `ClientEvent`s.
///
/// # Not the shell's path since §4.1(c)
///
/// `lodestone_shell::sim::Sim` builds the one `World` itself (with these plugins
/// among many others) and hands the handle *down* through `NetClient::connect` →
/// `ClientBuilder::ecs`. This constructor is for a client with **no driver** —
/// `lodestone_client::state::SharedState::default`, i.e. a bot or a test — which
/// legitimately owns its own `World`. Do not use it to make a second `World` in a
/// process that already has a driver: a component folded here would be invisible to
/// every one of that driver's systems, which is the defect §4.1(c) deleted.
///
/// The caller still has to [`crate::spawn_session`] the entity the session
/// components hang off: a plugin registers *behaviour*, and which entity is
/// "this client" is the owner's decision, exactly as
/// [`crate::spawn_local_player`] is not done by `LocalPlayerPlugin`.
///
/// The `App` is built only to run the plugins' `build` (which is the only way
/// to register schedules and systems) and is then discarded, keeping the
/// `World` it produced. That is azalea's own shape: it takes the `World` out of
/// the `App` and puts it behind an `Arc<RwLock<_>>`
/// (`azalea-client/src/client.rs:143`) because the driver is a hand-written
/// loop, not `App::run`. Nothing here calls `App::update`, so the discarded
/// `App`'s own `Main` schedule ordering is irrelevant; the caller runs named
/// schedules on the `World` directly (`world.run_schedule(NetIngest)`).
///
/// Carries no [`crate::WorldTime`] — see [`new_handle`] and
/// [`crate::CorePlugin`] on why inserting that is left to whoever is making a
/// particular `World` authoritative over the clock.
#[must_use]
pub fn new_ingest_handle() -> EcsHandle {
    let mut app = bevy_app::App::new();
    app.add_plugins((crate::ingest::IngestPlugin, crate::SessionPlugin));
    Arc::new(RwLock::new(std::mem::take(app.world_mut())))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The control for every hold bound in the tree.** An assertion that no
    /// guard is held for long is worth exactly as much as the evidence that the
    /// mechanism *would* have noticed one, so this holds a guard for a duration
    /// far outside the noise and observes the counter report it.
    ///
    /// Deliberately a sleep rather than a busy loop: `Instant::elapsed` is what
    /// [`hold_write`] measures with, and a sleep is the one way to make the
    /// expected value come from outside the code under test (the OS timer, not
    /// our own arithmetic).
    #[test]
    fn the_hold_meter_reports_a_deliberately_long_hold() {
        let handle = new_handle();
        handle.write().insert_resource(LockHolds::default());

        hold_write(&handle, |_| std::thread::sleep(Duration::from_millis(30)));

        let stats = hold_read(&handle, |w| w.resource::<LockHolds>().snapshot());
        assert_eq!(
            stats.holds, 1,
            "only the write is counted; the read that reads the counter records itself *after* snapshotting"
        );
        assert!(
            stats.longest_ns >= 25_000_000,
            "a 30 ms hold must be visible as at least 25 ms; got {} ns — the detector is broken, \
             which would make every 'no long hold' assertion vacuous",
            stats.longest_ns
        );
    }

    /// The other half of the control: a guard that does nothing must *not* look
    /// like a long hold. Without this, the assertion above is satisfied by a
    /// meter that reports a large constant.
    #[test]
    fn an_empty_hold_is_not_reported_as_a_long_one() {
        let handle = new_handle();
        handle.write().insert_resource(LockHolds::default());

        for _ in 0..100 {
            hold_write(&handle, |_| {});
        }

        let stats = hold_read(&handle, |w| w.resource::<LockHolds>().snapshot());
        assert_eq!(stats.holds, 100);
        assert!(
            stats.longest_ns < 1_000_000,
            "100 empty write guards must all be well under a millisecond; got {} ns",
            stats.longest_ns
        );
    }

    /// A `World` with no [`LockHolds`] is unmeasured rather than a panic —
    /// [`new_handle`] and [`new_ingest_handle`] both produce one, and
    /// `SharedState::default`'s bot path runs on exactly that.
    #[test]
    fn an_unmetered_world_is_not_a_panic() {
        let handle = new_handle();
        assert_eq!(hold_write(&handle, |_| 7), 7);
        assert_eq!(hold_read(&handle, |_| 7), 7);
    }

    // -----------------------------------------------------------------------
    // Rule 1's enforcement
    // -----------------------------------------------------------------------

    /// Runs `f` and reports whether it panicked, keeping the panic message off
    /// the test log.
    ///
    /// The hook swap is global and these tests run in one binary, so it is held
    /// for as short a window as possible. The alternative — letting four
    /// deliberate panics print — makes a real failure in this file impossible to
    /// spot.
    ///
    /// `AssertUnwindSafe` because the payload is always an [`EcsHandle`], which is
    /// not `RefUnwindSafe` (a `World` behind a lock never is). That is sound here
    /// for the reason the marker exists to check: every case below either drops the
    /// handle immediately or re-guards it through [`hold_write`], and the ledger's
    /// `Drop` is what guarantees the unwound guard is not still recorded.
    fn panicked(f: impl FnOnce()) -> bool {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        std::panic::set_hook(previous);
        out.is_err()
    }

    /// **The bug that shipped, as an assertion.** A read guard taken while this
    /// thread holds the write guard is the `accb993` client freeze; it must now
    /// panic rather than hang.
    ///
    /// This is what makes the whole rule testable at all: the failure it replaces
    /// has no observable behaviour to assert on — the process simply stops.
    #[test]
    fn a_read_inside_a_write_panics_instead_of_hanging() {
        let handle = new_handle();
        assert!(panicked(|| {
            hold_write(&handle, |_| {
                hold_read(&handle, |_| ());
            });
        }));
    }

    /// The other two always-fatal combinations, for completeness: a write inside
    /// a write, and a write inside a read.
    #[test]
    fn the_other_fatal_combinations_panic_too() {
        let handle = new_handle();
        assert!(panicked(|| hold_write(&handle, |_| hold_write(&handle, |_| ()))));
        assert!(panicked(|| hold_read(&handle, |_| hold_write(&handle, |_| ()))));
    }

    /// **The negative control.** Two *different* `World`s nest freely — the
    /// ledger keys on the handle's identity, so without this the check above is
    /// satisfied by one that fires on any nesting at all and would break every
    /// caller holding two worlds.
    #[test]
    fn guards_on_two_different_worlds_nest_freely() {
        let a = new_handle();
        let b = new_handle();
        assert!(!panicked(|| {
            hold_write(&a, |_| {
                hold_write(&b, |_| ());
                hold_read(&b, |_| ());
            });
        }));
    }

    /// **The second negative control.** Guards taken one *after* another are the
    /// normal case and must stay silent — otherwise the check would fire on every
    /// frame, since the driver takes many short guards per frame by design.
    #[test]
    fn sequential_guards_on_one_world_are_not_reentrancy() {
        let handle = new_handle();
        assert!(!panicked(|| {
            for _ in 0..10 {
                hold_write(&handle, |_| ());
                hold_read(&handle, |_| ());
            }
        }));
    }

    /// A panic *inside* the guard must not leave a phantom holder behind, or the
    /// first real guard after any system panic would report a bogus reentrancy
    /// and mask the actual failure. This is why the ledger entry is a `Drop`
    /// guard rather than a matching `pop`.
    #[test]
    fn an_unwinding_closure_leaves_the_ledger_clean() {
        let handle = new_handle();
        assert!(panicked(|| hold_write(&handle, |_| panic!("a system blew up"))));
        assert_eq!(
            hold_write(&handle, |_| 7),
            7,
            "the ledger still holds the unwound guard, so every later guard on this \
             thread now reports a reentrancy that is not there"
        );
    }

    /// [`LockHolds::reset`] zeroes the interval, which is what lets a caller
    /// measure one function rather than the process's history.
    #[test]
    fn reset_clears_the_interval() {
        let handle = new_handle();
        handle.write().insert_resource(LockHolds::default());
        hold_write(&handle, |_| std::thread::sleep(Duration::from_millis(5)));
        // Reset and snapshot under one guard: `hold_write` records *its own* hold
        // after `f` returns, so resetting in one guard and reading in the next
        // would always show the reader's own hold rather than zero.
        let stats = hold_write(&handle, |w| {
            let meter = w.resource::<LockHolds>();
            meter.reset();
            meter.snapshot()
        });
        assert_eq!(stats, HoldStats::default());
    }
}
