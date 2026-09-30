use super::*;

pub(crate) struct CompatibilityBase {
    pub(crate) crate_name: String,
    dir: PathBuf,
}

impl CompatibilityBase {
    pub(crate) fn matches(&self, dependency: &Value) -> bool {
        dependency.get("name").and_then(Value::as_str) == Some(self.crate_name.as_str())
            && dependency_table_name(dependency.get("kind")) == "dependencies"
            && !dependency.get("optional").and_then(Value::as_bool).unwrap_or(false)
            && dependency.get("path").and_then(Value::as_str)
                .and_then(|path| Path::new(path).canonicalize().ok())
                .is_some_and(|path| path == self.dir)
    }
}

pub(crate) fn validated_compatibility_bases(
    member_packages: &[&Value],
    version_crate_names: &BTreeSet<String>,
) -> Result<BTreeMap<String, CompatibilityBase>> {
    let mut bases = BTreeMap::new();
    for package in member_packages {
        let Some(declaration) = package
            .get("metadata")
            .and_then(|metadata| metadata.get("lodestone-isolation"))
            .and_then(|metadata| metadata.get("compatibility-base"))
        else {
            continue;
        };
        let crate_name = package
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("workspace package is missing a name"))?;
        let invalid = || anyhow!(
            "{crate_name}: compatibility-base must name another workspace version family with an exact required normal path dependency"
        );
        let base = declaration.as_str().ok_or_else(invalid)?;
        if !version_crate_names.contains(crate_name)
            || !version_crate_names.contains(base)
            || crate_name == base
        {
            return Err(invalid());
        }
        let base_package = member_packages
            .iter()
            .find(|candidate| candidate.get("name").and_then(Value::as_str) == Some(base))
            .ok_or_else(invalid)?;
        let base = CompatibilityBase {
            crate_name: base.to_owned(),
            dir: base_package
            .get("manifest_path")
            .and_then(Value::as_str)
            .and_then(|manifest| Path::new(manifest).parent())
            .ok_or_else(invalid)?
            .canonicalize()?,
        };
        let valid_edge = package
            .get("dependencies")
            .and_then(Value::as_array)
            .is_some_and(|dependencies| dependencies.iter().any(|dependency| base.matches(dependency)));
        if !valid_edge {
            return Err(invalid());
        }
        bases.insert(crate_name.to_owned(), base);
    }
    Ok(bases)
}

/// The result of an isolation check.
///
/// The lint exists to protect one concrete user requirement: **dropping support
/// for a version must mean deleting its `crates/versions/<version>` folder
/// and declared compatibility dependents, with only surfaced cleanup.** Two
/// dependency shapes break that promise, and this report is expressed directly
/// in those terms rather than in terms of
/// an allowlist of "blessed" shared crates (which rots every time a new
/// version-free crate such as `lodestone-world` is added):
///
/// 1. A version crate depending on **another version crate** — deleting the
///    dependency would break the dependent. Fatal unless the dependent declares
///    that exact crate as its compatibility base.
/// 2. A **shared (non-version) crate** depending on a version crate — deleting
///    the version folder would stop the shared crate from building. Fatal when
///    the dependency is required, but only a surfaced [`Severity::Warning`] when
///    it is optional or dev-only, because such a coupling still lets the version
///    be removed by deleting the folder plus one feature-gated line.
///
/// There is exactly one intended exception to rule 2: the **version registry**,
/// the single shared crate whose entire job is to map a protocol number to a
/// concrete adapter. It opts in via
/// `[package.metadata.lodestone-isolation] role = "version-registry"`, and its
/// *optional*, feature-gated edges to version crates are reported as
/// informational aggregation ([`Severity::Info`]) rather than warnings. This
/// exemption is safe by construction: it only ever reclassifies an edge that was
/// already non-fatal (an optional shared -> version warning). A *required*
/// registry -> version edge, and any undeclared version -> version edge, remain
/// fatal, so the role can never be abused to silence a build-breaking violation.
///
/// Whether a crate *is* a version crate is derived structurally from its
/// location under `crates/versions/`, so a brand-new version family is covered
/// automatically without editing this lint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IsolationReport {
    pub findings: Vec<IsolationFinding>,
}

impl IsolationReport {
    /// Fatal findings that must fail the check.
    pub fn violations(&self) -> impl Iterator<Item = &IsolationFinding> {
        self.findings
            .iter()
            .filter(|finding| finding.severity == Severity::Violation)
    }

    /// Non-fatal findings that are surfaced but do not fail the check.
    pub fn warnings(&self) -> impl Iterator<Item = &IsolationFinding> {
        self.findings
            .iter()
            .filter(|finding| finding.severity == Severity::Warning)
    }

    /// Informational findings: expected, by-design couplings (currently the
    /// version registry's optional edges to the version crates it aggregates).
    /// Surfaced for transparency but never fatal.
    pub fn infos(&self) -> impl Iterator<Item = &IsolationFinding> {
        self.findings
            .iter()
            .filter(|finding| finding.severity == Severity::Info)
    }

    /// Whether the check should fail.
    #[must_use]
    pub fn has_violations(&self) -> bool {
        self.violations().next().is_some()
    }

    /// A human-readable summary of the fatal violations, explaining *which*
    /// invariant each one breaks and *why* it matters, so someone who hits this
    /// understands the intent instead of reaching for an allowlist.
    #[must_use]
    pub fn violation_summary(&self) -> String {
        let mut summary = String::from(
            "protocol version crate isolation violations found (version removal must stay confined to declared compatibility families):",
        );
        for finding in self.violations() {
            let _ = write!(summary, "\n- {}", finding.describe());
        }
        summary
    }

    /// A human-readable summary of the surfaced warnings, or `None` when there
    /// are none. Warnings are real warts we intend to remove (for example the
    /// feature-gated live-test dependency from `lodestone-client` onto a version
    /// crate, pending a version-selecting registry crate), so they are always
    /// surfaced rather than silently ignored.
    #[must_use]
    pub fn warning_summary(&self) -> Option<String> {
        let mut warnings = self.warnings().peekable();
        warnings.peek()?;
        let mut summary =
            String::from("protocol version crate isolation warnings (surfaced, non-fatal):");
        for finding in warnings {
            let _ = write!(summary, "\n- {}", finding.describe());
        }
        Some(summary)
    }

    /// A human-readable summary of the informational, by-design couplings, or
    /// `None` when there are none. These are the version registry's optional,
    /// feature-gated edges to the version families it aggregates — the one place
    /// a shared crate is *meant* to name versions. They are surfaced so the
    /// aggregation is visible, never hidden.
    #[must_use]
    pub fn info_summary(&self) -> Option<String> {
        let mut infos = self.infos().peekable();
        infos.peek()?;
        let mut summary =
            String::from("protocol version registry aggregation (by design, non-fatal):");
        for finding in infos {
            let _ = write!(summary, "\n- {}", finding.describe());
        }
        Some(summary)
    }
}

/// A single dependency edge that the isolation lint has something to say about.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IsolationFinding {
    /// The crate that declares the offending dependency.
    pub crate_name: String,
    /// The dependency being pointed at.
    pub dependency_name: String,
    /// Which manifest table the dependency was declared in.
    pub dependency_table: &'static str,
    /// Whether the dependency is optional (feature-gated).
    pub optional: bool,
    /// Which invariant the edge relates to.
    pub rule: IsolationRule,
    /// Whether the edge fails the check or is merely surfaced.
    pub severity: Severity,
    /// Extra evidence for the finding, when a static rule explanation is not
    /// specific enough.
    pub detail: Option<String>,
}

impl IsolationFinding {
    fn describe(&self) -> String {
        let optional = if self.optional { ", optional" } else { "" };
        let mut description = format!(
            "{} -> {} (in [{}]{optional}): {}",
            self.crate_name,
            self.dependency_name,
            self.dependency_table,
            self.rule.explanation(),
        );
        if let Some(detail) = &self.detail {
            let _ = write!(description, " ({detail})");
        }
        description
    }
}

/// Whether a finding fails the check or is only reported.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Severity {
    /// Fails the check.
    Violation,
    /// Surfaced but does not fail the check.
    Warning,
    /// An expected, by-design coupling (the version registry's optional edges to
    /// the versions it aggregates). Surfaced for transparency, never fatal.
    Info,
}

/// The deletability invariant a finding relates to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IsolationRule {
    /// A version crate depends on another version crate without declaring it as
    /// its required compatibility base.
    VersionDependsOnVersion,
    /// A shared (non-version) crate depends on a version crate.
    SharedDependsOnVersion,
    /// The designated version registry depends on a version crate through an
    /// optional, feature-gated edge — the intended aggregation point.
    RegistryAggregatesVersion,
    /// The version registry is trying to aggregate a family whose generated
    /// shape-review checklist still has unreviewed entries.
    RegistryAggregatesUnreviewedVersion,
}

impl IsolationRule {
    fn explanation(self) -> &'static str {
        match self {
            IsolationRule::VersionDependsOnVersion => {
                "a version crate may depend on another only through its declared required compatibility base; other edges break isolation"
            }
            IsolationRule::SharedDependsOnVersion => {
                "a shared crate must not depend on a version crate, or deleting that version's folder would stop the shared crate from building"
            }
            IsolationRule::RegistryAggregatesVersion => {
                "the version registry aggregates this version through an optional, feature-gated edge; deleting the version stays a matter of removing its folder plus that one feature line"
            }
            IsolationRule::RegistryAggregatesUnreviewedVersion => {
                "the version registry must not advertise a family while SHAPE_REVIEW.toml still has unreviewed packet shape deltas"
            }
        }
    }
}

pub fn check_workspace_isolation(workspace_root: &Path) -> Result<IsolationReport> {
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

    let mut workspace_member_names = BTreeSet::new();
    let mut version_crate_names = BTreeSet::new();
    let mut version_crate_shape_review_violations = BTreeMap::new();
    let mut registry_crate_names = BTreeSet::new();
    let mut member_packages = Vec::new();
    let canonical_root = workspace_root.canonicalize().with_context(|| {
        format!(
            "canonicalize workspace root for isolation check: {}",
            workspace_root.display()
        )
    })?;

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
        workspace_member_names.insert(package_name.to_owned());

        // "Is a version crate" is derived structurally from the crate's location
        // under crates/versions/, never by name, so a new version family is
        // covered automatically without editing this lint.
        if package_manifest_is_under_protocol(&canonical_root, package)? {
            version_crate_names.insert(package_name.to_owned());
            let manifest_path = package
                .get("manifest_path")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("workspace package is missing manifest_path"))?;
            let manifest_dir = Path::new(manifest_path)
                .parent()
                .ok_or_else(|| anyhow!("{manifest_path} has no parent directory"))?;
            let review_path = manifest_dir.join("SHAPE_REVIEW.toml");
            if review_path.exists() {
                let family = manifest_dir
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or(package_name);
                let violations = shape_review_violations(family, &review_path)?;
                if !violations.is_empty() {
                    version_crate_shape_review_violations
                        .insert(package_name.to_owned(), violations.join("; "));
                }
            }
        }
        // The version registry opts in structurally via a metadata role, not by
        // name. This exemption is deliberately narrow (see the finding loop): it
        // can only reclassify an *optional* shared -> version edge (already a
        // non-fatal warning) as an informational, by-design aggregation. It has
        // no power over any fatal rule, so it cannot be abused to silence a real
        // violation.
        if package_is_version_registry(package) {
            registry_crate_names.insert(package_name.to_owned());
        }
        member_packages.push(package);
    }

    let compatibility_bases =
        validated_compatibility_bases(&member_packages, &version_crate_names)?;
    let mut findings = Vec::new();
    for package in member_packages {
        let crate_name = package
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("workspace package is missing a name"))?;
        let crate_is_version = version_crate_names.contains(crate_name);
        let crate_is_registry = registry_crate_names.contains(crate_name);
        let Some(dependencies) = package.get("dependencies").and_then(Value::as_array) else {
            continue;
        };

        for dependency in dependencies {
            let dependency_name = dependency
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("{crate_name} dependency is missing a name"))?;

            // Only workspace-internal edges can violate isolation; third-party
            // crates are never version crates.
            if !workspace_member_names.contains(dependency_name) {
                continue;
            }
            let dependency_is_version = version_crate_names.contains(dependency_name);
            if !dependency_is_version || dependency_name == crate_name {
                continue;
            }

            let dependency_table = dependency_table_name(dependency.get("kind"));
            let optional = dependency
                .get("optional")
                .and_then(Value::as_bool)
                .unwrap_or(false);

            if crate_is_version {
                let is_declared_base = compatibility_bases.get(crate_name)
                    .is_some_and(|base| base.matches(dependency));
                if is_declared_base {
                    continue;
                }

                // A version-to-version edge is permitted only when its
                // dependent declares this exact required edge as its base.
                findings.push(IsolationFinding {
                    crate_name: crate_name.to_owned(),
                    dependency_name: dependency_name.to_owned(),
                    dependency_table,
                    optional,
                    rule: IsolationRule::VersionDependsOnVersion,
                    severity: Severity::Violation,
                    detail: None,
                });
            } else {
                // Rule 2: shared -> version. A *required* edge makes the version
                // undeletable (fatal); an optional or dev-only edge is a
                // surfaced wart (warning) because the version can still be
                // dropped by deleting its folder plus one feature-gated line.
                let is_soft = optional || dependency_table == "dev-dependencies";
                let detail = version_crate_shape_review_violations
                    .get(dependency_name)
                    .cloned();
                let (rule, severity) = if crate_is_registry && is_soft && detail.is_some() {
                    (
                        IsolationRule::RegistryAggregatesUnreviewedVersion,
                        Severity::Violation,
                    )
                } else if crate_is_registry && is_soft {
                    // The designated registry is the ONE shared crate allowed to
                    // name versions, and only through optional/feature-gated
                    // edges. Downgrade this from a warning to an informational,
                    // by-design aggregation. Crucially this branch requires
                    // `is_soft`, so a *required* registry -> version edge falls
                    // through to the fatal arm below: the exemption can never
                    // hide a build-breaking coupling.
                    (IsolationRule::RegistryAggregatesVersion, Severity::Info)
                } else if is_soft {
                    (IsolationRule::SharedDependsOnVersion, Severity::Warning)
                } else {
                    (IsolationRule::SharedDependsOnVersion, Severity::Violation)
                };
                findings.push(IsolationFinding {
                    crate_name: crate_name.to_owned(),
                    dependency_name: dependency_name.to_owned(),
                    dependency_table,
                    optional,
                    rule,
                    severity,
                    detail,
                });
            }
        }
    }

    Ok(IsolationReport { findings })
}
