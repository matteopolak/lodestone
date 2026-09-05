#![cfg(feature = "jvm")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::sync_channel;
use std::time::{Duration, Instant};

use lodestone_jvm_bridge::adapter::{AdapterEvent, AdapterHost};
use lodestone_jvm_bridge::paper::{
    PaperBootstrapConfig, PaperPluginLifecyclePhase, PaperPluginLifecycleStep,
    PaperServerFacadeInput,
};
use lodestone_jvm_bridge::runtime::JvmConfig;

/// This deliberately uses stand-in archives. It proves only the production
/// loader's retained-entry callback boundary, not Paper startup or plugin
/// compatibility. Run it in a fresh process because JNI permits one JVM.
#[test]
#[ignore = "requires JAVA_HOME pointing to a JDK with javac and libjvm"]
fn lifecycle_entries_run_callbacks_on_the_adapter_worker() {
    let jdk = PathBuf::from(std::env::var_os("JAVA_HOME").expect("JAVA_HOME is required"));
    let fixture = FixtureDirectory::new();
    let paper_sources = fixture.path().join("paper-sources");
    let paper_classes = fixture.path().join("paper-classes");
    let shim_sources = fixture.path().join("shim-sources");
    let shim_classes = fixture.path().join("shim-classes");
    let adapter_sources = fixture.path().join("adapter-sources");
    let adapter_classes = fixture.path().join("adapter-classes");
    for path in [
        &paper_sources,
        &paper_classes,
        &shim_sources,
        &shim_classes,
        &adapter_sources,
        &adapter_classes,
    ] {
        fs::create_dir_all(path).expect("fixture directory");
    }
    let bootstrap_source = paper_sources.join("PaperBootstrap.java");
    fs::write(
        &bootstrap_source,
        "package io.papermc.paper; public final class PaperBootstrap { static { if (System.nanoTime() != Long.MIN_VALUE) throw new AssertionError(\"bootstrap initialized\"); } }",
    )
    .expect("bootstrap source");
    let shim_package = shim_sources.join("lodestone/bridge");
    fs::create_dir_all(&shim_package).expect("shim package directory");
    let shim_source = shim_package.join("IsolatedPaperShim.java");
    fs::write(
        &shim_source,
        "package lodestone.bridge; public final class IsolatedPaperShim { \
         public static native int blockStateId(int x, int y, int z); \
         public static native long serverTickCount(); \
         public static native int setBlockStateId(int x, int y, int z, int stateId); \
         public static native String currentPluginName(); \
         public static native String currentPluginVersion(); }",
    )
    .expect("shim source");
    let adapter_package = adapter_sources.join("fixture/adapter");
    fs::create_dir_all(&adapter_package).expect("adapter package directory");
    let adapter_source = adapter_package.join("LifecycleAdapter.java");
    fs::write(
        &adapter_source,
        "package fixture.adapter; public final class LifecycleAdapter { \
         private static native int blockStateId(int x, int y, int z); \
         public static void onTick(long tick) {} \
         public static void onBlockStateChanged(int x, int y, int z, int stateId) {} }",
    )
    .expect("adapter source");
    compile(&jdk, &paper_classes, &bootstrap_source);
    compile(&jdk, &shim_classes, &shim_source);
    compile(&jdk, &adapter_classes, &adapter_source);
    let manifest = fixture.path().join("MANIFEST.MF");
    fs::write(&manifest, "Manifest-Version: 1.0\nImplementation-Title: Paper\n")
        .expect("Paper manifest");
    let paper_jar = archive(
        &jdk,
        fixture.path(),
        "paper.jar",
        &paper_classes,
        Some(&manifest),
    );
    let plugins = fixture.path().join("plugins");
    fs::create_dir(&plugins).expect("plugins directory");
    for (jar, name, package, fail_enable, fail_disable) in [
        ("a-alpha.jar", "Alpha", "fixture.alpha", false, false),
        ("b-bravo.jar", "Bravo", "fixture.bravo", true, false),
        ("c-charlie.jar", "Charlie", "fixture.charlie", false, true),
        ("d-delta.jar", "Delta", "fixture.delta", false, false),
    ] {
        let source_root = fixture.path().join(format!("{name}-sources"));
        let classes = fixture.path().join(format!("{name}-classes"));
        fs::create_dir_all(&source_root).expect("plugin source directory");
        fs::create_dir_all(&classes).expect("plugin classes directory");
        let source = source_root.join("Main.java");
        fs::write(
            &source,
            callback_plugin_source(package, name, fail_enable, fail_disable),
        )
        .expect("plugin source");
        compile_with_classpath(&jdk, &classes, &source, &shim_classes);
        fs::write(
            classes.join("plugin.yml"),
            format!("name: {name}\nversion: one\nmain: {package}.Main\n"),
        )
        .expect("plugin descriptor");
        archive(&jdk, &plugins, jar, &classes, None);
    }

    let plan = PaperBootstrapConfig::new(&paper_jar, &plugins)
        .with_shim_path(&shim_classes)
        .with_isolated_native_shim()
        .discover()
        .expect("discover stand-in lifecycle inputs");
    let (status_sender, status_receiver) = sync_channel(1);
    let callback_log = fixture.path().join("callbacks.log");
    let mut host = AdapterHost::start_with_setup(
        JvmConfig::new()
            .with_classpath(&adapter_classes)
            .with_option(format!("-Dfixture.lifecycle.log={}", callback_log.display())),
        "fixture.adapter.LifecycleAdapter",
        Duration::from_secs(5),
        move |runtime, env, native_surface| {
            let lifecycle = plan.load_lifecycle_entries_in_runtime(runtime, env)
                .expect("load retained entry classes");
            assert_eq!(lifecycle.loader_count(), 5, "retain bootstrap and plugin loaders");
            assert!(lifecycle.retains_bootstrap_loader());
            assert_eq!(lifecycle.loaded_plugins()[3].descriptor().name(), "Delta");
            assert!(lifecycle.loaded_plugins()[3].retains_entry_association());
            let construction = lifecycle
                .into_construction_plan(
                    env,
                    PaperServerFacadeInput::native_server_surface(native_surface),
                )
                .expect("retain entry-only construction state")
                .construct_entries(env);
            assert_eq!(construction.plugins().len(), 4);
            assert!(construction.plugins()[0].retains_instance());
            let enablement = construction.enable_entries(env);
            assert_eq!(enablement.plugins().len(), 3);
            let disablement = enablement.disable_entries(env);
            assert_eq!(disablement.plugins().len(), 3);
            status_sender
                .send(disablement.status().clone())
                .map_err(|error| format!("send construction status: {error}"))?;
            Ok(disablement)
        },
    )
    .expect("start lifecycle adapter worker");
    let limit = Instant::now() + Duration::from_secs(5);
    loop {
        match host.poll().expect("lifecycle adapter readiness") {
            Some(AdapterEvent::Ready) => break,
            Some(AdapterEvent::TickCompleted(tick)) => panic!("unexpected adapter tick {tick}"),
            Some(AdapterEvent::BlockStateChangedCompleted(change)) => {
                panic!("unexpected adapter block-change callback {change:?}")
            }
            None => assert!(Instant::now() < limit, "lifecycle adapter did not become ready"),
        }
        std::thread::yield_now();
    }
    let status = status_receiver
        .try_recv()
        .expect("worker must publish callback status before readiness");
    assert_eq!(status.plugins()[0].descriptor().name(), "Alpha");
    assert_eq!(status.plugins()[0].phase(), PaperPluginLifecyclePhase::Disabled);
    assert_eq!(status.plugins()[1].descriptor().name(), "Bravo");
    assert_eq!(status.plugins()[1].phase(), PaperPluginLifecyclePhase::Failed);
    assert_eq!(
        status.plugins()[1].failure().expect("enable failure").step(),
        PaperPluginLifecycleStep::Enable,
    );
    assert_eq!(status.plugins()[2].descriptor().name(), "Charlie");
    assert_eq!(status.plugins()[2].phase(), PaperPluginLifecyclePhase::Failed);
    assert_eq!(
        status.plugins()[2].failure().expect("disable failure").step(),
        PaperPluginLifecycleStep::Disable,
    );
    assert_eq!(status.plugins()[3].descriptor().name(), "Delta");
    assert_eq!(status.plugins()[3].phase(), PaperPluginLifecyclePhase::Disabled);
    assert_eq!(
        fs::read_to_string(&callback_log).expect("callback log"),
        "Alpha-construct:Alpha:one\nBravo-construct:Bravo:one\nCharlie-construct:Charlie:one\nDelta-construct:Delta:one\nAlpha-enable:Alpha:one\nBravo-enable:Bravo:one\nCharlie-enable:Charlie:one\nDelta-enable:Delta:one\nDelta-disable:Delta:one\nCharlie-disable:Charlie:one\nAlpha-disable:Alpha:one\n",
    );
}

fn callback_plugin_source(
    package: &str,
    name: &str,
    fail_enable: bool,
    fail_disable: bool,
) -> String {
    let enable_failure = fail_enable.then_some("throw new IllegalStateException(\"enable failure\");").unwrap_or("");
    let disable_failure = fail_disable.then_some("throw new IllegalStateException(\"disable failure\");").unwrap_or("");
    format!(
        "package {package}; public final class Main {{ \
         private static void log(String event) {{ try {{ java.nio.file.Files.writeString( \
         java.nio.file.Path.of(System.getProperty(\"fixture.lifecycle.log\")), event + \"\\n\", \
         java.nio.file.StandardOpenOption.CREATE, java.nio.file.StandardOpenOption.APPEND); \
         }} catch (java.io.IOException error) {{ throw new RuntimeException(error); }} }} \
         private static String identity() {{ return lodestone.bridge.IsolatedPaperShim.currentPluginName() + \":\" + lodestone.bridge.IsolatedPaperShim.currentPluginVersion(); }} \
         public Main() {{ log(\"{name}-construct:\" + identity()); }} \
         public void onEnable() {{ log(\"{name}-enable:\" + identity()); {enable_failure} }} \
         public void onDisable() {{ log(\"{name}-disable:\" + identity()); {disable_failure} }} }}"
    )
}

struct FixtureDirectory(PathBuf);

impl FixtureDirectory {
    fn new() -> Self {
        let output = Command::new("mktemp")
            .arg("-d")
            .output()
            .expect("fixture directory");
        assert!(output.status.success(), "mktemp fixture directory");
        let path = PathBuf::from(String::from_utf8(output.stdout).expect("UTF-8 fixture path").trim());
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for FixtureDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn compile(jdk: &Path, output: &Path, source: &Path) {
    let result = Command::new(jdk.join("bin/javac"))
        .arg("-d")
        .arg(output)
        .arg(source)
        .output()
        .expect("javac");
    assert!(
        result.status.success(),
        "javac: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

fn compile_with_classpath(jdk: &Path, output: &Path, source: &Path, classpath: &Path) {
    let result = Command::new(jdk.join("bin/javac"))
        .arg("-classpath")
        .arg(classpath)
        .arg("-d")
        .arg(output)
        .arg(source)
        .output()
        .expect("javac");
    assert!(
        result.status.success(),
        "javac: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

fn archive(
    jdk: &Path,
    root: &Path,
    name: &str,
    classes: &Path,
    manifest: Option<&Path>,
) -> PathBuf {
    let jar = root.join(name);
    let mut command = Command::new(jdk.join("bin/jar"));
    command.arg("--create").arg("--file").arg(&jar);
    if let Some(manifest) = manifest {
        command.arg("--manifest").arg(manifest);
    }
    let result = command
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
