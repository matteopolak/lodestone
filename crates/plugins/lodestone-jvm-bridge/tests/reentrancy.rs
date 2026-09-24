//! The threading design's guards.
//!
//! # What this file asserts, and why each half is needed
//!
//! The bridge's central claim is that a JNI callback **cannot** reproduce the
//! `EcsHandle` reentrancy deadlock — the one that froze this client on the
//! first tick of the first block dig, with no panic and no log line. That claim
//! is made three ways, and each is checked here, because each fails
//! differently:
//!
//! | claim | mechanism | test |
//! |---|---|---|
//! | this crate has no *route* to an `EcsHandle` | dependency graph | [`this_crate_cannot_reach_an_ecs_handle`] |
//! | `WorldPort` has no *field* that reaches one | source grep | [`the_port_type_names_no_lock_or_handle`] |
//! | servicing a port under a real tick guard completes | runtime watchdog | [`a_java_style_callback_completes_under_a_real_tick_guard`] |
//!
//! # The controls
//!
//! Each gate is paired with something that must fail, because this repo has a
//! documented case of a search reporting absence because the search itself was
//! broken, and because an assertion of an absence is only worth the evidence
//! that its detector fires.
//!
//! [`the_grep_detector_finds_a_handle_where_one_really_is`] runs the source-grep
//! detector over `lodestone-plugin-support`'s reentrancy module, which genuinely
//! does name `EcsHandle`, so a detector that read nothing could not pass it.
//! [`the_watchdog_reports_a_wedge_for_the_shape_the_port_forbids`] wedges on
//! purpose — a raw `handle.read()` from inside a `hold_write`, the historical
//! bug exactly — proving the watchdog can tell a hang from a pass.
//!
//! # Why the grep gate lives here rather than in `port.rs`
//!
//! A source-grep gate placed inside the file it greps matches its **own**
//! assertion string and passes with the defended line deleted. That is a real
//! species of vacuous test, and the only thing that distinguishes a working
//! grep gate from a self-satisfying one is living in a different file.
//!
//! # A note on the wedge control and codegen backends
//!
//! The control wedges rather than panicking, deliberately. `within_budget`
//! reports a panicking worker as `Disconnected`, but a panic raised on a
//! *freshly spawned* thread aborts the process under this workspace's Cranelift
//! debug backend (the carve-out `Cargo.toml` documents for `lodestone-ecs` and
//! `lodestone-plugin-support`). A hang is backend-independent, is the shape the
//! historical bug actually had, and needs no profile entry in a shared
//! manifest.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lodestone_ecs::hold_write;
use lodestone_jvm_bridge::{ObjectKind, channel, service_with_world};
use lodestone_plugin_support::reentrancy::{assert_ecs_only_dependency_graph, within_budget};

fn workspace_root() -> PathBuf {
    // `CARGO_MANIFEST_DIR` is `crates/plugins/lodestone-jvm-bridge`.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("the crate has three ancestors up to the workspace root")
        .to_path_buf()
}

/// The static half, and the strongest of the three: a crate that cannot *name*
/// `lodestone-shell` or `lodestone-client` has no route to a real `EcsHandle`
/// at all, so the deadlock is not merely avoided but unrepresentable.
///
/// Reuses `lodestone-plugin-support`'s own check rather than restating it —
/// one mechanism per invariant, so a change to the doctrine lands in one place.
#[test]
fn this_crate_cannot_reach_an_ecs_handle() {
    assert_ecs_only_dependency_graph(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"));
}

/// Whether `text` contains a token that would give a type a route to the world
/// lock.
///
/// Substrings rather than a regex on purpose: this repo has a measured incident
/// where a guard's own pattern contained the table's field separator, grep
/// exited 2, and the `|| true` meant to swallow a no-match swallowed the error
/// identically — five rules reported PASS for weeks having measured nothing.
fn names_a_lock_route(text: &str) -> Vec<&'static str> {
    const FORBIDDEN: &[&str] = &["EcsHandle", "RwLock", "parking_lot", "MutexGuard"];
    FORBIDDEN
        .iter()
        .copied()
        .filter(|needle| text.contains(needle))
        .collect()
}

/// Only the declarations are scanned, not the prose: `port.rs`'s module
/// documentation discusses `EcsHandle` at length — that is the *point* of the
/// documentation — and a gate that matched comments would be unsatisfiable.
///
/// Comment stripping is line-oriented and does not understand strings, which is
/// fine here and would not be in general: a hand-rolled Rust lexer gets
/// lifetimes wrong (`&'static str` opening an unterminated char literal has
/// silently disabled comment detection in three scanners in this repo). This
/// one never tries to track literals, so it has no state to desynchronise.
fn code_only(text: &str) -> String {
    text.lines()
        .map(|line| line.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The type-level half: `WorldPort` must have no field, and `port.rs` no
/// declaration, from which the world lock can be reached. One added field would
/// re-open the deadlock while compiling perfectly.
///
/// `service_with_world` is the deliberate exception — it is the servicer side,
/// runs on the tick thread, and takes the guard *itself* so that
/// `hold_write`'s ledger can catch a host that wires it wrongly. It is
/// therefore allowed to name `EcsHandle`, and the assertion is scoped to the
/// part of the file above it.
#[test]
fn the_port_type_names_no_lock_or_handle() {
    let port_rs = workspace_root().join("crates/plugins/lodestone-jvm-bridge/src/port.rs");
    let text = std::fs::read_to_string(&port_rs).expect("read port.rs");

    // Premise: without this, an empty read would satisfy every assertion below.
    assert!(
        text.contains("pub struct WorldPort"),
        "premise failed: port.rs must declare WorldPort, or this gate is \
         measuring nothing"
    );

    let declarations = code_only(&text);
    let struct_body = declarations
        .split_once("pub struct WorldPort")
        .expect("WorldPort declaration")
        .1
        .split_once('}')
        .expect("WorldPort body ends")
        .0;

    let found = names_a_lock_route(struct_body);
    assert!(
        found.is_empty(),
        "WorldPort's fields name {found:?}. This type is the ONLY thing a JNI \
         callback holds, and its lack of a route to the world lock is what makes \
         the reentrancy deadlock unrepresentable rather than merely discouraged. \
         Adding such a field compiles fine and silently re-opens the bug that \
         froze this client on the first tick of the first block dig. Route world \
         access through WorldPort::request instead; see port.rs's module doc."
    );
}

/// The control for the grep detector: the same predicate over a file that
/// genuinely *does* name `EcsHandle` must find it. Without this, a detector
/// that read an empty string would pass the gate above forever.
#[test]
fn the_grep_detector_finds_a_handle_where_one_really_is() {
    let known = workspace_root().join("crates/plugins/lodestone-plugin-support/src/reentrancy.rs");
    let text = std::fs::read_to_string(&known).expect("read reentrancy.rs");
    let found = names_a_lock_route(&code_only(&text));
    assert!(
        found.contains(&"EcsHandle"),
        "control failed: the detector must find EcsHandle in a file that uses it, \
         or a clean result for WorldPort proves nothing. Found: {found:?}"
    );
}

/// The runtime half, and the shape that actually matters: a "Java handler" on
/// its own thread calls back into the world **while the tick thread holds a
/// real write guard shaped like the driver's**, and the whole thing completes.
///
/// This is the exact scenario the issue names as the hardest part — a Bukkit
/// handler calling `world.getBlockAt()` from inside an event — expressed
/// through the port. It must finish well inside the budget rather than wedge.
#[test]
fn a_java_style_callback_completes_under_a_real_tick_guard() {
    let handle = lodestone_ecs::new_handle();
    let (port, servicer) = channel::<u32, u32>(Duration::from_secs(2));

    // The "JVM thread": it does what a Bukkit handler does — reach back into
    // the world, twice, mid-handler.
    let plugin_thread = std::thread::spawn(move || {
        let first = port.request(11)?;
        let second = port.request(4)?;
        Ok::<_, lodestone_jvm_bridge::PortError>((first, second))
    });

    let servicing_handle = Arc::clone(&handle);
    let outcome = within_budget(Duration::from_secs(5), move || {
        // The tick thread's whole job during dispatch: service the port. It is
        // NOT inside a guard here — that is the design — and `service_with_world`
        // takes a short one per request.
        let mut served = 0;
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while served < 2 && std::time::Instant::now() < deadline {
            served += service_with_world(&servicer, &servicing_handle, 8, |_world, req| req * 3 + 1);
            std::thread::yield_now();
        }
        served
    });

    assert_eq!(
        outcome.expect("the servicing thread must not wedge"),
        2,
        "both callbacks must be served"
    );
    assert_eq!(
        plugin_thread.join().expect("plugin thread"),
        Ok((34, 13)),
        "the handler must receive real answers, not defaults"
    );

    // And the handle must still be usable afterwards — a leaked guard would
    // make this hang, which is the failure the whole design is about.
    let usable = within_budget(Duration::from_secs(2), move || {
        hold_write(&handle, |_world| 7)
    });
    assert_eq!(
        usable.expect("no guard may outlive the dispatch"),
        7,
        "the world lock must be free once dispatch is done"
    );
}

/// The control for the watchdog: the shape the port exists to forbid must
/// actually wedge. Without this, the test above passes for a harness that
/// could never detect a hang at all.
///
/// This reproduces the historical bug directly — a raw `handle.read()` taken
/// from inside a `hold_write`, bypassing the ledger that would otherwise panic
/// — on a handle used by nothing else, since the wedged thread keeps its guard
/// forever by design.
#[test]
fn the_watchdog_reports_a_wedge_for_the_shape_the_port_forbids() {
    let doomed = lodestone_ecs::new_handle();
    let outcome = within_budget(Duration::from_millis(400), move || {
        hold_write(&doomed, |_world| {
            // `hold_read` would panic here (the ledger sees both guards). The
            // raw method cannot be intercepted — `EcsHandle` is a type alias
            // for `Arc<RwLock<World>>`, so `.read()` is parking_lot's own
            // inherent method — and hangs instead. That gap is why the port
            // exists rather than relying on the ledger.
            let _guard = doomed.read();
        });
    });
    assert!(
        outcome.is_err(),
        "control failed: a reentrant raw read must wedge. If this returned Ok, \
         the watchdog cannot detect the deadlock and the passing gate above is \
         vacuous."
    );
    // The wedged thread is leaked on purpose; joining it is the one thing that
    // would turn this control into the hang it detects.
}

/// Identity's half of "fails gracefully rather than dangling": a handle to a
/// released object must report staleness, and must never resolve to whatever
/// later reuses its slot.
///
/// The slot reuse is forced rather than hoped for — release then immediately
/// re-register, which is exactly what a busy server does with entity ids — so
/// this is the discriminating input rather than a happy path that would pass
/// under a plain-index implementation too.
#[test]
fn a_released_handle_never_resolves_to_its_slots_next_occupant() {
    let mut registry = lodestone_jvm_bridge::ObjectRegistry::<u64>::new();

    let old = registry.handle_for(ObjectKind::Player, 11);
    assert_eq!(registry.resolve(old, ObjectKind::Player), Ok(&11));

    assert!(registry.release(&11), "the player was live");

    // The freed slot is reused here — a plain index would now answer for 4.
    let new = registry.handle_for(ObjectKind::Player, 4);
    assert_eq!(registry.resolve(new, ObjectKind::Player), Ok(&4));
    assert_eq!(
        registry.resolve(old, ObjectKind::Player),
        Err(lodestone_jvm_bridge::ResolveError::Stale),
        "the stale handle must fail, not resolve to the slot's new occupant"
    );

    // And a handle used at the wrong kind is a distinct, reported failure
    // rather than staleness — a plugin bug, reported as one.
    assert_eq!(
        registry.resolve(new, ObjectKind::Block),
        Err(lodestone_jvm_bridge::ResolveError::KindMismatch {
            expected: ObjectKind::Block,
            actual: ObjectKind::Player,
        })
    );
}

/// Re-exposing an object a plugin already holds must yield the **same** handle:
/// Bukkit plugins compare entity references for identity, and two live handles
/// to one entity would break `equals` in a way that presents as a plugin bug.
#[test]
fn one_object_has_exactly_one_live_handle() {
    let mut registry = lodestone_jvm_bridge::ObjectRegistry::<u64>::new();
    let first = registry.handle_for(ObjectKind::Entity, 11);
    let second = registry.handle_for(ObjectKind::Entity, 11);
    assert_eq!(first, second);
    assert_eq!(registry.len(), 1);
}
