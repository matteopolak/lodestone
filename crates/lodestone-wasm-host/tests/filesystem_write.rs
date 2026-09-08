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
fn filesystem_data_rejects_an_unsafe_plugin_name() {
    let wasm = support::build_example_plugin(&["fs-write"]);
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("unsafe-plugin-name");
    let mut host = PluginHost::new(CapabilitySet::permissive())
        .expect("engine")
        .with_filesystem_root(root);
    let mut granted = declared_capabilities();
    granted.insert(Capability::FsWrite);

    let error = host
        .load_file("../escape", &wasm, &granted)
        .expect_err("filesystem data must not use a path-bearing plugin name");
    assert!(matches!(error, HostError::InvalidFilesystemName { .. }), "{error:?}");
    assert!(host.is_empty(), "a plugin with an unsafe data root must not load");
}

#[test]
fn a_granted_fs_write_is_confined_to_the_configured_root() {
    let wasm = support::build_example_plugin(&["fs-write"]);
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("fs-write-control");
    std::fs::create_dir_all(&root).expect("create the plugin filesystem root");
    let plugin_root = root.join("writer");
    let inside = plugin_root.join("written.txt");
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

#[test]
fn persistent_data_survives_a_successful_directory_reload() {
    let wasm = support::build_example_plugin(&["fs-write"]);
    let directory = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("reload-plugins");
    let plugin_dir = directory.join("chat-responder");
    std::fs::create_dir_all(&plugin_dir).expect("create plugin directory");
    std::fs::write(
        plugin_dir.join("plugin.toml"),
        r#"name = "chat-responder"
version = "0.1.0"
abi = "lodestone:plugin@0.27.0"
module = "chat_responder.wasm"
capabilities = ["log", "observe:chat", "act:chat", "fs:write"]
"#,
    )
    .expect("write plugin manifest");
    std::fs::copy(&wasm, plugin_dir.join("chat_responder.wasm")).expect("install plugin module");

    let data_root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("reload-data");
    let mut host = PluginHost::new(CapabilitySet::permissive())
        .expect("engine")
        .with_filesystem_root(data_root.clone());
    host.load_directory(&directory)
        .into_iter()
        .next()
        .expect("one manifest")
        .expect("initial guest must load");
    host.tick_all(&[]);

    let data_file = data_root.join("chat-responder/written.txt");
    assert_eq!(std::fs::read(&data_file).expect("read persisted guest data"), b"written-by-guest");
    let replacement = host
        .stage_directory_reload(&directory, &Default::default())
        .expect("a valid replacement must stage");
    assert_eq!(replacement.plugins().len(), 1);
    assert_eq!(
        std::fs::read(&data_file).expect("reload must retain the plugin data directory"),
        b"written-by-guest"
    );
}

#[test]
fn persistent_writes_are_bounded_atomic_and_deletable() {
    let wasm = support::build_example_plugin(&["fs-write"]);
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("fs-write-lifecycle");
    let plugin_root = root.join("writer");
    std::fs::create_dir_all(&plugin_root).expect("create plugin filesystem root");
    let mut host = PluginHost::new(CapabilitySet::permissive())
        .expect("engine")
        .with_filesystem_root(root.clone());
    let mut granted = declared_capabilities();
    granted.insert(Capability::FsWrite);
    host.load_file("writer", &wasm, &granted).expect("load writer");

    let plugin = &mut host.plugins_mut()[0];
    plugin
        .write_file("record.bin", b"old-value".to_vec())
        .expect("initial write");
    plugin
        .write_file("record.bin", b"new-value".to_vec())
        .expect("atomic replacement");
    assert_eq!(std::fs::read(plugin_root.join("record.bin")).expect("read replacement"), b"new-value");
    assert!(
        std::fs::read_dir(&plugin_root)
            .expect("list plugin data")
            .all(|entry| !entry
                .expect("read plugin data entry")
                .file_name()
                .to_string_lossy()
                .contains(".tmp-")),
        "a completed atomic write must not leave its staging file"
    );
    let oversized = vec![0; lodestone_wasm_host::MAX_PLUGIN_FILE_BYTES + 1];
    let error = plugin
        .write_file("record.bin", oversized)
        .expect_err("oversized data must be refused");
    assert!(error.contains("per-file limit"), "{error}");
    assert_eq!(
        std::fs::read(plugin_root.join("record.bin")).expect("oversized write must not replace data"),
        b"new-value"
    );
    plugin.delete_file("record.bin").expect("delete persisted data");
    assert!(!plugin_root.join("record.bin").exists(), "deleted data must be absent");
}

#[test]
fn guest_deletion_removes_plugin_data_through_the_real_import() {
    let wasm = support::build_example_plugin(&["fs-delete"]);
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("fs-delete-guest");
    let mut host = PluginHost::new(CapabilitySet::permissive())
        .expect("engine")
        .with_filesystem_root(root.clone());
    let mut granted = declared_capabilities();
    granted.insert(Capability::FsWrite);
    host.load_file("deleter", &wasm, &granted)
        .expect("load guest deletion fixture");

    let actions = host.tick_all(&[]);
    let report = actions
        .iter()
        .find_map(|action| match action {
            Action::SendChat(text) if text.starts_with("fs-delete:") => Some(text.as_str()),
            _ => None,
        })
        .expect("the guest must report both lifecycle writes");
    assert!(report.contains("seeded=true"), "seed write failed: {report}");
    assert!(report.contains("deleted=true"), "delete write failed: {report}");
    assert!(
        !root.join("deleter/delete-me.txt").exists(),
        "an empty guest write must remove the plugin data file"
    );
}
