#![cfg(feature = "jvm")]

use std::time::Duration;

use lodestone_jvm_bridge::adapter::AdapterHost;
use lodestone_jvm_bridge::runtime::JvmConfig;

#[test]
fn rejects_invalid_adapter_before_starting_a_jvm() {
    for name in ["", "a..B", "a/B", "a.1B", "a;B"] {
        let error = AdapterHost::start(JvmConfig::new(), name, Duration::from_secs(2))
            .expect_err("invalid class must not reach JVM startup");
        assert!(error.to_string().contains("adapter class"), "{error}");
    }
}

#[test]
fn rejects_zero_deadline_before_starting_a_jvm() {
    let error = AdapterHost::start(JvmConfig::new(), "example.Adapter", Duration::ZERO)
        .expect_err("zero deadline must not reach JVM startup");
    assert!(error.to_string().contains("deadline"), "{error}");
}

/// Run this test in its own process with JAVA_HOME set to an installed JDK.
/// It compiles only repository-owned Java source and starts one in-process JVM.
#[test]
#[ignore = "requires JAVA_HOME pointing to a JDK with javac and libjvm"]
fn java_adapter_registration_world_query_and_exception_are_connected() {
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Instant;
    use jni::jni_str;
    use lodestone_jvm_bridge::adapter::AdapterEvent;

    let jdk = PathBuf::from(std::env::var_os("JAVA_HOME").expect("JAVA_HOME is required"));
    let output = Command::new("mktemp").arg("-d").output().expect("temporary fixture directory");
    assert!(output.status.success());
    let root = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
    let classes = root.join("classes");
    std::fs::create_dir(&classes).expect("fixture classes directory");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/java/BridgeAdapter.java");
    let compile = Command::new(jdk.join("bin/javac"))
        .arg("-d").arg(&classes).arg(source).output().expect("javac");
    assert!(compile.status.success(), "javac: {}", String::from_utf8_lossy(&compile.stderr));
    let adapter_jar = root.join("adapter.jar");
    let archive = Command::new(jdk.join("bin/jar"))
        .arg("--create").arg("--file").arg(&adapter_jar).arg("-C").arg(&classes).arg(".")
        .output().expect("jar");
    assert!(archive.status.success(), "jar: {}", String::from_utf8_lossy(&archive.stderr));
    let setup_ran = Arc::new(AtomicBool::new(false));
    let setup_observed = Arc::clone(&setup_ran);
    let mut host = AdapterHost::start_with_setup(
        JvmConfig::new().with_classpath(&adapter_jar),
        "lodestone.fixture.BridgeAdapter",
        Duration::from_secs(5),
        move |_, env| {
            env.find_class(jni_str!("java/lang/Object")).map_err(|error| error.to_string())?;
            setup_observed.store(true, Ordering::SeqCst);
            Ok(())
        },
    ).expect("worker startup");
    let mut ready = false;
    let mut success = false;
    let mut queries = Vec::new();
    let limit = Instant::now() + Duration::from_secs(15);
    let failure = loop {
        assert!(Instant::now() < limit, "production adapter did not finish");
        host.service_pending(8, |query| {
            queries.push((query.x, query.y, query.z));
            if query.x < 0 {
                Err("fixture chunk unavailable".to_owned())
            } else {
                Ok((query.x * 31 + query.y * 7 - query.z * 5 + 17) as u32)
            }
        });
        match host.poll() {
            Ok(Some(AdapterEvent::Ready)) => {
                assert!(!ready);
                assert!(setup_ran.load(Ordering::SeqCst), "setup must finish before readiness");
                ready = true;
                host.dispatch_tick(37).unwrap();
            }
            Ok(Some(AdapterEvent::TickCompleted(tick))) => {
                assert_eq!(tick, 37);
                success = true;
                host.dispatch_tick(38).unwrap();
            }
            Err(error) => break error,
            Ok(None) => {}
        }
        std::thread::sleep(Duration::from_millis(1));
    };
    assert!(ready && success, "registration/control callback failed: {failure}");
    assert_eq!(queries, [(11, 7, -3), (-19, 5, 23)]);
    let failure = failure.to_string();
    assert!(failure.contains("onTick(J)V"), "{failure}");
    assert!(failure.contains("RuntimeException"), "{failure}");
    assert!(failure.contains("blockStateId(-19,5,23): fixture chunk unavailable"), "{failure}");
    drop(host);
    std::fs::remove_dir_all(&root).expect("remove generated fixture directory");
}
