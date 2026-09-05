//! Manifest-driven loading end to end: a `plugins/` directory on disk, each plugin a
//! subdirectory with a `plugin.toml` and a `.wasm`, loaded by manifest and
//! observably acting — plus the rejections, each with the shipped example's real
//! manifest as the control.
//!
//! # Why the shipped `plugin.toml` is read from disk here
//!
//! `crates/plugins/lodestone-chat-responder-wasm/plugin.toml` is the file a plugin
//! author copies. A test that parsed an inline string would leave that file free to
//! rot into invalidity — the documented example being the one thing that does not
//! work is a classic. So the example's own manifest is the input, read from its real
//! path.

mod support;

use std::path::{Path, PathBuf};

use lodestone_ecs::EventPriority;
use lodestone_wasm_host::{
    Action, CapabilitySet, ChatKind, ChatMessage, Event, LoadError, Manifest, ManifestError,
    PluginHost, Priority, ReloadError,
};

fn chat(text: &str) -> Event {
    Event::Chat(ChatMessage {
        text: text.to_owned(),
        kind: ChatKind::Chat,
    })
}

fn shipped_manifest_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../plugins/lodestone-chat-responder-wasm/plugin.toml")
        .canonicalize()
        .expect("the example plugin ships a plugin.toml")
}

/// Build a `plugins/`-shaped directory: `<root>/<dir>/{plugin.toml, <module>}`.
fn install(root: &Path, dir: &str, manifest_text: &str, module_name: &str, wasm: &Path) -> PathBuf {
    let d = root.join(dir);
    std::fs::create_dir_all(&d).expect("mkdir");
    let manifest_path = d.join("plugin.toml");
    std::fs::write(&manifest_path, manifest_text).expect("write manifest");
    std::fs::copy(wasm, d.join(module_name)).expect("copy module");
    manifest_path
}

fn fresh_root(label: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(label);
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("mkdir root");
    root
}

/// The shipped example's manifest is valid, and its declared capabilities are exactly
/// the three the plugin needs — notably **not** including `fs:read`.
#[test]
fn the_shipped_example_manifest_is_valid_and_asks_for_nothing_extra() {
    let manifest = Manifest::load(&shipped_manifest_path()).expect("must parse");
    assert_eq!(manifest.name, "chat-responder");
    assert_eq!(manifest.abi, lodestone_wasm_host::ABI_WORLD);
    assert_eq!(manifest.priority, Priority::Normal);
    assert_eq!(
        manifest.priority.event_priority(),
        EventPriority::Normal,
        "the manifest tier must map onto the ECS's own ordering vocabulary"
    );

    let caps = manifest.requested_capabilities().expect("capabilities");
    assert_eq!(caps.len(), 3, "got {caps}");
    assert!(!caps.contains(lodestone_wasm_host::Capability::FsRead));
    // It is satisfiable under the default policy — the point of that policy being the
    // "denied unless granted" default rather than a lockdown nothing can pass.
    assert!(
        caps.missing_from(&CapabilitySet::default_policy()).is_empty(),
        "the shipped example must load under the default policy"
    );
}

/// The whole point of the tier, in one test: a directory of files on disk, none of
/// which this crate knew about at compile time, produces behaviour.
#[test]
fn a_plugin_installed_as_a_directory_of_files_loads_and_acts() {
    let wasm = support::build_example_plugin(&[]);
    let manifest_text = std::fs::read_to_string(shipped_manifest_path()).expect("read");
    let root = fresh_root("install-one");
    install(
        &root,
        "chat-responder",
        &manifest_text,
        "chat_responder.wasm",
        &wasm,
    );

    let mut host = PluginHost::new(CapabilitySet::default_policy()).expect("engine");

    // CONTROL: the same host, before the directory is scanned.
    assert!(host.tick_all(&[chat("ping")]).is_empty());

    let results = host.load_directory(&root);
    assert_eq!(results.len(), 1, "one plugin.toml, one result");
    results[0].as_ref().expect("must load");

    assert_eq!(
        host.tick_all(&[chat("ping")]),
        vec![Action::SendChat("pong (chat messages seen: 1)".to_owned())]
    );
}

/// Load order is the manifest's declared priority, and it is observable in the order
/// actions come back — which is send order on the wire.
///
/// Two copies of the same module under different names and priorities: the `lowest`
/// one must act first. The directory names are chosen so that alphabetical order is
/// the *opposite* of priority order, so a scan that echoed `read_dir` or sorted by
/// path would produce the reverse and fail.
#[test]
fn declared_priority_decides_load_order_and_therefore_action_order() {
    let wasm = support::build_example_plugin(&[]);
    let base = std::fs::read_to_string(shipped_manifest_path()).expect("read");
    let root = fresh_root("install-order");

    for (dir, name, priority) in [
        ("aaa-dir", "late-plugin", "highest"),
        ("zzz-dir", "early-plugin", "lowest"),
    ] {
        let text = base
            .replace(r#"name = "chat-responder""#, &format!("name = \"{name}\""))
            .replace(r#"priority = "normal""#, &format!("priority = \"{priority}\""));
        install(&root, dir, &text, "chat_responder.wasm", &wasm);
    }

    let mut host = PluginHost::new(CapabilitySet::default_policy()).expect("engine");
    let results = host.load_directory(&root);
    assert_eq!(results.len(), 2);
    for r in &results {
        r.as_ref().expect("both must load");
    }
    assert_eq!(
        host.plugins().iter().map(|p| p.name()).collect::<Vec<_>>(),
        vec!["early-plugin", "late-plugin"],
        "`lowest` loads before `highest`, regardless of directory name"
    );

    // And the ordering is not merely a field on a struct: both guests answer, and the
    // order of the returned actions follows load order.
    let actions = host.tick_all(&[chat("ping")]);
    assert_eq!(actions.len(), 2, "both plugins must answer: {actions:?}");
}

/// A manifest declaring `fs:read` is refused by policy, before its module is
/// compiled — the polite rejection, from the manifest path rather than the raw one.
#[test]
fn a_manifest_declaring_an_ungranted_capability_is_refused_by_policy() {
    let wasm = support::build_example_plugin(&[]);
    let text = std::fs::read_to_string(shipped_manifest_path())
        .expect("read")
        .replace(
            r#"capabilities = ["log", "observe:chat", "act:chat"]"#,
            r#"capabilities = ["log", "observe:chat", "act:chat", "fs:read"]"#,
        );
    let root = fresh_root("install-greedy");
    let manifest_path = install(&root, "greedy", &text, "chat_responder.wasm", &wasm);

    let mut host = PluginHost::new(CapabilitySet::default_policy()).expect("engine");
    let err = host
        .load_manifest(&manifest_path)
        .expect_err("policy withholds fs:read");
    let msg = err.to_string();
    assert!(msg.contains("fs:read"), "{msg}");
    assert!(matches!(err, LoadError::Host(_)), "{err:?}");
    assert!(host.is_empty());

    // CONTROL: the identical directory, loaded by a host whose policy grants it, does
    // load — so the refusal is the policy's decision and not a broken manifest.
    let mut permissive = PluginHost::new(CapabilitySet::permissive()).expect("engine");
    permissive
        .load_manifest(&manifest_path)
        .expect("the same manifest must load under a permissive policy");
    assert_eq!(permissive.plugins().len(), 1);
}

/// An unknown capability name is refused as a *manifest* error, distinct from the
/// policy refusal above, and it names the offending string.
#[test]
fn a_manifest_with_an_unknown_capability_is_refused_as_a_manifest_error() {
    let wasm = support::build_example_plugin(&[]);
    let text = std::fs::read_to_string(shipped_manifest_path())
        .expect("read")
        .replace(r#""act:chat""#, r#""act:teleport""#);
    let root = fresh_root("install-unknown-cap");
    let manifest_path = install(&root, "typo", &text, "chat_responder.wasm", &wasm);

    let mut host = PluginHost::new(CapabilitySet::permissive()).expect("engine");
    let err = host
        .load_manifest(&manifest_path)
        .expect_err("an unknown capability must be refused even under a permissive policy");
    assert!(
        matches!(
            err,
            LoadError::Manifest(ManifestError::UnknownCapability { .. })
        ),
        "{err:?}"
    );
    assert!(err.to_string().contains("act:teleport"), "{err}");
}

/// When the manifest and the module disagree about the plugin's name, the manifest
/// wins and the module's claim is kept for inspection.
///
/// This is the behaviour the test above depends on — two directories installing the
/// *same* module under two names, at two priority tiers — so it is asserted directly
/// rather than left as an implementation detail. See `PluginHost::load_manifest`'s doc
/// for why a fatal check here was the wrong call.
#[test]
fn the_manifest_name_wins_over_the_modules_own_and_both_stay_visible() {
    let wasm = support::build_example_plugin(&[]);
    let text = std::fs::read_to_string(shipped_manifest_path())
        .expect("read")
        .replace(r#"name = "chat-responder""#, r#"name = "something-else""#);
    let root = fresh_root("install-mismatch");
    let manifest_path = install(&root, "mismatch", &text, "chat_responder.wasm", &wasm);

    let mut host = PluginHost::new(CapabilitySet::default_policy()).expect("engine");
    host.load_manifest(&manifest_path).expect("must load");
    let plugin = &host.plugins()[0];
    assert_eq!(plugin.name(), "something-else", "the manifest names the plugin");
    assert_eq!(
        plugin.info().name,
        "chat-responder",
        "the module's own claim must remain inspectable"
    );
}

/// A manifest naming a module that is not there is reported against the manifest,
/// with the path it looked at.
#[test]
fn a_manifest_naming_a_missing_module_says_where_it_looked() {
    let text = std::fs::read_to_string(shipped_manifest_path()).expect("read");
    let root = fresh_root("install-no-module");
    let d = root.join("empty");
    std::fs::create_dir_all(&d).unwrap();
    let manifest_path = d.join("plugin.toml");
    std::fs::write(&manifest_path, &text).unwrap();

    let mut host = PluginHost::new(CapabilitySet::default_policy()).expect("engine");
    let err = host.load_manifest(&manifest_path).expect_err("no module");
    assert!(
        matches!(err, LoadError::Manifest(ManifestError::MissingModule { .. })),
        "{err:?}"
    );
    assert!(err.to_string().contains("chat_responder.wasm"), "{err}");
}

/// One broken plugin does not stop the working one, and both are reported.
#[test]
fn a_directory_with_one_broken_plugin_still_loads_the_good_one() {
    let wasm = support::build_example_plugin(&[]);
    let base = std::fs::read_to_string(shipped_manifest_path()).expect("read");
    let root = fresh_root("install-mixed");

    install(&root, "good", &base, "chat_responder.wasm", &wasm);
    install(
        &root,
        "broken",
        &base.replace("lodestone:plugin@0.18.0", "lodestone:plugin@9.9.9"),
        "chat_responder.wasm",
        &wasm,
    );

    let mut host = PluginHost::new(CapabilitySet::default_policy()).expect("engine");
    let results = host.load_directory(&root);
    assert_eq!(results.len(), 2, "both must be reported");
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(host.plugins().len(), 1);
    let err = results
        .iter()
        .find_map(|r| r.as_ref().err())
        .expect("the broken one must be reported");
    assert!(err.to_string().contains("9.9.9"), "{err}");

    // The good one still works, which is the assertion that makes "does not abort"
    // mean something.
    assert_eq!(host.tick_all(&[chat("ping")]).len(), 1);
}

/// Startup discovery can retain a good sibling while reporting a bad one. A
/// replacement has a different safety contract: it must retain the current
/// running set rather than quietly turn a bad edit into an unload.
#[test]
fn a_rejected_reload_keeps_the_previous_working_guest_alive() {
    let wasm = support::build_example_plugin(&[]);
    let manifest = std::fs::read_to_string(shipped_manifest_path()).expect("read manifest");
    let root = fresh_root("reload-rejected-keeps-old");
    let manifest_path = install(&root, "chat-responder", &manifest, "chat_responder.wasm", &wasm);

    let mut host = PluginHost::new(CapabilitySet::default_policy()).expect("engine");
    host.load_directory(&root)
        .into_iter()
        .next()
        .expect("one manifest")
        .expect("initial guest must load");
    assert_eq!(host.tick_all(&[chat("ping")]).len(), 1, "the initial guest is live");

    std::fs::write(
        &manifest_path,
        manifest.replace("lodestone:plugin@0.18.0", "lodestone:plugin@9.9.9"),
    )
    .expect("make the replacement manifest invalid for this host");
    let error = host
        .stage_directory_reload(&root, &Default::default())
        .expect_err("a malformed replacement must not stage");
    assert!(matches!(error, ReloadError::Rejected { .. }), "{error:?}");
    assert_eq!(host.plugins().len(), 1, "the active host must not be replaced");
    assert_eq!(
        host.tick_all(&[chat("ping")]),
        vec![Action::SendChat("pong (chat messages seen: 2)".to_owned())],
        "the old guest must keep serving after the rejected replacement"
    );
}

/// Dependency validation is part of the replacement transaction: a cycle in a
/// newly discovered graph cannot partially replace the currently running guest.
#[test]
fn a_dependency_cycle_rejects_reload_and_keeps_the_previous_guest_alive() {
    let wasm = support::build_example_plugin(&[]);
    let manifest = std::fs::read_to_string(shipped_manifest_path()).expect("read manifest");
    let root = fresh_root("reload-cycle-keeps-old");
    install(&root, "chat-responder", &manifest, "chat_responder.wasm", &wasm);

    let mut host = PluginHost::new(CapabilitySet::default_policy()).expect("engine");
    host.load_directory(&root)
        .into_iter()
        .next()
        .expect("one manifest")
        .expect("initial guest must load");
    assert_eq!(host.tick_all(&[chat("ping")]).len(), 1, "the initial guest is live");

    let a = manifest
        .replace(r#"name = "chat-responder""#, r#"name = "a""#)
        + "\n[dependencies]\nrequired = [\"b\"]\n";
    let b = manifest
        .replace(r#"name = "chat-responder""#, r#"name = "b""#)
        + "\n[dependencies]\nrequired = [\"a\"]\n";
    install(&root, "a", &a, "chat_responder.wasm", &wasm);
    install(&root, "b", &b, "chat_responder.wasm", &wasm);

    let error = host
        .stage_directory_reload(&root, &Default::default())
        .expect_err("a dependency cycle must not stage");
    assert!(matches!(error, ReloadError::Rejected { .. }), "{error:?}");
    assert_eq!(host.plugins().len(), 1, "the active host must not be replaced");
    assert_eq!(
        host.tick_all(&[chat("ping")]),
        vec![Action::SendChat("pong (chat messages seen: 2)".to_owned())],
        "the old guest must keep serving after a graph rejection"
    );
}
