#![cfg(feature = "jvm")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::sync_channel;
use std::time::{Duration, Instant};

use lodestone_jvm_bridge::adapter::{AdapterEvent, AdapterHost};
use lodestone_jvm_bridge::paper::{
    PaperBootstrapConfig, PaperServerFacadeInput, PaperServerFacadeState,
};
use lodestone_jvm_bridge::runtime::JvmConfig;

/// This deliberately uses stand-in archives. It proves only the production
/// loader's non-initializing lifecycle boundary, not Paper startup or plugin
/// compatibility. Run it in a fresh process because JNI permits one JVM.
#[test]
#[ignore = "requires JAVA_HOME pointing to a JDK with javac and libjvm"]
fn lifecycle_entries_load_without_initialization() {
    let jdk = PathBuf::from(std::env::var_os("JAVA_HOME").expect("JAVA_HOME is required"));
    let fixture = FixtureDirectory::new();
    let paper_sources = fixture.path().join("paper-sources");
    let paper_classes = fixture.path().join("paper-classes");
    let plugin_sources = fixture.path().join("plugin-sources");
    let plugin_classes = fixture.path().join("plugin-classes");
    let shim_sources = fixture.path().join("shim-sources");
    let shim_classes = fixture.path().join("shim-classes");
    let adapter_sources = fixture.path().join("adapter-sources");
    let adapter_classes = fixture.path().join("adapter-classes");
    for path in [
        &paper_sources,
        &paper_classes,
        &plugin_sources,
        &plugin_classes,
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
    let plugin_source = plugin_sources.join("Main.java");
    fs::write(
        &plugin_source,
        "package fixture.plugin; public final class Main { static { if (System.nanoTime() != Long.MIN_VALUE) throw new AssertionError(\"plugin initialized\"); } }",
    )
    .expect("plugin source");
    let shim_package = shim_sources.join("lodestone/bridge");
    fs::create_dir_all(&shim_package).expect("shim package directory");
    let shim_source = shim_package.join("IsolatedPaperShim.java");
    fs::write(
        &shim_source,
        "package lodestone.bridge; public final class IsolatedPaperShim { \
         static { if (System.nanoTime() != Long.MIN_VALUE) throw new AssertionError(\"shim initialized\"); } \
         public static native int blockStateId(int x, int y, int z); }",
    )
    .expect("shim source");
    let adapter_package = adapter_sources.join("fixture/adapter");
    fs::create_dir_all(&adapter_package).expect("adapter package directory");
    let adapter_source = adapter_package.join("LifecycleAdapter.java");
    fs::write(
        &adapter_source,
        "package fixture.adapter; public final class LifecycleAdapter { \
         private static native int blockStateId(int x, int y, int z); \
         public static void onTick(long tick) {} }",
    )
    .expect("adapter source");
    compile(&jdk, &paper_classes, &bootstrap_source);
    compile(&jdk, &plugin_classes, &plugin_source);
    compile(&jdk, &shim_classes, &shim_source);
    compile(&jdk, &adapter_classes, &adapter_source);
    fs::write(
        plugin_classes.join("plugin.yml"),
        "name: Fixture\nversion: one\nmain: fixture.plugin.Main\n",
    )
    .expect("plugin descriptor");
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
    archive(&jdk, &plugins, "fixture.jar", &plugin_classes, None);

    let plan = PaperBootstrapConfig::new(&paper_jar, &plugins)
        .with_shim_path(&shim_classes)
        .with_isolated_native_shim()
        .discover()
        .expect("discover stand-in lifecycle inputs");
    let (construction_sender, construction_receiver) = sync_channel(1);
    let mut host = AdapterHost::start_with_setup(
        JvmConfig::new().with_classpath(&adapter_classes),
        "fixture.adapter.LifecycleAdapter",
        Duration::from_secs(5),
        move |runtime, env, native_surface| {
            let lifecycle = plan.load_lifecycle_entries_in_runtime(runtime, env)
                .expect("load classes without running static initializers");
            assert_eq!(lifecycle.loader_count(), 2, "retain bootstrap and plugin loaders");
            assert!(lifecycle.retains_bootstrap_loader());
            assert_eq!(lifecycle.loaded_plugins()[0].descriptor().name(), "Fixture");
            assert!(lifecycle.loaded_plugins()[0].retains_entry_association());
            let construction = lifecycle
                .into_construction_plan(PaperServerFacadeInput::native_block_state_read(
                    native_surface,
                ))
                .expect("retain worker-owned native facade state");
            construction_sender
                .send(construction.readiness().clone())
                .map_err(|error| format!("send construction readiness: {error}"))?;
            Ok(construction)
        },
    )
    .expect("start lifecycle adapter worker");
    let limit = Instant::now() + Duration::from_secs(5);
    loop {
        match host.poll().expect("lifecycle adapter readiness") {
            Some(AdapterEvent::Ready) => break,
            Some(AdapterEvent::TickCompleted(tick)) => panic!("unexpected adapter tick {tick}"),
            None => assert!(Instant::now() < limit, "lifecycle adapter did not become ready"),
        }
        std::thread::yield_now();
    }
    let readiness = construction_receiver
        .try_recv()
        .expect("worker must publish construction state before readiness");
    assert_eq!(readiness.facade(), PaperServerFacadeState::NativeBlockStateRead);
    assert_eq!(readiness.plugins()[0].descriptor().name(), "Fixture");
    assert_eq!(
        readiness.plugins()[0].blocker().to_string(),
        "the installed server capability cannot supply plugin construction semantics",
    );
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
