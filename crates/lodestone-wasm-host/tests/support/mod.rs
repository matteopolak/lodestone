//! Builds the example plugin into a real `.wasm` file, for tests that then load it
//! **from that file** at runtime.
//!
//! # Why the test builds it rather than reading a checked-in artifact
//!
//! Two options were rejected.
//!
//! A **checked-in `.wasm`** would be a binary blob in git that nothing forces to
//! match `src/lib.rs`, and the failure mode is the worst kind: the test passes
//! against a stale artifact while the source it claims to exercise has moved on.
//!
//! A **`just` recipe the human runs first**, with the test skipping when the file
//! is absent, is the *precondition* species of vacuous test that `CLAUDE.md` names
//! outright — it reports green on a machine where the plugin was never built, which
//! is precisely the machine where you need it to report red. So this module builds
//! the artifact and **panics** if it cannot, including when the `wasm32` target is
//! not installed. There is no skip path.
//!
//! # Is a test-built artifact still "separately built"?
//!
//! Yes, and the distinction that matters is not who typed the command. This is a
//! different crate, compiled for a different target triple by a separate `rustc`
//! invocation into a file on disk, which the host then opens by path with no
//! compile-time knowledge of it whatsoever — there is no `include_bytes!` anywhere
//! in this crate. Delete the `.wasm` and the host loads nothing; replace it with a
//! different plugin and the host loads that instead. That is the property the gate
//! is about.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Build the example plugin for `wasm32-unknown-unknown` with `features`, and
/// return the path of the resulting **core module** `.wasm`.
///
/// A core module, not a component, on purpose: this is exactly what a plugin
/// author gets from a plain `cargo build --target wasm32-unknown-unknown`, and
/// `PluginHost::load_file` encoding it is the production path under test. Handing
/// the host a pre-encoded component would exercise a path most plugin authors
/// never take.
pub fn build_example_plugin(features: &[&str]) -> PathBuf {
    let plugin_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../plugins/lodestone-chat-responder-wasm")
        .canonicalize()
        .expect("the example plugin crate must exist next to this one");

    // Per-feature-set target dir, so the well-behaved and misbehaving artifacts
    // never overwrite each other — two tests asserting opposite things about "the
    // plugin" while sharing one output path is a race that would show up as an
    // inexplicable pass.
    let label = if features.is_empty() {
        "default".to_owned()
    } else {
        features.join("-")
    };
    let target_dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("plugin-{label}"));

    let mut cmd = Command::new(env!("CARGO"));
    cmd.current_dir(&plugin_dir)
        .arg("build")
        .arg("--release")
        .arg("--target")
        .arg("wasm32-unknown-unknown")
        .arg("--target-dir")
        .arg(&target_dir)
        // Bound the nested build: the outer `cargo test` is already using the
        // machine, and this crate's own guidance is never to let two cargo runs
        // fight over it.
        .arg("-j")
        .arg("2");
    if !features.is_empty() {
        cmd.arg("--features").arg(features.join(","));
    }
    // The parent `cargo test` exports these, and a nested build that inherits them
    // resolves flags and target dirs meant for the host build.
    cmd.env_remove("CARGO_TARGET_DIR")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTFLAGS");

    let out = cmd.output().expect("failed to spawn cargo for the guest build");
    assert!(
        out.status.success(),
        "building the example plugin for wasm32-unknown-unknown failed (features: {label}).\n\
         If the target is missing, run `rustup target add wasm32-unknown-unknown`.\n\
         --- stdout ---\n{}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    let wasm = target_dir
        .join("wasm32-unknown-unknown/release")
        .join("lodestone_chat_responder_wasm.wasm");
    assert!(
        wasm.is_file(),
        "cargo reported success but {} does not exist",
        wasm.display()
    );
    wasm
}
