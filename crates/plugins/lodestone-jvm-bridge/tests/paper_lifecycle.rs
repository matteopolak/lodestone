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
/// loader's retained-entry construction boundary, not Paper startup or plugin
/// compatibility. Run it in a fresh process because JNI permits one JVM.
#[test]
#[ignore = "requires JAVA_HOME pointing to a JDK with javac and libjvm"]
fn lifecycle_entries_construct_on_the_adapter_worker() {
    let jdk = PathBuf::from(std::env::var_os("JAVA_HOME").expect("JAVA_HOME is required"));
    let fixture = FixtureDirectory::new();
    let paper_sources = fixture.path().join("paper-sources");
    let paper_classes = fixture.path().join("paper-classes");
    let plugin_sources = fixture.path().join("plugin-sources");
    let plugin_classes = fixture.path().join("plugin-classes");
    let failing_sources = fixture.path().join("failing-sources");
    let failing_classes = fixture.path().join("failing-classes");
    let adapter_sources = fixture.path().join("adapter-sources");
    let adapter_classes = fixture.path().join("adapter-classes");
    for path in [
        &paper_sources,
        &paper_classes,
        &plugin_sources,
        &plugin_classes,
        &failing_sources,
        &failing_classes,
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
        "package fixture.plugin; public final class Main { \
         private static int constructions; \
         public Main() { constructions++; } \
         public static int constructions() { return constructions; } }",
    )
    .expect("plugin source");
    let failing_package = failing_sources.join("fixture/failing");
    fs::create_dir_all(&failing_package).expect("failing plugin package directory");
    let failing_source = failing_package.join("Main.java");
    fs::write(
        &failing_source,
        "package fixture.failing; public final class Main { \
         public Main() { throw new IllegalStateException(\"deliberate fixture failure\"); } }",
    )
    .expect("failing plugin source");
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
    compile(&jdk, &plugin_classes, &plugin_source);
    compile(&jdk, &failing_classes, &failing_source);
    compile(&jdk, &adapter_classes, &adapter_source);
    fs::write(
        plugin_classes.join("plugin.yml"),
        "name: Fixture\nversion: one\nmain: fixture.plugin.Main\n",
    )
    .expect("plugin descriptor");
    fs::write(
        failing_classes.join("plugin.yml"),
        "name: Failing\nversion: one\nmain: fixture.failing.Main\n",
    )
    .expect("failing plugin descriptor");
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
    archive(&jdk, &plugins, "a-failing.jar", &failing_classes, None);
    archive(&jdk, &plugins, "b-fixture.jar", &plugin_classes, None);

    let plan = PaperBootstrapConfig::new(&paper_jar, &plugins)
        .discover()
        .expect("discover stand-in lifecycle inputs");
    let (status_sender, status_receiver) = sync_channel(1);
    let mut host = AdapterHost::start_with_setup(
        JvmConfig::new().with_classpath(&adapter_classes),
        "fixture.adapter.LifecycleAdapter",
        Duration::from_secs(5),
        move |runtime, env, native_surface| {
            let lifecycle = plan.load_lifecycle_entries_in_runtime(runtime, env)
                .expect("load retained entry classes");
            assert_eq!(lifecycle.loader_count(), 3, "retain bootstrap and plugin loaders");
            assert!(lifecycle.retains_bootstrap_loader());
            assert_eq!(lifecycle.loaded_plugins()[1].descriptor().name(), "Fixture");
            assert!(lifecycle.loaded_plugins()[1].retains_entry_association());
            let construction = lifecycle
                .into_construction_plan(
                    env,
                    PaperServerFacadeInput::entry_construction_only(native_surface),
                )
                .expect("retain entry-only construction state")
                .construct_entries(env);
            assert_eq!(construction.plugins().len(), 1);
            assert!(construction.plugins()[0].retains_instance());
            status_sender
                .send(construction.status().clone())
                .map_err(|error| format!("send construction status: {error}"))?;
            Ok(construction)
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
        .expect("worker must publish construction status before readiness");
    assert_eq!(status.plugins()[0].descriptor().name(), "Failing");
    assert_eq!(
        status.plugins()[0].phase(),
        PaperPluginLifecyclePhase::Failed,
    );
    let failure = status.plugins()[0].failure().expect("constructor failure is retained");
    assert_eq!(failure.step(), PaperPluginLifecycleStep::Construct);
    assert!(failure.message().contains("deliberate fixture failure"), "{failure:?}");
    assert_eq!(status.plugins()[1].descriptor().name(), "Fixture");
    assert_eq!(status.plugins()[1].phase(), PaperPluginLifecyclePhase::Constructed);
    assert_eq!(status.plugins()[1].failure(), None);
    assert!(PaperPluginLifecyclePhase::Constructed
        .accepts(PaperPluginLifecycleStep::Enable));
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
