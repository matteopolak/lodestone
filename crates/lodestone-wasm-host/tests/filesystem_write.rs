//! The `fs:write` import is denied structurally and confined when granted.
//!
//! The denied arm loads a guest that really references the write interface with
//! no write capability, proving that the linker omitted the import. The control
//! grants the same module and checks both an accepted write and a rejected parent
//! traversal, including the host-side recording sink.

mod support;

use std::path::PathBuf;

use lodestone_wasm_host::{Action, Capability, CapabilitySet, HostError, PluginHost};

fn declared_capabilities() -> CapabilitySet {
    CapabilitySet::from_iter([Capability::Log, Capability::ActChat])
}

#[test]
fn a_plugin_that_uses_fs_write_without_the_grant_is_refused_at_load() {
    let wasm = support::build_example_plugin(&["fs-write"]);
    let mut host = PluginHost::new(CapabilitySet::permissive()).expect("engine");
    let mut granted = declared_capabilities();
    // A read grant must not link the separate write interface.
    granted.insert(Capability::FsRead);

    let err = host
        .load_file("writer", &wasm, &granted)
        .expect_err("a plugin using an ungranted import must not load");

    assert!(matches!(err, HostError::Instantiate { .. }), "{err:?}");
    assert!(
        err.to_string().contains("lodestone:plugin/filesystem-write"),
        "the refusal must name the withheld interface: {err}"
    );
    assert!(host.is_empty(), "a refused plugin must not be retained");
}

#[test]
fn a_granted_fs_write_is_confined_to_the_configured_root() {
    let wasm = support::build_example_plugin(&["fs-write"]);
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("fs-write-control");
    std::fs::create_dir_all(&root).expect("create the plugin filesystem root");
    let inside = root.join("written.txt");
    let escaped = root.parent().expect("control root has a parent").join("outside.txt");
    let _ = std::fs::remove_file(&inside);
    let _ = std::fs::remove_file(&escaped);

    let mut host = PluginHost::new(CapabilitySet::permissive())
        .expect("engine")
        .with_filesystem_root(root.clone());
    let mut granted = declared_capabilities();
    granted.insert(Capability::FsWrite);
    host.load_file("writer", &wasm, &granted)
        .expect("the same module must load once fs:write is granted");

    let actions = host.tick_all(&[]);
    let report = actions
        .iter()
        .find_map(|action| match action {
            Action::SendChat(text) if text.starts_with("fs-write:") => Some(text.as_str()),
            _ => None,
        })
        .expect("the fixture must report both write attempts");
    assert!(report.contains("outside=false"), "parent traversal escaped: {report}");
    assert!(report.contains("inside=true"), "root write failed: {report}");

    assert_eq!(std::fs::read(&inside).expect("read the accepted write"), b"written-by-guest");
    assert!(!escaped.exists(), "a parent traversal must not write outside the root");
    assert_eq!(
        host.plugins()[0].attempted_file_writes(),
        &[
            ("../outside.txt".to_owned(), b"must-not-escape".to_vec()),
            ("written.txt".to_owned(), b"written-by-guest".to_vec()),
        ]
    );
}
