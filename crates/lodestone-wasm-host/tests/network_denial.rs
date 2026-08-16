//! The network half of issue #176's sandbox verification gate — the sibling of
//! `tests/capability_denial.rs`'s filesystem gate, and structurally different for
//! a reason worth stating up front.
//!
//! # Why this is not "grant the capability and watch it succeed"
//!
//! The filesystem gate has two arms because `fs:read` is a real, grantable
//! capability: refuse it and a hostile guest is denied at *instantiation*; grant
//! it and the same guest genuinely reads bytes, which is the control that proves
//! the detector can see an open door and not just a closed one.
//!
//! Networking has no equivalent door. The `lodestone:plugin` world defines no
//! sockets interface, so there is nothing to grant — `src/host.rs`'s own header
//! already states this precisely: "no clock, no socket … for a guest to find".
//! `wasm32-unknown-unknown`'s `std::net` is backed by `sys::unsupported`
//! end-to-end, so a guest that calls `TcpStream::connect` never touches an
//! import at all; the error comes from a stub inside its own copy of `std`,
//! before wasmtime is involved. There is no "sandbox disabled" configuration of
//! *this* host to compare against, because the host was never in the business of
//! deciding this either way.
//!
//! So the control below answers the question that actually matters here: is the
//! guest's connect attempt a **genuine** one, or would it fail against any
//! address for reasons that have nothing to do with sandboxing (test host has no
//! loopback, the port is already refused, etc.)? It proves that by running the
//! identical `TcpStream::connect` call natively, in this test's own process,
//! against a real listener, and requiring it to succeed — the "world" species of
//! vacuous test `CLAUDE.md` warns about is exactly a gate whose target could
//! never have been reached in the first place.

mod support;

use std::io::ErrorKind;
use std::net::TcpListener;

use lodestone_wasm_host::{Action, Capability, CapabilitySet, ChatKind, ChatMessage, Event, PluginHost};

/// Must match the literal hardcoded in
/// `crates/plugins/lodestone-chat-responder-wasm/src/lib.rs`'s `attempt_network`.
/// A guest has no environment and no argv, so there is no channel to hand it a
/// dynamic port; a fixed, distinctive one is the fixture's own address.
const NETWORK_PROBE_ADDR: &str = "127.0.0.1:47899";

/// The control's own listener, distinct from the one above so the two tests do
/// not race each other for a socket under `--test-threads=2`.
const NATIVE_CONTROL_ADDR: &str = "127.0.0.1:47900";

fn chat(text: &str) -> Event {
    Event::Chat(ChatMessage {
        text: text.to_owned(),
        kind: ChatKind::Chat,
    })
}

fn capabilities() -> CapabilitySet {
    CapabilitySet::from_iter([Capability::Log, Capability::ObserveChat, Capability::ActChat])
}

#[test]
fn a_guest_cannot_open_a_tcp_socket_even_to_a_real_reachable_listener() {
    // A real listener at the exact address the guest will dial, so a failure to
    // connect cannot be blamed on "nothing was listening".
    let listener = TcpListener::bind(NETWORK_PROBE_ADDR)
        .expect("bind the probe listener — if this fails, another test or process is holding the port");
    listener.set_nonblocking(true).expect("nonblocking accept");

    let wasm = support::build_example_plugin(&["network"]);
    let mut host = PluginHost::new(CapabilitySet::default_policy()).expect("engine");
    host.load_file("net-hog", &wasm, &capabilities())
        .expect("a guest that merely attempts a connect is well-formed and must load");

    let actions = host.tick_all(&[chat("go")]);

    let reported = actions
        .iter()
        .find_map(|a| match a {
            Action::SendChat(t) if t.starts_with("net:") => Some(t.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("the fixture must report its attempt; got {actions:?}"));
    assert!(
        reported.contains("ok=false"),
        "a wasm32-unknown-unknown guest must never observe a successful connect: {reported}"
    );

    // And the denial happened before anything reached the wire: the listener
    // received no connection at all, not merely "the guest gave up and did not
    // tell us". Without this, a guest that silently swallowed a real connection
    // and just printed a false `ok=false` would pass the assertion above.
    match listener.accept() {
        Err(e) if e.kind() == ErrorKind::WouldBlock => {}
        other => panic!("the guest's connect attempt reached the real listener: {other:?}"),
    }
}

/// THE CONTROL. See this file's header for why it takes this shape rather than
/// "grant the capability and watch it succeed": there is no capability to grant.
/// What this proves instead is that the identical `TcpStream::connect` call, run
/// outside the sandbox, against a real listener, genuinely succeeds — ruling out
/// the premise-false failure where the test environment itself cannot make a
/// loopback connection for unrelated reasons (a locked-down CI sandbox, IPv6-only
/// loopback, etc.), which would make the assertion above pass vacuously.
#[test]
fn the_control_the_identical_connect_call_succeeds_natively_against_a_real_listener() {
    let listener = TcpListener::bind(NATIVE_CONTROL_ADDR).expect("bind the control listener");

    let acceptor = std::thread::spawn(move || listener.accept());

    let stream = std::net::TcpStream::connect(NATIVE_CONTROL_ADDR)
        .expect("a native connect to a real local listener must succeed — if it does not, the test \
                 environment itself cannot make loopback connections and the gate above is not evidence \
                 of sandboxing");
    drop(stream);

    let (accepted, _) = acceptor
        .join()
        .expect("acceptor thread must not panic")
        .expect("the listener must accept the connection the native call just made");
    drop(accepted);
}
