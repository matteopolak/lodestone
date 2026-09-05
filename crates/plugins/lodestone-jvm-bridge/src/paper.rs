//! Paper bootstrap and plugin-descriptor discovery for an operator-supplied set of jars.
//!
//! This module opens jars but never extracts them: descriptor names are exact
//! archive lookups, descriptor reads are bounded, and every selected path stays
//! an operator path. Discovery happens before JVM startup, so invalid metadata
//! fails without loading arbitrary plugin bytecode or touching a world port.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use jni::objects::JClass;
use jni::Env;
use zip::ZipArchive;

use crate::runtime::{JvmConfig, JvmRuntime};

const BOOTSTRAP_CLASS: &str = "io.papermc.paper.PaperBootstrap";
const BOOTSTRAP_ENTRY: &str = "io/papermc/paper/PaperBootstrap.class";
const PAPER_MANIFEST_TITLE: &str = "Implementation-Title: Paper";
const PAPER_DESCRIPTOR: &str = "paper-plugin.yml";
const BUKKIT_DESCRIPTOR: &str = "plugin.yml";
const MAX_DESCRIPTOR_BYTES: u64 = 64 * 1024;
const DEFAULT_MAX_PLUGINS: usize = 256;

/// Operator paths needed to inspect one Paper server jar and its plugin jars.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaperBootstrapConfig {
    paper_jar: PathBuf,
    plugins_directory: PathBuf,
    shim_paths: Vec<PathBuf>,
    max_plugins: usize,
}

impl PaperBootstrapConfig {
    /// Starts a plan for this explicit Paper jar and plugin directory.
    #[must_use]
    pub fn new(paper_jar: impl AsRef<Path>, plugins_directory: impl AsRef<Path>) -> Self {
        Self {
            paper_jar: paper_jar.as_ref().to_owned(),
            plugins_directory: plugins_directory.as_ref().to_owned(),
            shim_paths: Vec::new(),
            max_plugins: DEFAULT_MAX_PLUGINS,
        }
    }

    /// Adds a directory or jar before the Paper jar in bootstrap resolution.
    ///
    /// Shim ordering is deliberate: the isolated loader resolves a matching
    /// class from these paths before the Paper jar, without modifying either.
    #[must_use]
    pub fn with_shim_path(mut self, path: impl AsRef<Path>) -> Self {
        self.shim_paths.push(path.as_ref().to_owned());
        self
    }

    /// Limits discovered plugin jars before opening any descriptor.
    #[must_use]
    pub fn with_max_plugins(mut self, max_plugins: usize) -> Self {
        self.max_plugins = max_plugins;
        self
    }

    /// Validates the server jar and returns sorted, metadata-checked plugins.
    pub fn discover(self) -> Result<PaperBootstrapPlan, PaperBootstrapError> {
        if self.max_plugins == 0 {
            return Err(PaperBootstrapError::new("Paper plugin limit must be positive"));
        }
        validate_paper_jar(&self.paper_jar)?;
        for shim in &self.shim_paths {
            validate_operator_path(shim, "shim")?;
        }
        let plugin_jars = plugin_jars(&self.plugins_directory, self.max_plugins)?;
        let mut names = BTreeSet::new();
        let mut plugins = Vec::with_capacity(plugin_jars.len());
        for jar in plugin_jars {
            let descriptor = discover_plugin(&jar)?;
            let key = descriptor.name.to_ascii_lowercase();
            if !names.insert(key) {
                return Err(PaperBootstrapError::new(format!(
                    "duplicate plugin name {:?} in {}",
                    descriptor.name,
                    jar.display()
                )));
            }
            plugins.push(descriptor);
        }
        Ok(PaperBootstrapPlan {
            paper_jar: self.paper_jar,
            shim_paths: self.shim_paths,
            plugins,
        })
    }
}

/// A validated, deterministic input set for a later Paper lifecycle host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaperBootstrapPlan {
    paper_jar: PathBuf,
    shim_paths: Vec<PathBuf>,
    plugins: Vec<PaperPluginDescriptor>,
}

impl PaperBootstrapPlan {
    /// The user-supplied Paper server jar selected for this plan.
    pub fn paper_jar(&self) -> &Path {
        &self.paper_jar
    }

    /// Validated plugins in stable jar-path order.
    pub fn plugins(&self) -> &[PaperPluginDescriptor] {
        &self.plugins
    }

    /// Builds the ordered classpath for the server bootstrap only.
    ///
    /// Plugin jars stay out of this loader until a lifecycle host gives each
    /// plugin its own class-loader policy; discovery must not accidentally
    /// make one plugin's classes visible to another.
    fn bootstrap_loader_config(&self) -> JvmConfig {
        self.shim_paths.iter().chain(std::iter::once(&self.paper_jar)).fold(
            JvmConfig::new(),
            |config, path| config.with_classpath(path),
        )
    }

    /// Starts the JVM without placing operator jars on its system classpath.
    ///
    /// [`Self::load_bootstrap`] supplies the ordered operator paths to the
    /// isolated loader instead. Keeping the system loader empty prevents an
    /// accidental system-loader lookup from defeating shim-first resolution.
    pub fn start_runtime(&self) -> Result<JvmRuntime, PaperBootstrapError> {
        JvmRuntime::start(&JvmConfig::new()).map_err(|error| {
            PaperBootstrapError::new(format!("could not start Paper JVM: {error}"))
        })
    }

    /// Loads, but does not initialize or invoke, Paper's bootstrap class.
    ///
    /// This is the real host-callable consumer of the validated plan. Calling
    /// it is still not plugin enablement: the caller must install the native
    /// surface and lifecycle policy before invoking any Paper or plugin code.
    pub fn load_bootstrap<'local>(
        &self,
        runtime: &JvmRuntime,
        env: &mut Env<'local>,
    ) -> Result<JClass<'local>, PaperBootstrapError> {
        runtime
            .load_isolated_class(env, &self.bootstrap_loader_config(), BOOTSTRAP_CLASS)
            .map_err(|error| PaperBootstrapError::new(format!(
                "could not load Paper bootstrap class {BOOTSTRAP_CLASS}: {error}"
            )))
    }
}

/// The metadata needed to choose a future plugin lifecycle policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaperPluginDescriptor {
    jar: PathBuf,
    kind: PaperPluginDescriptorKind,
    name: String,
    version: String,
    main_class: String,
}

impl PaperPluginDescriptor {
    /// The operator-supplied jar containing this descriptor.
    pub fn jar(&self) -> &Path {
        &self.jar
    }

    /// Which supported descriptor supplied this metadata.
    pub fn kind(&self) -> PaperPluginDescriptorKind {
        self.kind
    }

    /// Plugin identity, validated before lifecycle work begins.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Plugin version text, retained for diagnostics and a later lifecycle.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Dotted Java binary name for the plugin entry class.
    pub fn main_class(&self) -> &str {
        &self.main_class
    }
}

/// The two descriptor formats accepted by Paper.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaperPluginDescriptorKind {
    /// Paper's own descriptor format.
    Paper,
    /// Bukkit's compatibility descriptor format.
    Bukkit,
}

/// A bounded, actionable jar or descriptor discovery failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaperBootstrapError(String);

impl PaperBootstrapError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for PaperBootstrapError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for PaperBootstrapError {}

fn validate_paper_jar(path: &Path) -> Result<(), PaperBootstrapError> {
    validate_operator_path(path, "Paper server jar")?;
    let mut archive = open_jar(path)?;
    require_exact_entry(&mut archive, BOOTSTRAP_ENTRY, path)?;
    let manifest = read_exact_entry(&mut archive, "META-INF/MANIFEST.MF", path)?;
    let manifest = std::str::from_utf8(&manifest).map_err(|error| {
        PaperBootstrapError::new(format!("Paper manifest {} is not UTF-8: {error}", path.display()))
    })?;
    if !manifest.lines().any(|line| line.trim_end() == PAPER_MANIFEST_TITLE) {
        return Err(PaperBootstrapError::new(format!(
            "Paper server jar {} lacks {PAPER_MANIFEST_TITLE:?}",
            path.display()
        )));
    }
    Ok(())
}

fn validate_operator_path(path: &Path, kind: &str) -> Result<(), PaperBootstrapError> {
    let metadata = fs::metadata(path).map_err(|error| {
        PaperBootstrapError::new(format!("{kind} {} is unavailable: {error}", path.display()))
    })?;
    if !metadata.is_file() && !metadata.is_dir() {
        return Err(PaperBootstrapError::new(format!(
            "{kind} {} is neither a file nor a directory",
            path.display()
        )));
    }
    Ok(())
}

fn plugin_jars(directory: &Path, max_plugins: usize) -> Result<Vec<PathBuf>, PaperBootstrapError> {
    let metadata = fs::metadata(directory).map_err(|error| {
        PaperBootstrapError::new(format!("plugin directory {} is unavailable: {error}", directory.display()))
    })?;
    if !metadata.is_dir() {
        return Err(PaperBootstrapError::new(format!(
            "plugin directory {} is not a directory",
            directory.display()
        )));
    }
    let mut jars = Vec::new();
    for entry in fs::read_dir(directory).map_err(|error| {
        PaperBootstrapError::new(format!("could not read plugin directory {}: {error}", directory.display()))
    })? {
        let entry = entry.map_err(|error| {
            PaperBootstrapError::new(format!("could not read plugin directory entry: {error}"))
        })?;
        let path = entry.path();
        if entry.file_type().map_err(|error| {
            PaperBootstrapError::new(format!("could not inspect plugin path {}: {error}", path.display()))
        })?.is_file() && path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("jar")) {
            jars.push(path);
        }
    }
    jars.sort();
    if jars.len() > max_plugins {
        return Err(PaperBootstrapError::new(format!(
            "plugin directory {} has {} jars, exceeding the limit of {max_plugins}",
            directory.display(),
            jars.len()
        )));
    }
    Ok(jars)
}

fn discover_plugin(jar: &Path) -> Result<PaperPluginDescriptor, PaperBootstrapError> {
    let mut archive = open_jar(jar)?;
    let paper_entries = entry_count(&mut archive, PAPER_DESCRIPTOR, jar)?;
    let bukkit_entries = entry_count(&mut archive, BUKKIT_DESCRIPTOR, jar)?;
    if paper_entries > 1 || bukkit_entries > 1 {
        return Err(PaperBootstrapError::new(format!(
            "plugin jar {} has duplicate descriptor entries",
            jar.display()
        )));
    }
    let (name, kind) = match (paper_entries, bukkit_entries) {
        (1, 0) => (PAPER_DESCRIPTOR, PaperPluginDescriptorKind::Paper),
        (0, 1) => (BUKKIT_DESCRIPTOR, PaperPluginDescriptorKind::Bukkit),
        (0, 0) => {
            return Err(PaperBootstrapError::new(format!(
                "plugin jar {} has neither {PAPER_DESCRIPTOR} nor {BUKKIT_DESCRIPTOR}",
                jar.display()
            )));
        }
        (1, 1) => {
            return Err(PaperBootstrapError::new(format!(
                "plugin jar {} has both {PAPER_DESCRIPTOR} and {BUKKIT_DESCRIPTOR}; refusing ambiguous metadata",
                jar.display()
            )));
        }
        _ => unreachable!("descriptor counts above one returned early"),
    };
    let bytes = read_exact_entry(&mut archive, name, jar)?;
    let text = std::str::from_utf8(&bytes).map_err(|error| {
        PaperBootstrapError::new(format!("descriptor {name} in {} is not UTF-8: {error}", jar.display()))
    })?;
    let fields = scalar_fields(text, name, jar)?;
    let plugin_name = required_field(&fields, "name", name, jar)?;
    let version = required_field(&fields, "version", name, jar)?;
    let main_class = required_field(&fields, "main", name, jar)?;
    validate_plugin_name(&plugin_name, name, jar)?;
    validate_scalar("version", &version, name, jar)?;
    validate_class_name(&main_class, name, jar)?;
    Ok(PaperPluginDescriptor {
        jar: jar.to_owned(),
        kind,
        name: plugin_name,
        version,
        main_class,
    })
}

fn open_jar(path: &Path) -> Result<ZipArchive<File>, PaperBootstrapError> {
    let file = File::open(path).map_err(|error| {
        PaperBootstrapError::new(format!("could not open jar {}: {error}", path.display()))
    })?;
    ZipArchive::new(file).map_err(|error| {
        PaperBootstrapError::new(format!("could not read jar {}: {error}", path.display()))
    })
}

fn entry_count(
    archive: &mut ZipArchive<File>,
    name: &str,
    path: &Path,
) -> Result<usize, PaperBootstrapError> {
    let mut count = 0;
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(|error| {
            PaperBootstrapError::new(format!("could not inspect jar {}: {error}", path.display()))
        })?;
        if entry.name() == name {
            count += 1;
        }
    }
    Ok(count)
}

fn require_exact_entry(
    archive: &mut ZipArchive<File>,
    name: &str,
    path: &Path,
) -> Result<(), PaperBootstrapError> {
    if entry_count(archive, name, path)? != 1 {
        return Err(PaperBootstrapError::new(format!(
            "jar {} must contain exactly one {name}",
            path.display()
        )));
    }
    Ok(())
}

fn read_exact_entry(
    archive: &mut ZipArchive<File>,
    name: &str,
    path: &Path,
) -> Result<Vec<u8>, PaperBootstrapError> {
    require_exact_entry(archive, name, path)?;
    let entry = archive.by_name(name).map_err(|error| {
        PaperBootstrapError::new(format!("could not read {name} from {}: {error}", path.display()))
    })?;
    if entry.size() > MAX_DESCRIPTOR_BYTES {
        return Err(PaperBootstrapError::new(format!(
            "archive entry {name} in {} exceeds {MAX_DESCRIPTOR_BYTES} bytes",
            path.display()
        )));
    }
    let mut bytes = Vec::with_capacity(entry.size() as usize);
    entry.take(MAX_DESCRIPTOR_BYTES + 1).read_to_end(&mut bytes).map_err(|error| {
        PaperBootstrapError::new(format!("could not read {name} from {}: {error}", path.display()))
    })?;
    if bytes.len() as u64 > MAX_DESCRIPTOR_BYTES {
        return Err(PaperBootstrapError::new(format!(
            "archive entry {name} in {} exceeds {MAX_DESCRIPTOR_BYTES} bytes",
            path.display()
        )));
    }
    Ok(bytes)
}

fn scalar_fields(
    text: &str,
    descriptor: &str,
    jar: &Path,
) -> Result<BTreeMap<String, String>, PaperBootstrapError> {
    let mut fields = BTreeMap::new();
    for (line_number, line) in text.lines().enumerate() {
        if line.chars().next().is_some_and(char::is_whitespace)
            || line.trim().is_empty()
            || line.trim_start().starts_with('#')
        {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        if !matches!(key, "name" | "version" | "main") {
            continue;
        }
        if value.is_empty() || matches!(value, "|" | ">") {
            return Err(PaperBootstrapError::new(format!(
                "descriptor {descriptor} in {} has no scalar {key} at line {}",
                jar.display(),
                line_number + 1
            )));
        }
        let value = unquote(value);
        if fields.insert(key.to_owned(), value.to_owned()).is_some() {
            return Err(PaperBootstrapError::new(format!(
                "descriptor {descriptor} in {} repeats {key}",
                jar.display()
            )));
        }
    }
    Ok(fields)
}

fn unquote(value: &str) -> &str {
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        if matches!(bytes[0], b'\'' | b'"') && bytes[0] == bytes[value.len() - 1] {
            return &value[1..value.len() - 1];
        }
    }
    value
}

fn required_field(
    fields: &BTreeMap<String, String>,
    name: &str,
    descriptor: &str,
    jar: &Path,
) -> Result<String, PaperBootstrapError> {
    fields.get(name).cloned().ok_or_else(|| PaperBootstrapError::new(format!(
        "descriptor {descriptor} in {} is missing required {name}",
        jar.display()
    )))
}

fn validate_plugin_name(value: &str, descriptor: &str, jar: &Path) -> Result<(), PaperBootstrapError> {
    validate_scalar("name", value, descriptor, jar)?;
    if !value.chars().all(|character| character.is_ascii_alphanumeric() || matches!(character, ' ' | '_' | '-' | '.')) {
        return Err(PaperBootstrapError::new(format!(
            "descriptor {descriptor} in {} has invalid plugin name {value:?}",
            jar.display()
        )));
    }
    Ok(())
}

fn validate_scalar(
    field: &str,
    value: &str,
    descriptor: &str,
    jar: &Path,
) -> Result<(), PaperBootstrapError> {
    if value.trim().is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        return Err(PaperBootstrapError::new(format!(
            "descriptor {descriptor} in {} has invalid {field}",
            jar.display()
        )));
    }
    Ok(())
}

fn validate_class_name(value: &str, descriptor: &str, jar: &Path) -> Result<(), PaperBootstrapError> {
    let valid = value.split('.').all(|segment| {
        let mut chars = segment.chars();
        chars.next().is_some_and(|character| character.is_ascii_alphabetic() || matches!(character, '_' | '$'))
            && chars.all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '$'))
    });
    if valid {
        Ok(())
    } else {
        Err(PaperBootstrapError::new(format!(
            "descriptor {descriptor} in {} has invalid main class {value:?}",
            jar.display()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_TEST_DIRECTORY: AtomicUsize = AtomicUsize::new(0);

    #[test]
    fn discovery_sorts_jars_and_accepts_each_supported_descriptor() {
        let fixture = Fixture::new();
        fixture.paper_jar();
        fixture.plugin_jar("z-last.jar", BUKKIT_DESCRIPTOR, valid_descriptor("Zulu", "z.Main"));
        fixture.plugin_jar("a-first.jar", PAPER_DESCRIPTOR, valid_descriptor("Alpha", "a.Main"));
        let plan = PaperBootstrapConfig::new(fixture.paper_path(), fixture.plugins_path())
            .with_shim_path(fixture.root.join("shim"))
            .discover()
            .expect("discover plugins");
        assert_eq!(plan.plugins().iter().map(PaperPluginDescriptor::name).collect::<Vec<_>>(), ["Alpha", "Zulu"]);
        assert_eq!(plan.plugins()[0].kind(), PaperPluginDescriptorKind::Paper);
        assert_eq!(plan.plugins()[1].kind(), PaperPluginDescriptorKind::Bukkit);
    }

    #[test]
    fn discovery_rejects_duplicate_names_and_ambiguous_descriptors() {
        let fixture = Fixture::new();
        fixture.paper_jar();
        fixture.plugin_jar("first.jar", BUKKIT_DESCRIPTOR, valid_descriptor("Same", "a.Main"));
        fixture.plugin_jar("second.jar", PAPER_DESCRIPTOR, valid_descriptor("same", "b.Main"));
        let error = PaperBootstrapConfig::new(fixture.paper_path(), fixture.plugins_path())
            .discover()
            .expect_err("duplicate names must fail");
        assert!(error.to_string().contains("duplicate plugin name"));

        let fixture = Fixture::new();
        fixture.paper_jar();
        let descriptor = valid_descriptor("One", "a.Main");
        write_jar(
            &fixture.plugins_path().join("ambiguous.jar"),
            [
                (PAPER_DESCRIPTOR, descriptor.as_str()),
                (BUKKIT_DESCRIPTOR, descriptor.as_str()),
            ],
        );
        let error = PaperBootstrapConfig::new(fixture.paper_path(), fixture.plugins_path())
            .discover()
            .expect_err("two descriptors must fail");
        assert!(error.to_string().contains("both paper-plugin.yml and plugin.yml"));
    }

    #[test]
    fn discovery_rejects_invalid_or_missing_metadata_before_a_jvm_starts() {
        let fixture = Fixture::new();
        fixture.paper_jar();
        fixture.plugin_jar("bad.jar", BUKKIT_DESCRIPTOR, "name: Bad\nversion: one\nmain: not/a/class\n");
        let error = PaperBootstrapConfig::new(fixture.paper_path(), fixture.plugins_path())
            .discover()
            .expect_err("invalid main must fail");
        assert!(error.to_string().contains("invalid main class"));

        let fixture = Fixture::new();
        fixture.paper_jar();
        fixture.plugin_jar("missing.jar", BUKKIT_DESCRIPTOR, "name: Missing\nmain: a.Main\n");
        let error = PaperBootstrapConfig::new(fixture.paper_path(), fixture.plugins_path())
            .discover()
            .expect_err("missing version must fail");
        assert!(error.to_string().contains("missing required version"));
    }

    #[test]
    fn paper_discovery_requires_the_expected_class_and_manifest_marker() {
        let fixture = Fixture::new();
        write_jar(&fixture.paper_path(), [("META-INF/MANIFEST.MF", "Implementation-Title: Paper\n")]);
        let error = PaperBootstrapConfig::new(fixture.paper_path(), fixture.plugins_path())
            .discover()
            .expect_err("missing bootstrap class must fail");
        assert!(error.to_string().contains(BOOTSTRAP_ENTRY));
    }

    #[test]
    #[ignore = "requires LODESTONE_PAPER_JAR pointing to a locally materialized Paper server jar"]
    fn local_paper_jar_is_discovered_without_extracting_it() {
        let paper_jar = PathBuf::from(std::env::var_os("LODESTONE_PAPER_JAR")
            .expect("LODESTONE_PAPER_JAR is required"));
        let fixture = Fixture::new();
        let plan = PaperBootstrapConfig::new(paper_jar, fixture.plugins_path())
            .discover()
            .expect("discover local Paper jar");
        assert!(plan.plugins().is_empty());
    }

    fn valid_descriptor(name: &str, main: &str) -> String {
        format!("name: {name}\nversion: one\nmain: {main}\n")
    }

    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let number = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!("lodestone-paper-discovery-{}-{number}", std::process::id()));
            fs::create_dir_all(root.join("plugins")).expect("create fixture plugin directory");
            fs::create_dir(root.join("shim")).expect("create fixture shim directory");
            Self { root }
        }

        fn paper_path(&self) -> PathBuf {
            self.root.join("paper.jar")
        }

        fn plugins_path(&self) -> PathBuf {
            self.root.join("plugins")
        }

        fn paper_jar(&self) {
            write_jar(
                &self.paper_path(),
                [
                    (BOOTSTRAP_ENTRY, "class bytes are not read during discovery"),
                    ("META-INF/MANIFEST.MF", "Manifest-Version: 1.0\nImplementation-Title: Paper\n"),
                ],
            );
        }

        fn plugin_jar(&self, name: &str, descriptor: &str, contents: impl AsRef<str>) {
            write_jar(&self.plugins_path().join(name), [(descriptor, contents.as_ref())]);
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn write_jar<const N: usize>(path: &Path, entries: [(&str, &str); N]) {
        let file = File::create(path).expect("create fixture jar");
        let mut writer = zip::ZipWriter::new(file);
        for (name, contents) in entries {
            writer.start_file(name, zip::write::SimpleFileOptions::default()).expect("write fixture entry");
            writer.write_all(contents.as_bytes()).expect("write fixture contents");
        }
        writer.finish().expect("finish fixture jar");
    }
}
