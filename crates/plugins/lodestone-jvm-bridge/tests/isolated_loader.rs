#![cfg(feature = "jvm")]

use std::path::{Path, PathBuf};
use std::process::Command;

use jni::objects::JString;
use jni::{jni_sig, jni_str};
use lodestone_jvm_bridge::runtime::{JvmConfig, JvmRuntime};

/// Runs in a separate test binary because a process can start only one JVM.
/// The app class is compiled once against the real target. The same production
/// loader must select REAL with the original jar first and SHIM when the shim
/// jar is first, proving ordered interception without rewriting either jar.
#[test]
#[ignore = "requires JAVA_HOME pointing to a JDK with javac and libjvm"]
fn ordered_operator_jars_select_the_intercepting_definition() {
    let jdk = PathBuf::from(std::env::var_os("JAVA_HOME").expect("JAVA_HOME is required"));
    let output = Command::new("mktemp")
        .arg("-d")
        .output()
        .expect("temporary fixture directory");
    assert!(output.status.success());
    let root = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
    let real_classes = root.join("real-classes");
    let shim_classes = root.join("shim-classes");
    let app_classes = root.join("app-classes");
    for path in [&real_classes, &shim_classes, &app_classes] {
        std::fs::create_dir(path).expect("fixture classes directory");
    }
    let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/java/loader");
    compile(
        &jdk,
        &real_classes,
        &source_root.join("real/lodestone/target/World.java"),
        None,
    );
    compile(
        &jdk,
        &shim_classes,
        &source_root.join("shim/lodestone/target/World.java"),
        None,
    );
    compile(
        &jdk,
        &app_classes,
        &source_root.join("app/lodestone/fixture/LoaderProbe.java"),
        Some(&real_classes),
    );
    let real_jar = archive(&jdk, &root, "real.jar", &real_classes);
    let shim_jar = archive(&jdk, &root, "shim.jar", &shim_classes);
    let app_jar = archive(&jdk, &root, "app.jar", &app_classes);

    let runtime = JvmRuntime::start(&JvmConfig::new()).expect("start isolated JVM");
    runtime.with_attached_thread(|env| {
        let control = runtime.load_isolated_class(
            env,
            &JvmConfig::new().with_classpath(&real_jar).with_classpath(&app_jar),
            "lodestone.fixture.LoaderProbe",
        )
        .expect("load control probe");
        assert_eq!(source(env, &control)?, "REAL");
        let intercepted = runtime.load_isolated_class(
            env,
            &JvmConfig::new()
                .with_classpath(&shim_jar)
                .with_classpath(&app_jar)
                .with_classpath(&real_jar),
            "lodestone.fixture.LoaderProbe",
        )
        .expect("load intercepted probe");
        assert_eq!(source(env, &intercepted)?, "SHIM");
        Ok(())
    }).expect("run isolated loader fixture");
    std::fs::remove_dir_all(&root).expect("remove generated fixture directory");
}

fn compile(jdk: &Path, output: &Path, source: &Path, classpath: Option<&Path>) {
    let mut command = Command::new(jdk.join("bin/javac"));
    command.arg("-d").arg(output);
    if let Some(classpath) = classpath {
        command.arg("-cp").arg(classpath);
    }
    let result = command.arg(source).output().expect("javac");
    assert!(
        result.status.success(),
        "javac: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

fn archive(jdk: &Path, root: &Path, name: &str, classes: &Path) -> PathBuf {
    let jar = root.join(name);
    let result = Command::new(jdk.join("bin/jar"))
        .arg("--create")
        .arg("--file")
        .arg(&jar)
        .arg("-C")
        .arg(classes)
        .arg(".")
        .output()
        .expect("jar");
    assert!(
        result.status.success(),
        "jar: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    jar
}

fn source(
    env: &mut jni::Env<'_>,
    class: &jni::objects::JClass<'_>,
) -> jni::errors::Result<String> {
    let value = env.call_static_method(
        class,
        jni_str!("source"),
        jni_sig!("()Ljava/lang/String;"),
        &[],
    )?.l()?;
    let value = env.cast_local::<JString>(value)?;
    value.try_to_string(env)
}
