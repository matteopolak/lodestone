//! The unbounded-allocation half of issue #176's sandbox verification gate.
//!
//! # Why the control is "a looser ceiling grows further", not "the sandbox off"
//!
//! `PluginHost::new` always builds a `StoreLimits` and installs it with
//! `store.limiter(..)` (`src/host.rs`) — there is no public way to load a guest
//! with memory accounting turned off, by design: an *always-on* limiter is a
//! stronger property than a togglable one, and the crate is right not to expose
//! the off switch just so a test can exercise it. So the control this file needs
//! is not "run the same module with the limiter removed" — the only way to build
//! that host is to modify production code to weaken it for a test, which is
//! itself the failure mode `CLAUDE.md` warns about ("a neuter... must be short
//! and restored"), not a load-bearing negative control.
//!
//! Instead: the same guest, unmodified, run under two different
//! `with_memory_limit` values. If the limiter were built but never installed —
//! the exact "defaulted trait method plus a wrapper impl" shape this session's
//! brief calls out, or simply a forgotten `store.limiter(...)` call — both runs
//! would be capped at whatever `wasm32`'s own address-space ceiling or the
//! guest's own allocator happens to hit, **identically**, and the "tight" test
//! below would pass for the wrong reason: it would look like enforcement while
//! actually measuring a platform constant. Requiring the loose run to grow
//! *materially further* than the tight one is what makes that failure mode
//! visible instead of silently indistinguishable from success.

mod support;

use lodestone_wasm_host::{Action, Capability, CapabilitySet, ChatKind, ChatMessage, Event, PluginHost};

fn chat(text: &str) -> Event {
    Event::Chat(ChatMessage {
        text: text.to_owned(),
        kind: ChatKind::Chat,
    })
}

fn capabilities() -> CapabilitySet {
    CapabilitySet::from_iter([Capability::Log, Capability::ObserveChat, Capability::ActChat])
}

/// A ceiling well below the crate's own `DEFAULT_MEMORY_LIMIT`, so the test does
/// not depend on that constant's exact value.
const TIGHT_LIMIT: usize = 16 * 1024 * 1024;
/// A ceiling well above it — the "sandbox weakened" arm of the control.
const LOOSE_LIMIT: usize = 256 * 1024 * 1024;

/// Parse the fixture's own report, `"alloc: bytes=<N> denied=<bool>"`.
fn grown_bytes(actions: &[Action]) -> (usize, bool) {
    let reported = actions
        .iter()
        .find_map(|a| match a {
            Action::SendChat(t) if t.starts_with("alloc:") => Some(t.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("the fixture must report its allocation attempt; got {actions:?}"));

    let bytes: usize = reported
        .split("bytes=")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("could not parse a byte count out of: {reported}"));
    let denied = reported.contains("denied=true");
    (bytes, denied)
}

#[test]
fn unbounded_allocation_is_capped_by_the_configured_memory_limit() {
    let wasm = support::build_example_plugin(&["alloc-loop"]);
    let mut host = PluginHost::new(CapabilitySet::default_policy())
        .expect("engine")
        .with_memory_limit(TIGHT_LIMIT);
    host.load_file("hog", &wasm, &capabilities())
        .expect("a guest that merely allocates is well-formed and must load");

    let actions = host.tick_all(&[chat("go")]);
    let (grown, denied) = grown_bytes(&actions);

    assert!(
        denied,
        "the loop must report a denial once it hits the configured ceiling; grown={grown} actions={actions:?}"
    );
    assert!(
        grown <= TIGHT_LIMIT,
        "grew {grown} bytes past the configured {TIGHT_LIMIT}-byte ceiling — the limiter did not hold"
    );
    // Not merely "denied at the very first chunk for an unrelated reason" — the
    // guest genuinely got most of the way to the ceiling before being stopped.
    assert!(
        grown >= TIGHT_LIMIT / 2,
        "grew only {grown} of {TIGHT_LIMIT} configured bytes — suspiciously small; the fixture may not \
         be exercising the limiter at all"
    );

    // The guest is not marked failed: `try_reserve` denial is an ordinary `Err`
    // the guest handles itself, not a trap. Confirms the fixture measures a
    // graceful ceiling rather than accidentally proving the *preemption* fixture's
    // point instead.
    assert!(
        host.plugins()[0].failure().is_none(),
        "a `try_reserve` denial must not trap the guest: {:?}",
        host.plugins()[0].failure()
    );
}

/// THE CONTROL. See this file's header for what it proves and why it takes this
/// shape instead of an on/off sandbox toggle.
#[test]
fn the_control_a_looser_ceiling_lets_the_same_guest_grow_further() {
    let wasm = support::build_example_plugin(&["alloc-loop"]);

    let mut tight = PluginHost::new(CapabilitySet::default_policy())
        .expect("engine")
        .with_memory_limit(TIGHT_LIMIT);
    tight
        .load_file("hog", &wasm, &capabilities())
        .expect("load under the tight ceiling");
    let (tight_grown, tight_denied) = grown_bytes(&tight.tick_all(&[chat("go")]));

    let mut loose = PluginHost::new(CapabilitySet::default_policy())
        .expect("engine")
        .with_memory_limit(LOOSE_LIMIT);
    loose
        .load_file("hog", &wasm, &capabilities())
        .expect("load under the loose ceiling");
    let (loose_grown, loose_denied) = grown_bytes(&loose.tick_all(&[chat("go")]));

    // Both are eventually denied — the fixture's own loop cap (2 GiB) is still
    // far past `LOOSE_LIMIT` — so the discriminator is *how far each got*, not
    // whether either succeeded outright.
    assert!(tight_denied, "the tight host must still hit its ceiling");
    assert!(loose_denied, "the loose host must still hit its (much higher) ceiling");
    assert!(
        loose_grown > tight_grown * 4,
        "a host configured with a {LOOSE_LIMIT}-byte ceiling grew only {loose_grown} bytes, no further \
         than {tight_grown} bytes under a {TIGHT_LIMIT}-byte one — the configured limit is not what is \
         constraining growth, which means the limiter may be built but never installed"
    );
    assert!(
        loose_grown <= LOOSE_LIMIT,
        "the loose host still must not exceed its own configured ceiling: grew {loose_grown} of {LOOSE_LIMIT}"
    );
}
