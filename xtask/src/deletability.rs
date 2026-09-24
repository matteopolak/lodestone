use super::*;

/// One dependency edge that points at the version family being deleted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeletabilityEdge {
    /// The crate that depends on the version family.
    pub crate_name: String,
    /// Which manifest table declared the dependency.
    pub dependency_table: &'static str,
    /// Whether the dependency is optional (feature-gated).
    pub optional: bool,
    /// Whether the dependent is itself a version crate (a hard isolation break).
    pub dependent_is_version_crate: bool,
}

impl DeletabilityEdge {
    /// A required, non-optional dependency from a shared crate — or *any*
    /// dependency from another version crate — makes the folder impossible to
    /// delete without editing code that must keep compiling. Everything else is
    /// a one-line manifest edit.
    fn is_blocker(&self) -> bool {
        self.dependent_is_version_crate
            || (!self.optional && self.dependency_table == "dependencies")
    }
}

/// The result of simulating the deletion of one version family's folder.
///
/// The user requirement this proves is concrete: **dropping support for a
/// version must mean deleting a single `crates/versions/<version>` folder and
/// having it be mostly all gone.** This report is the continuously-checkable
/// form of the manual deletion drill — it enumerates every crate that depends on
/// the target and classifies each edge as either a *blocker* (something that
/// would fail to compile and therefore breaks the "just delete the folder"
/// promise) or a *manual edit* (a one-line, feature-gated reference that is
/// expected to be removed alongside the folder).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeletabilityReport {
    /// The resolved package name, e.g. `lodestone-v1-8`.
    pub target_crate: String,
    /// The folder that would be deleted, relative to the workspace root.
    pub target_dir: String,
    /// Edges that would break compilation if the folder were simply deleted.
    pub blockers: Vec<DeletabilityEdge>,
    /// Feature-gated / optional / dev edges that need a one-line manifest edit.
    pub manual_edits: Vec<DeletabilityEdge>,
    /// Concrete manifest lines that mention the target crate, as actionable
    /// `path:line` edits.
    pub manifest_lines: Vec<ManifestLine>,
    /// Source lines in the designated version registry that reference the family
    /// through a feature cfg or its crate path. These stay behind `#[cfg]` so
    /// they never break the build, but a dead `#[cfg(feature = "v47")]` emits an
    /// `unexpected_cfgs` warning once the feature is gone, and the workspace
    /// standard is zero warnings — so they are surfaced as required edits too.
    pub registry_source_lines: Vec<ManifestLine>,
}

/// A concrete manifest line that references the version family being deleted.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct ManifestLine {
    /// Manifest path relative to the workspace root.
    pub path: String,
    /// 1-based line number.
    pub line: usize,
    /// The trimmed line text.
    pub text: String,
}

impl DeletabilityReport {
    /// Whether the folder can be dropped without breaking any crate's build.
    #[must_use]
    pub fn is_cleanly_deletable(&self) -> bool {
        self.blockers.is_empty()
    }

    /// A human-readable blast-radius report.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        let _ = write!(
            out,
            "deletion drill for {} (folder {}):",
            self.target_crate, self.target_dir
        );

        if self.blockers.is_empty() {
            let _ = write!(
                out,
                "\n  cleanly deletable: removing the folder plus the {} manifest line(s) below leaves every crate building (no code changes, nothing structurally undeletable)",
                self.manifest_lines.len()
            );
        } else {
            let _ = write!(
                out,
                "\n  NOT cleanly deletable: {} crate(s) would fail to build:",
                self.blockers.len()
            );
            for edge in &self.blockers {
                let why = if edge.dependent_is_version_crate {
                    "another version crate depends on it (isolation break)"
                } else {
                    "required (non-optional) dependency from a shared crate"
                };
                let _ = write!(
                    out,
                    "\n    - {} [{}]: {why}",
                    edge.crate_name, edge.dependency_table
                );
            }
        }

        let _ = write!(
            out,
            "\n  manifest edits to make when deleting the folder ({}):",
            self.manifest_lines.len()
        );
        for line in &self.manifest_lines {
            let _ = write!(out, "\n    - {}:{}  {}", line.path, line.line, line.text);
        }
        if !self.registry_source_lines.is_empty() {
            let _ = write!(
                out,
                "\n  registry source edits for a warning-clean deletion ({}):",
                self.registry_source_lines.len()
            );
            for line in &self.registry_source_lines {
                let _ = write!(out, "\n    - {}:{}  {}", line.path, line.line, line.text);
            }
        }
        if !self.manual_edits.is_empty() {
            let _ = write!(out, "\n  affected crates:");
            for edge in &self.manual_edits {
                let optional = if edge.optional { ", optional" } else { "" };
                let _ = write!(
                    out,
                    "\n    - {} (feature-gated reference in [{}]{optional})",
                    edge.crate_name, edge.dependency_table
                );
            }
        }
        out
    }
}

/// Simulates deleting a version family's folder and reports the fallout.
///
/// `requested` may be the package name (`lodestone-v1-8`), the folder name
/// (`v47`), or a path under `crates/versions/`. Dependency-graph edges catch
/// every crate that could reference the version in source (a crate can only
/// `use lodestone_v1_8` if it declares a dependency on it). Cargo *feature*
/// forwards such as `live-v47 = ["lodestone-registry/v47"]` are not edges but
/// are validated by Cargo at resolve time, so they are caught separately by
/// scanning manifests for the family's folder token; together the two cover
/// every way deleting the folder can break a build.
pub fn check_workspace_deletable(
    workspace_root: &Path,
    requested: &str,
) -> Result<DeletabilityReport> {
    let metadata = cargo_metadata(workspace_root)?;
    let workspace_members = metadata
        .get("workspace_members")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("cargo metadata did not include workspace_members"))?
        .iter()
        .filter_map(Value::as_str)
        .collect::<BTreeSet<_>>();
    let packages = metadata
        .get("packages")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("cargo metadata did not include packages"))?;
    let canonical_root = workspace_root.canonicalize().with_context(|| {
        format!(
            "canonicalize workspace root for deletability check: {}",
            workspace_root.display()
        )
    })?;

    let mut version_crate_names = BTreeSet::new();
    let mut member_packages = Vec::new();
    let mut target: Option<(String, String)> = None;

    for package in packages {
        let Some(package_id) = package.get("id").and_then(Value::as_str) else {
            continue;
        };
        if !workspace_members.contains(package_id) {
            continue;
        }
        let package_name = package
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("workspace package is missing a name"))?;

        if package_manifest_is_under_protocol(&canonical_root, package)? {
            version_crate_names.insert(package_name.to_owned());
            let manifest_path = package
                .get("manifest_path")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("version package is missing manifest_path"))?;
            let dir = version_crate_dir(&canonical_root, Path::new(manifest_path))?;
            let folder = Path::new(&dir)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            if requested == package_name
                || requested == folder
                || requested.trim_end_matches('/') == dir
                || family_dir_name(requested) == folder
                || format!("lodestone-{requested}") == package_name
            {
                target = Some((package_name.to_owned(), dir));
            }
        }
        member_packages.push(package);
    }

    let (target_crate, target_dir) = target.ok_or_else(|| {
        anyhow!(
            "no version crate matched {requested:?}; expected a package name (lodestone-v1-8), folder (1.8), or path under crates/versions/"
        )
    })?;

    let mut blockers = Vec::new();
    let mut manual_edits = Vec::new();
    for package in &member_packages {
        let crate_name = package
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("workspace package is missing a name"))?;
        if crate_name == target_crate {
            continue;
        }
        let Some(dependencies) = package.get("dependencies").and_then(Value::as_array) else {
            continue;
        };
        for dependency in dependencies {
            let dependency_name = dependency.get("name").and_then(Value::as_str);
            if dependency_name != Some(target_crate.as_str()) {
                continue;
            }
            let edge = DeletabilityEdge {
                crate_name: crate_name.to_owned(),
                dependency_table: dependency_table_name(dependency.get("kind")),
                optional: dependency
                    .get("optional")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                dependent_is_version_crate: version_crate_names.contains(crate_name),
            };
            if edge.is_blocker() {
                blockers.push(edge);
            } else {
                manual_edits.push(edge);
            }
        }
    }

    let manifest_lines =
        manifest_lines_mentioning(workspace_root, &member_packages, &target_crate, &target_dir)?;
    let registry_source_lines = registry_source_lines_mentioning(
        &canonical_root,
        &member_packages,
        &target_crate,
        &target_dir,
    )?;

    Ok(DeletabilityReport {
        target_crate,
        target_dir,
        blockers,
        manual_edits,
        manifest_lines,
        registry_source_lines,
    })
}

/// The Cargo feature name a version family is gated behind, e.g. `v1-8` for
/// package `lodestone-v1-8`. Derived from the package name rather than the
/// directory: the four families renamed to era-start Minecraft-version
/// directories (`1.8`, `1.9`, `1.14`, `26.2`) decoupled "where the crate
/// lives" from "what its package/feature suffix is", so the folder name is no
/// longer a safe stand-in for the feature token (and for the renamed
/// families, would not even match it).
fn feature_token_for(target_crate: &str) -> &str {
    target_crate.strip_prefix("lodestone-").unwrap_or(target_crate)
}

/// Scans the designated version registry's source tree for lines that gate on
/// the family being deleted (a `#[cfg(feature = "v1-8")]` entry or a
/// `lodestone_v1_8::` path). These stay behind `#[cfg]` so they never break the
/// build, but the dead cfg emits an `unexpected_cfgs` warning once the feature
/// is gone. The registry is identified structurally by its metadata role, never
/// by name, so this cannot be pointed at an arbitrary crate.
fn registry_source_lines_mentioning(
    canonical_root: &Path,
    member_packages: &[&Value],
    target_crate: &str,
    _target_dir: &str,
) -> Result<Vec<ManifestLine>> {
    let feature_token = feature_token_for(target_crate);
    let snake_name = target_crate.replace('-', "_");
    let cfg_needle = format!("feature = \"{feature_token}\"");

    let mut lines = Vec::new();
    for package in member_packages {
        if !package_is_version_registry(package) {
            continue;
        }
        let Some(manifest_path) = package.get("manifest_path").and_then(Value::as_str) else {
            continue;
        };
        let src_dir = Path::new(manifest_path)
            .parent()
            .map(|parent| parent.join("src"))
            .unwrap_or_default();
        for file in rust_sources_under(&src_dir) {
            let Ok(contents) = std::fs::read_to_string(&file) else {
                continue;
            };
            let display_path = file
                .canonicalize()
                .ok()
                .and_then(|canonical| {
                    canonical
                        .strip_prefix(canonical_root)
                        .ok()
                        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
                })
                .unwrap_or_else(|| file.to_string_lossy().into_owned());
            for (index, line) in contents.lines().enumerate() {
                if line.contains(&cfg_needle) || line.contains(&snake_name) {
                    lines.push(ManifestLine {
                        path: display_path.clone(),
                        line: index + 1,
                        text: line.trim().to_owned(),
                    });
                }
            }
        }
    }
    lines.sort();
    Ok(lines)
}

/// Recursively collects `*.rs` files under `dir`.
fn rust_sources_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// Returns the version crate's folder path relative to the workspace root
/// (for example `crates/versions/1.8`).
fn version_crate_dir(canonical_root: &Path, manifest_path: &Path) -> Result<String> {
    let canonical_manifest = manifest_path
        .canonicalize()
        .with_context(|| format!("canonicalize manifest path {}", manifest_path.display()))?;
    let relative = canonical_manifest
        .strip_prefix(canonical_root)
        .with_context(|| {
            format!(
                "manifest {} is not under workspace root",
                manifest_path.display()
            )
        })?;
    let dir = relative.parent().unwrap_or(Path::new(""));
    Ok(dir.to_string_lossy().replace('\\', "/"))
}

/// Scans the workspace root manifest plus every member manifest for lines that
/// literally mention the target crate, producing actionable `path:line` edits.
fn manifest_lines_mentioning(
    workspace_root: &Path,
    member_packages: &[&Value],
    target_crate: &str,
    target_dir: &str,
) -> Result<Vec<ManifestLine>> {
    let mut manifests = BTreeSet::new();
    manifests.insert(workspace_root.join("Cargo.toml"));
    for package in member_packages {
        if let Some(manifest_path) = package.get("manifest_path").and_then(Value::as_str) {
            manifests.insert(PathBuf::from(manifest_path));
        }
    }

    let canonical_root = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_owned());
    // The package name's suffix (e.g. `v1-8`, from `lodestone-v1-8`) is how
    // *feature* references name the family, as in
    // `live-v1-8 = ["lodestone-registry/v1-8"]`. Cargo validates these feature
    // strings at resolve time, so a dangling one breaks even the default build
    // — yet it is invisible to the dependency graph. We therefore scan
    // manifests for both the package name and this token. This is no longer
    // the folder's own name: the four families renamed to era-start Minecraft
    // version directories decoupled "where the crate lives" from "what its
    // package/feature suffix is".
    let folder_token = feature_token_for(target_crate);
    let mut lines = Vec::new();
    for manifest in manifests {
        let Ok(contents) = std::fs::read_to_string(&manifest) else {
            continue;
        };
        let display_path = manifest
            .canonicalize()
            .ok()
            .and_then(|canonical| {
                canonical
                    .strip_prefix(&canonical_root)
                    .ok()
                    .map(|relative| relative.to_string_lossy().replace('\\', "/"))
            })
            .unwrap_or_else(|| manifest.to_string_lossy().into_owned());
        // The target's own manifest is deleted with the folder, so its
        // references are not edits anyone has to make.
        if display_path.starts_with(target_dir) {
            continue;
        }
        for (index, line) in contents.lines().enumerate() {
            if line.contains(target_crate) || line_forwards_to_family_feature(line, folder_token) {
                lines.push(ManifestLine {
                    path: display_path.clone(),
                    line: index + 1,
                    text: line.trim().to_owned(),
                });
            }
        }
    }
    lines.sort();
    Ok(lines)
}

/// Whether a manifest line forwards a Cargo *feature* to the family being
/// deleted, e.g. `live-v1-8 = ["lodestone-registry/v1-8"]`. Such references are
/// validated by Cargo at resolve time (a dangling one fails the whole build) but
/// are not dependency-graph edges, so they must be caught textually. Matches the
/// feature token only as a `/<token>` path segment ending at a feature-string
/// boundary, so `v1-8` never matches inside a longer token such as `v1-80`.
pub(crate) fn line_forwards_to_family_feature(line: &str, folder_token: &str) -> bool {
    if folder_token.is_empty() {
        return false;
    }
    let needle = format!("/{folder_token}");
    let mut search_from = 0;
    while let Some(offset) = line[search_from..].find(&needle) {
        let end = search_from + offset + needle.len();
        let boundary = line[end..].chars().next().is_none_or(|next| {
            !next.is_ascii_alphanumeric() && next != '-' && next != '_' && next != '.'
        });
        if boundary {
            return true;
        }
        search_from = end;
    }
    false
}
