//! The runtime-loading gate: a `.wasm` file that this crate has **no compile-time
//! knowledge of** is built by a separate `rustc` invocation for a different target
//! triple, opened by path at runtime, and observably changes what the host
//! produces.
//!
//! # Why the negative control runs first
//!
//! `docs/plans/runtime-plugin-loading.md`'s M1 gate specifies a control in which
//! the `.wasm` is absent and the same test must observe zero actions. It is run
//! *before* the load here rather than as a separate test, for a reason worth
//! keeping: as a separate test it would share nothing with the positive case
//! except a name, and could pass because of a typo in the event it feeds in. In
//! this order, the identical `chat("…ping…")` batch is pushed through the identical
//! host, and the only thing that differs between the empty result and the
//! populated one is that a file was loaded.

mod support;

use lodestone_wasm_host::{
    Action, Capability, CapabilitySet, ChatKind, ChatMessage, Event, PluginHost,
};

fn chat(text: &str) -> Event {
    Event::Chat(ChatMessage {
        text: text.to_owned(),
        kind: ChatKind::Chat,
    })
}

/// The capability set a well-behaved chat responder declares.
fn responder_capabilities() -> CapabilitySet {
    CapabilitySet::from_iter([Capability::Log, Capability::ObserveChat, Capability::ActChat])
}

#[test]
fn a_wasm_file_built_separately_changes_behaviour_once_loaded_from_disk() {
    let wasm = support::build_example_plugin(&[]);
    assert!(
        wasm.extension().is_some_and(|e| e == "wasm"),
        "the artifact under test must be a .wasm file on disk: {}",
        wasm.display()
    );

    let mut host = PluginHost::new(CapabilitySet::default_policy()).expect("engine");

    // NEGATIVE CONTROL. Nothing loaded, so nothing acts. If this ever passes for
    // the wrong reason — say `tick_all` returning empty unconditionally — the
    // positive assertion below fails, so the pair is not satisfiable by a stub.
    assert_eq!(
        host.tick_all(&[chat("hello ping there")]),
        Vec::<Action>::new(),
        "control: a host with no plugin loaded must produce no actions"
    );

    host.load_file("chat-responder", &wasm, &responder_capabilities())
        .expect("the example plugin must load");

    // The plugin identified itself across the boundary. These strings are authored
    // in the *guest* crate — a separate compilation unit this crate does not link —
    // so they are not something the host could have invented.
    let info = host.plugins()[0].info().clone();
    assert_eq!(info.name, "chat-responder");
    assert_eq!(info.abi, lodestone_wasm_host::ABI_WORLD);

    let actions = host.tick_all(&[chat("hello ping there")]);
    assert_eq!(
        actions,
        vec![Action::SendChat("pong (chat messages seen: 1)".to_owned())],
        "the loaded plugin must answer a chat containing `ping`"
    );
}

/// Answered by observation: the guest owns its state across host calls, rather
/// than the host rebuilding a stateless guest per tick.
///
/// The counter in the reply is the evidence. A stateless request/response host —
/// one that built a fresh instance per tick — would answer `seen: 1` every time,
/// which is why the assertion is on the *second* and *third* values rather than on
/// the mere presence of a number.
#[test]
fn guest_state_persists_across_ticks() {
    let wasm = support::build_example_plugin(&[]);
    let mut host = PluginHost::new(CapabilitySet::default_policy()).expect("engine");
    host.load_file("chat-responder", &wasm, &responder_capabilities())
        .expect("load");

    let mut replies = Vec::new();
    for _ in 0..3 {
        replies.extend(host.tick_all(&[chat("ping")]));
    }
    assert_eq!(
        replies,
        vec![
            Action::SendChat("pong (chat messages seen: 1)".to_owned()),
            Action::SendChat("pong (chat messages seen: 2)".to_owned()),
            Action::SendChat("pong (chat messages seen: 3)".to_owned()),
        ],
        "the guest's linear memory must survive between host calls"
    );
}

/// A tick with no events still runs the guest, and a chat that does not match
/// produces nothing — so "it replied" is a decision about the input rather than an
/// unconditional side effect of being ticked.
#[test]
fn a_non_matching_chat_and_an_empty_batch_both_produce_nothing() {
    let wasm = support::build_example_plugin(&[]);
    let mut host = PluginHost::new(CapabilitySet::default_policy()).expect("engine");
    host.load_file("chat-responder", &wasm, &responder_capabilities())
        .expect("load");

    assert_eq!(host.tick_all(&[]), Vec::<Action>::new());
    assert_eq!(
        host.tick_all(&[chat("nothing to see here")]),
        Vec::<Action>::new()
    );
    // Control on the two assertions above: the same host, same tick, does act on a
    // matching message — and the count proves the non-matching one *was* delivered
    // and counted, not silently dropped before reaching the guest.
    assert_eq!(
        host.tick_all(&[chat("ping")]),
        vec![Action::SendChat("pong (chat messages seen: 2)".to_owned())]
    );
}

/// The guest reached the host's log sink, which is the `log` capability's only
/// effect. Asserted because `logging` is the one import granted unconditionally,
/// and an unconditional grant that turned out to be a no-op would hide a broken
/// `add_to_linker` for every other import too.
#[test]
fn the_guest_reaches_the_hosts_log_sink() {
    let wasm = support::build_example_plugin(&[]);
    let mut host = PluginHost::new(CapabilitySet::default_policy()).expect("engine");
    host.load_file("chat-responder", &wasm, &responder_capabilities())
        .expect("load");
    let lines = host.plugins()[0].log_lines();
    assert!(
        lines.iter().any(|(_, m)| m.contains("starting up")),
        "expected the guest's init log line, got {lines:?}"
    );
}
