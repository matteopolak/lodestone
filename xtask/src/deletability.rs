use super::*;

/// One dependency edge that points at the version family being deleted.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct DeletabilityEdge {
    /// The crate that depends on the version family.
    pub crate_name: String,
    /// The version package this edge points to.
    pub dependency_name: String,
    /// Which manifest table declared the dependency.
    pub dependency_table: &'static str,
    /// Target condition, when declared in a target-specific dependency table.
    pub dependency_target: Option<String>,
    /// Whether the dependency is optional (feature-gated).
    pub optional: bool,
    /// Whether the dependent is structurally a version crate.
    pub dependent_is_version_crate: bool,
}

impl DeletabilityEdge {
    fn describe(&self) -> String {
        let target = self.dependency_target.as_deref()
            .map(|target| format!(", target {target}"))
            .unwrap_or_default();
        format!("{} -> {} [{}{target}]", self.crate_name, self.dependency_name, self.dependency_table)
    }

    /// Required shared normal/build edges and undeclared version edges block
    /// removal. Valid compatibility edges are classified before this predicate.
    fn is_blocker(&self) -> bool {
        self.dependent_is_version_crate
            || (!self.optional && self.dependency_table != "dev-dependencies")
    }
}

/// A version family's folder included in a removal plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemovalFamily {
    pub crate_name: String,
    pub dir: String,
}

/// The structural plan for removing a family and its declared compatibility
/// dependents. Success means no blocking dependency remains after the listed
/// cleanup; it does not claim a deletion followed by a build was executed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeletabilityReport {
    /// The resolved package name, e.g. `lodestone-v1-8`.
    pub target_crate: String,
    /// The folder that would be deleted, relative to the workspace root.
    pub target_dir: String,
    /// Requested family plus the reverse transitive compatibility closure.
    pub removal_families: Vec<RemovalFamily>,
    /// Declared required base edges explaining why dependent families join it.
    pub compatibility_edges: Vec<DeletabilityEdge>,
    /// Required shared edges and undeclared version edges blocking the plan.
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
    /// Whether the removal plan has no structural blockers after cleanup.
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
            "removal plan for {} (folder {}):",
            self.target_crate, self.target_dir
        );
        let _ = write!(out, "\n  folders to remove ({}):", self.removal_families.len());
        for family in &self.removal_families {
            let _ = write!(out, "\n    - {} ({})", family.dir, family.crate_name);
        }
        if !self.compatibility_edges.is_empty() {
            let _ = write!(out, "\n  declared compatibility dependents included:");
            for edge in &self.compatibility_edges {
                let _ = write!(out, "\n    - {}", edge.describe());
            }
        }

        if self.blockers.is_empty() {
            let _ = write!(
                out,
                "\n  removal plan has no structural blockers after the listed cleanup ({} manifest line(s)); deletion and build were not executed",
                self.manifest_lines.len(),
            );
        } else {
            let _ = write!(
                out,
                "\n  removal plan blocked by {} dependency edge(s):",
                self.blockers.len()
            );
            for edge in &self.blockers {
                let why = if edge.dependent_is_version_crate {
                    "undeclared version dependency breaks isolation"
                } else {
                    "required (non-optional) dependency from a shared crate"
                };
                let _ = write!(
                    out,
                    "\n    - {}: {why}",
                    edge.describe(),
                );
            }
        }

        let _ = write!(
            out,
            "\n  manifest cleanup outside removed folders ({}):",
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
                    "\n    - {} (cleanup{optional})",
                    edge.describe(),
                );
            }
        }
        out
    }
}

/// Reports a family's structural removal plan and compatibility closure.
///
/// `requested` may be the package name (`lodestone-v1-8`), the folder name
/// (`v47`), or a path under `crates/versions/`. Dependency-graph edges catch
/// every crate that could reference the version in source (a crate can only
/// `use lodestone_v1_8` if it declares a dependency on it). Cargo *feature*
/// forwards such as `live-v47 = ["lodestone-registry/v47"]` are not edges but
/// are validated by Cargo at resolve time, so they are caught separately by
/// scanning manifests for the family's feature token, package, dependency
/// aliases and directory. This reads metadata and files without deleting them
/// or running a build.
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
    let mut version_dirs = BTreeMap::new();
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
            version_dirs.insert(package_name.to_owned(), dir.clone());
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
    let mut compatibility_edges = Vec::new();
    let compatibility_bases =
        super::isolation::validated_compatibility_bases(&member_packages, &version_crate_names)?;
    let mut removal_names = BTreeSet::from([target_crate.clone()]);
    loop {
        let before = removal_names.len();
        for (dependent, base) in &compatibility_bases {
            if removal_names.contains(&base.crate_name) {
                removal_names.insert(dependent.clone());
            }
        }
        if before == removal_names.len() {
            break;
        }
    }
    let removal_families = removal_names.iter().map(|name| RemovalFamily {
        crate_name: name.clone(),
        dir: version_dirs[name].clone(),
    }).collect::<Vec<_>>();
    for package in &member_packages {
        let crate_name = package
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("workspace package is missing a name"))?;
        let Some(dependencies) = package.get("dependencies").and_then(Value::as_array) else {
            continue;
        };
        for dependency in dependencies {
            let Some(dependency_name) = dependency.get("name").and_then(Value::as_str) else {
                continue;
            };
            if !removal_names.contains(dependency_name) {
                continue;
            }
            let edge = DeletabilityEdge {
                crate_name: crate_name.to_owned(),
                dependency_name: dependency_name.to_owned(),
                dependency_table: dependency_table_name(dependency.get("kind")),
                dependency_target: dependency.get("target").and_then(Value::as_str).map(str::to_owned),
                optional: dependency
                    .get("optional")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                dependent_is_version_crate: version_crate_names.contains(crate_name),
            };
            if removal_names.contains(crate_name)
                && compatibility_bases.get(crate_name).is_some_and(|base| base.matches(dependency))
            {
                compatibility_edges.push(edge);
            } else if edge.is_blocker() {
                blockers.push(edge);
            } else {
                manual_edits.push(edge);
            }
        }
    }

    let mut manifest_lines = Vec::new();
    let mut registry_source_lines = Vec::new();
    for family in &removal_families {
        manifest_lines.extend(manifest_lines_mentioning(
            workspace_root, &member_packages, &family.crate_name, &family.dir,
        )?);
        registry_source_lines.extend(registry_source_lines_mentioning(
            &canonical_root, &member_packages, &family.crate_name, &family.dir,
        )?);
    }
    for lines in [&mut manifest_lines, &mut registry_source_lines] {
        lines.retain(|line| !removal_families.iter()
            .any(|family| Path::new(&line.path).starts_with(&family.dir)));
        lines.sort();
        lines.dedup();
    }
    for edges in [&mut blockers, &mut manual_edits, &mut compatibility_edges] {
        edges.sort();
        edges.dedup();
    }

    Ok(DeletabilityReport {
        target_crate,
        target_dir,
        removal_families,
        compatibility_edges,
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
    let cfg_needle = format!("feature = \"{feature_token}\"");

    let mut lines = Vec::new();
    for package in member_packages {
        if !package_is_version_registry(package) {
            continue;
        }
        let snake_names = dependency_names_for(package, target_crate).into_iter()
            .map(|name| name.replace('-', "_")).collect::<BTreeSet<_>>();
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
                if line.contains(&cfg_needle)
                    || snake_names.iter().any(|name| line_contains_cargo_token(line, name))
                {
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
    let mut dependency_names = BTreeMap::new();
    let mut workspace_dependency_names = BTreeSet::from([target_crate.to_owned()]);
    for package in member_packages {
        if let Some(manifest_path) = package.get("manifest_path").and_then(Value::as_str) {
            manifests.insert(PathBuf::from(manifest_path));
            let names = dependency_names_for(package, target_crate);
            workspace_dependency_names.extend(names.iter().cloned());
            dependency_names.insert(PathBuf::from(manifest_path), names);
        }
    }
    dependency_names.insert(workspace_root.join("Cargo.toml"), workspace_dependency_names);

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
        if Path::new(&display_path).starts_with(target_dir) {
            continue;
        }
        for (index, line) in contents.lines().enumerate() {
            if line_contains_cargo_token(line, target_crate)
                || line_contains_cargo_token(line, target_dir)
                || dependency_names.get(&manifest).is_some_and(|names| names.iter()
                    .any(|name| line_contains_cargo_token(line, name)))
                || line_forwards_to_family_feature(line, folder_token)
            {
                lines.push(ManifestLine {
                    path: display_path.clone(),
                    line: index + 1,
                    text: line.trim().to_owned(),
                });
            }
        }
    }
    lines.sort();
    lines.dedup();
    Ok(lines)
}

fn dependency_names_for(package: &Value, target_crate: &str) -> BTreeSet<String> {
    package.get("dependencies").and_then(Value::as_array).into_iter().flatten()
        .filter(|dependency| dependency.get("name").and_then(Value::as_str) == Some(target_crate))
        .map(|dependency| dependency.get("rename").and_then(Value::as_str)
            .unwrap_or(target_crate).to_owned())
        .collect()
}

fn line_contains_cargo_token(line: &str, token: &str) -> bool {
    let is_name_char = |ch: char| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.');
    line.match_indices(token).any(|(start, _)| {
        line[..start].chars().next_back().is_none_or(|ch| !is_name_char(ch))
            && line[start + token.len()..].chars().next().is_none_or(|ch| !is_name_char(ch))
    })
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
