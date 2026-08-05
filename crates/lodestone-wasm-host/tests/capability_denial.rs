//! The capability gate, gated against a plugin that **genuinely misbehaves** — and
//! the control that proves the detector fires.
//!
//! # Why an absence assertion needs all three of these
//!
//! `CLAUDE.md`: *"Assertions of an absence need a control proving the detector
//! works."* `crates/lodestone-fuzz` is the precedent — it asserts refusal through a
//! `RecordingSink` (an `AdapterError::Decode` **and** zero writes), because a
//! discarding sink structurally cannot distinguish "refused" from "wrongly
//! accepted". The same three observations are needed here, and no two of them are
//! sufficient:
//!
//! | observation | what it rules out |
//! |---|---|
//! | the load returns `Err`, and the message names `lodestone:plugin/filesystem` | that the plugin loaded and was merely unlucky |
//! | [`LoadedPlugin::attempted_file_reads`] is empty *and the host has no plugins at all* | that it loaded, read the file, and the error came from somewhere else |
//! | **the control**: the same module, granted the capability, records the read | that `attempted_file_reads` is always empty — the premise-false failure that fires in the safe-looking direction |
//!
//! The third is the one that costs something to build and is the one people skip.
//! Without it, deleting the body of `filesystem::Host::read_file` would leave every
//! assertion in this file passing.
//!
//! # The load-bearing sentence
//!
//! **The manifest is a declaration; the `Linker` is the enforcement.** A plugin that
//! lies about its capabilities does not get them — it fails to load. That is a
//! property of the component model rather than of our code: imports resolve by name
//! at instantiation, and an interface policy withheld was never added to the
//! `Linker`. See `docs/wasm-plugin-host.md` §"The capability probe" for the two
//! component import lists that establish it.

mod support;

use lodestone_wasm_host::{Capability, CapabilitySet, ChatKind, ChatMessage, Event, HostError, PluginHost};

fn chat(text: &str) -> Event {
    Event::Chat(ChatMessage {
        text: text.to_owned(),
        kind: ChatKind::Chat,
    })
}

/// What the misbehaving plugin *declares* — note the absence of `fs:read`. It is
/// the same set the well-behaved plugin declares, so the only difference between
/// the two arms of this file is what the module does.
fn declared_capabilities() -> CapabilitySet {
    CapabilitySet::from_iter([Capability::Log, Capability::ObserveChat, Capability::ActChat])
}

#[test]
fn a_plugin_that_uses_a_capability_it_did_not_declare_is_refused_at_load() {
    let wasm = support::build_example_plugin(&["misbehave"]);
    let mut host = PluginHost::new(CapabilitySet::default_policy()).expect("engine");

    let err = host
        .load_file("thief", &wasm, &declared_capabilities())
        .expect_err("a plugin using an ungranted import must not load");

    // (1) It is the *instantiation* that refused, and wasmtime's own message names
    //     the interface — so the refusal is about the filesystem capability and not
    //     about, say, a missing export.
    assert!(
        matches!(err, HostError::Instantiate { .. }),
        "expected HostError::Instantiate, got {err:?}"
    );
    let text = err.to_string();
    assert!(
        text.contains("lodestone:plugin/filesystem"),
        "the refusal must name the interface it withheld; got:\n{text}"
    );

    // (2) Nothing loaded, so there is nothing to read a file *with*, and no guest
    //     for the host to tick.
    assert!(
        host.is_empty(),
        "a refused plugin must not be retained: {:?}",
        host.plugins()
    );
    assert!(
        host.tick_all(&[chat("ping")]).is_empty(),
        "a host that refused its only plugin must produce no actions"
    );
}

/// **THE CONTROL.** The same `.wasm`, the same host type, the same tick — with the
/// capability granted. If this test did not pass, every assertion in the test above
/// would be vacuous, because nothing would establish that the filesystem interface
/// is reachable at all.
#[test]
fn the_control_the_same_module_reaches_the_filesystem_once_granted() {
    let wasm = support::build_example_plugin(&["misbehave"]);

    // A scoped root with one readable file in it. `granted.txt` is the path the
    // fixture asks for from *inside* the root; it also asks for `/etc/passwd`, from
    // outside.
    let root = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("fs-root-control");
    std::fs::create_dir_all(&root).expect("create the plugin filesystem root");
    std::fs::write(root.join("granted.txt"), b"granted-bytes").expect("seed the root");

    let mut host = PluginHost::new(CapabilitySet::permissive())
        .expect("engine")
        .with_filesystem_root(root.clone());

    let mut granted = declared_capabilities();
    granted.insert(Capability::FsRead);
    host.load_file("thief", &wasm, &granted)
        .expect("the very same module must load once fs:read is granted");

    let actions = host.tick_all(&[chat("ping")]);

    // The recording sink fired: the host saw both attempts, in the fixture's order.
    let attempts = host.plugins()[0].attempted_file_reads().to_vec();
    assert_eq!(
        attempts,
        vec!["/etc/passwd".to_owned(), "granted.txt".to_owned()],
        "the host must record every attempted read"
    );

    // And the second layer of defence held: even a granted plugin is confined to
    // its root, so the outside-root read failed and the inside-root read returned
    // the host's bytes. The guest reports both back through its chat action, which
    // is how a value the *host* wrote (`granted-bytes`) becomes observable here
    // after a full round trip through the guest.
    let reported = actions
        .iter()
        .find_map(|a| match a {
            lodestone_wasm_host::Action::SendChat(t) if t.starts_with("fs:") => Some(t.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("the fixture must report its reads; got {actions:?}"));
    assert!(
        reported.contains("outside=false"),
        "a read outside the configured root must be refused even when granted: {reported}"
    );
    assert!(
        reported.contains("inside=granted-bytes"),
        "a read inside the root must return the host's bytes: {reported}"
    );
}

/// The polite refusal: a plugin that *declares* a capability policy withholds is
/// rejected before its module is compiled, with a message naming the capability.
///
/// Distinct from the test above, and both are needed: this one is the honest plugin
/// meeting a restrictive operator, that one is the dishonest plugin meeting the
/// runtime.
#[test]
fn declaring_an_ungranted_capability_is_refused_before_the_module_is_compiled() {
    let wasm = support::build_example_plugin(&["misbehave"]);
    let mut host = PluginHost::new(CapabilitySet::default_policy()).expect("engine");

    let mut requested = declared_capabilities();
    requested.insert(Capability::FsRead);
    let err = host
        .load_file("honest-but-greedy", &wasm, &requested)
        .expect_err("policy withholds fs:read");

    assert!(
        matches!(err, HostError::CapabilityDenied { .. }),
        "expected CapabilityDenied, got {err:?}"
    );
    let text = err.to_string();
    assert!(text.contains("fs:read"), "{text}");
    assert!(host.is_empty());
}
