//! End-to-end controls for the privileged, version-locked broker import.
//!
//! The three cases intentionally separate operator authority from compatibility:
//! default policy denies the capability, an exact descriptor reaches guest code,
//! and a mismatched descriptor is rejected before an invalid module can be read.

mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lodestone_wasm_host::{
    CapabilitySet, HostError, LoadError, PluginHost, VersionBroker, VersionBrokerDescriptor,
    VersionBrokerRecord, ABI_WORLD, VERSION_BROKER_ABI,
};

struct FixedBroker {
    descriptor: VersionBrokerDescriptor,
}

impl VersionBroker for FixedBroker {
    fn descriptor(&self) -> VersionBrokerDescriptor {
        self.descriptor.clone()
    }

    fn lookup(&self, key: &str) -> Option<VersionBrokerRecord> {
        (key == "protocol").then(|| VersionBrokerRecord::new(key, "copied-from-host"))
    }
}

fn fresh_root(label: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(label);
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create fixture root");
    root
}

fn manifest_text(lock: &VersionBrokerDescriptor, module: &str) -> String {
    format!(
        r#"name = "version-broker-fixture"
version = "0.1.0"
abi = "{ABI_WORLD}"
module = "{module}"
priority = "normal"
description = "Exercises the privileged broker import."
capabilities = ["log", "version:broker"]

[version-lock]
family = "{}"
protocol = {}
abi = "{}"
"#,
        lock.family(),
        lock.protocol(),
        lock.abi(),
    )
}

fn install(root: &Path, manifest: &str, wasm: &[u8]) -> PathBuf {
    let plugin_dir = root.join("version-broker-fixture");
    std::fs::create_dir_all(&plugin_dir).expect("create plugin directory");
    std::fs::write(plugin_dir.join("plugin.toml"), manifest).expect("write manifest");
    std::fs::write(plugin_dir.join("plugin.wasm"), wasm).expect("write module");
    plugin_dir.join("plugin.toml")
}

fn exact_descriptor() -> VersionBrokerDescriptor {
    VersionBrokerDescriptor::new("v26-2", 776, VERSION_BROKER_ABI)
}

#[test]
fn the_privileged_broker_is_denied_by_default_before_instantiation() {
    let wasm = support::build_example_plugin(&["version-broker"]);
    let root = fresh_root("version-broker-denied");
    let manifest = manifest_text(&exact_descriptor(), "plugin.wasm");
    let manifest_path = install(&root, &manifest, &std::fs::read(&wasm).expect("read wasm"));

    let mut host = PluginHost::new(CapabilitySet::default_policy()).expect("engine");
    let error = host
        .load_manifest(&manifest_path)
        .expect_err("the default policy must withhold version:broker");
    assert!(matches!(error, LoadError::Host(HostError::CapabilityDenied { .. })));
    assert!(error.to_string().contains("version:broker"));
    assert!(host.is_empty(), "a denied guest must not be retained");
}

#[test]
fn an_exact_lock_reaches_guest_code_with_only_copied_values() {
    let wasm = support::build_example_plugin(&["version-broker"]);
    let root = fresh_root("version-broker-match");
    let descriptor = exact_descriptor();
    let manifest = manifest_text(&descriptor, "plugin.wasm");
    let manifest_path = install(&root, &manifest, &std::fs::read(&wasm).expect("read wasm"));
    let broker = FixedBroker {
        descriptor: descriptor.clone(),
    };

    let mut host = PluginHost::new(CapabilitySet::permissive())
        .expect("engine")
        .with_version_broker(Arc::new(broker));
    host.load_manifest(&manifest_path)
        .expect("the exact lock and capability grant must load");

    let lines = host.plugins()[0].log_lines();
    assert!(
        lines.iter().any(|(_, line)| line.contains(
            "version broker family=v26-2 protocol=776 abi=lodestone:version-broker@0.1 selected-protocol=copied-from-host"
        )),
        "guest must observe the copied descriptor and lookup value: {lines:?}"
    );
}

#[test]
fn a_mismatched_lock_is_rejected_before_the_module_is_read() {
    let root = fresh_root("version-broker-mismatch");
    let required = exact_descriptor();
    let manifest = manifest_text(&required, "plugin.wasm");
    // The module is deliberately invalid. A compatibility failure proves the
    // loader checked the lock before trying to read or compile these bytes.
    let manifest_path = install(&root, &manifest, b"not a wasm module");
    let actual = VersionBrokerDescriptor::new("v1-21-11", 774, VERSION_BROKER_ABI);

    let mut host = PluginHost::new(CapabilitySet::permissive())
        .expect("engine")
        .with_version_broker(Arc::new(FixedBroker { descriptor: actual }));
    let error = host
        .load_manifest(&manifest_path)
        .expect_err("a different family must refuse the plugin");
    match error {
        LoadError::Host(HostError::VersionBrokerMismatch {
            required_family,
            required_protocol,
            actual_family,
            actual_protocol,
            ..
        }) => {
            assert_eq!(required_family, "v26-2");
            assert_eq!(required_protocol, 776);
            assert_eq!(actual_family, "v1-21-11");
            assert_eq!(actual_protocol, 774);
        }
        other => panic!("expected load-time broker mismatch, got {other:?}"),
    }
    assert!(host.is_empty());
}
