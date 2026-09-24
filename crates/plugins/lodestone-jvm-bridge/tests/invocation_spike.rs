//! Live gate for the classloader-plus-JNI invocation fixture.
//!
//! The fixture is hermetic once run in its pinned container, but it requires a
//! JVM and the container runtime. Keep it ignored on ordinary host test runs.

use std::path::PathBuf;
use std::process::Command;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("bridge crate has three ancestors to the workspace root")
        .to_path_buf()
}

#[test]
#[ignore = "requires the pinned JDK/container runtime"]
fn intercepted_jni_invocation_spike_passes_all_scenarios() {
    let root = workspace_root();
    let script = root.join(
        "crates/plugins/lodestone-jvm-bridge/spike/invocation/run.sh",
    );
    let status = Command::new("bash")
        .arg(script)
        .current_dir(root)
        .status()
        .expect("start the invocation spike runner");
    assert!(status.success(), "invocation spike runner failed: {status}");
}
