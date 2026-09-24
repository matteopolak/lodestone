//! A guest that never returns is interrupted, marked failed, and never called
//! again — the failure isolation `docs/plans/runtime-plugin-loading.md` names as
//! this tier's strongest argument beyond portability.
//!
//! # Why this test exists in the shape it does
//!
//! A fuel-exhaustion test pointed at a well-behaved plugin measures nothing: the
//! plugin returns long before the budget runs out, so the assertion passes whether
//! fuel is configured or not. That is the *world* species of vacuous test —
//! unreadable from the test source, because the flaw lives in the input. So the
//! subject here is a fixture built from the example plugin's own source with
//! `--features spin`, which spins forever, and the **control** is the well-behaved
//! artifact under the *same* fuel budget.
//!
//! # A note on what is not proven here
//!
//! This proves *fuel* preempts. It does not prove epoch interruption works, because
//! this crate does not use epochs — see `src/host.rs`'s header for why (an epoch
//! deadline with no watchdog to increment the epoch is a deadline that can never
//! trip). Epochs and the watchdog thread that would make them real remain future
//! work, not yet built.

mod support;

use lodestone_wasm_host::{Capability, CapabilitySet, ChatKind, ChatMessage, Event, PluginHost};

fn chat(text: &str) -> Event {
    Event::Chat(ChatMessage {
        text: text.to_owned(),
        kind: ChatKind::Chat,
    })
}

fn capabilities() -> CapabilitySet {
    CapabilitySet::from_iter([Capability::Log, Capability::ObserveChat, Capability::ActChat])
}

/// The budget both arms run under. Low enough that a spin loop dies quickly, high
/// enough that real work finishes — and the same number for subject and control, so
/// the difference between them cannot be the budget.
const FUEL: u64 = 2_000_000;

#[test]
fn a_guest_that_spins_forever_is_preempted_and_permanently_failed() {
    let wasm = support::build_example_plugin(&["spin"]);
    let mut host = PluginHost::new(CapabilitySet::default_policy())
        .expect("engine")
        .with_fuel(FUEL);

    // It loads: a spinning plugin is a *valid* plugin, and `init` returns normally.
    // Refusing it at load time would be a different (and impossible) feature —
    // nothing can tell from a module that it will not terminate.
    host.load_file("spinner", &wasm, &capabilities())
        .expect("a spinning plugin is still a well-formed one and must load");

    let actions = host.tick_all(&[chat("ping")]);
    assert!(
        actions.is_empty(),
        "a preempted guest must produce no actions, got {actions:?}"
    );

    let failure = host.plugins()[0]
        .failure()
        .expect("the guest must be marked failed")
        .to_owned();
    assert!(
        failure.contains("fuel"),
        "the failure must be attributed to fuel exhaustion, not to something \
         incidental; got:\n{failure}"
    );

    // And it is not retried. A host that re-entered a poisoned `Store` every tick
    // would burn the whole budget forever instead of once.
    assert!(host.tick_all(&[chat("ping")]).is_empty());
    assert_eq!(
        host.plugins()[0].failure().map(str::to_owned),
        Some(failure),
        "the recorded failure must not be overwritten by later ticks"
    );
}

/// **THE CONTROL.** The well-behaved artifact, same host configuration, same
/// `FUEL`. It completes and is not marked failed — so the test above is about the
/// guest's behaviour and not about the budget being impossibly small.
#[test]
fn the_control_a_well_behaved_guest_completes_under_the_same_budget() {
    let wasm = support::build_example_plugin(&[]);
    let mut host = PluginHost::new(CapabilitySet::default_policy())
        .expect("engine")
        .with_fuel(FUEL);
    host.load_file("chat-responder", &wasm, &capabilities())
        .expect("load");

    let actions = host.tick_all(&[chat("ping")]);
    assert_eq!(actions.len(), 1, "got {actions:?}");
    assert_eq!(
        host.plugins()[0].failure(),
        None,
        "a well-behaved guest must not be marked failed under a budget that only \
         a spin loop exhausts"
    );
}
