use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConnectedReport {
    pub roots: Vec<String>,
    pub findings: Vec<ConnectedFinding>,
    pub allowed: Vec<ConnectedAllowance>,
}

impl ConnectedReport {
    pub fn violations(&self) -> impl Iterator<Item = &ConnectedFinding> {
        self.findings.iter()
    }

    #[must_use]
    pub fn has_violations(&self) -> bool {
        !self.findings.is_empty()
    }

    #[must_use]
    pub fn violation_summary(&self) -> String {
        let mut summary = String::from(
            "workspace connectivity violations found (crate is not reachable from any shipped binary/cdylib root through non-dev dependencies):",
        );
        for finding in &self.findings {
            let _ = write!(summary, "\n- {}", finding.describe());
        }
        summary
    }

    #[must_use]
    pub fn success_summary(&self) -> String {
        format!(
            "workspace connectivity check passed ({} shipped root(s), {} explicit exception(s))",
            self.roots.len(),
            self.allowed.len()
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConnectedFinding {
    pub crate_name: String,
    pub reason: ConnectedReason,
}

impl ConnectedFinding {
    fn describe(&self) -> String {
        match &self.reason {
            ConnectedReason::NoWorkspaceDependents => format!(
                "{} is unreachable; it has no workspace dependents outside dev-dependencies",
                self.crate_name
            ),
            ConnectedReason::OnlyDevDependents(dependents) => format!(
                "{} is unreachable; it is only used by dev-dependencies from {}",
                self.crate_name,
                format_crate_list(dependents)
            ),
            ConnectedReason::OnlyUnreachableDependents(dependents) => {
                let dependent_word = if dependents.len() == 1 {
                    "dependent"
                } else {
                    "dependents"
                };
                let verb = if dependents.len() == 1 { "is" } else { "are" };
                format!(
                    "{} is unreachable; its non-dev workspace {dependent_word} {} {verb} also unreachable",
                    self.crate_name,
                    format_crate_list(dependents)
                )
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConnectedReason {
    NoWorkspaceDependents,
    OnlyDevDependents(Vec<String>),
    OnlyUnreachableDependents(Vec<String>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConnectedAllowance {
    pub crate_name: String,
    pub owner: String,
    pub reason: String,
}

pub fn check_workspace_connected(workspace_root: &Path) -> Result<ConnectedReport> {
    check_workspace_connected_with_allowlist(workspace_root, Path::new(DEFAULT_CONNECTED_ALLOWLIST))
}

pub fn check_workspace_connected_with_allowlist(
    workspace_root: &Path,
    allowlist_path: &Path,
) -> Result<ConnectedReport> {
    let metadata = cargo_metadata(workspace_root)?;
    let allowlist = load_connected_allowlist(workspace_root, allowlist_path)?;
    let allowed_names = allowlist
        .iter()
        .map(|allowance| allowance.crate_name.clone())
        .collect::<BTreeSet<_>>();
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

    let mut workspace_packages = BTreeMap::new();
    for package in packages {
        let Some(package_id) = package.get("id").and_then(Value::as_str) else {
            continue;
        };
        if !workspace_members.contains(package_id) {
            continue;
        }
        let name = package_name(package)?;
        workspace_packages.insert(name.to_owned(), package);
    }

    for allowance in &allowlist {
        if !workspace_packages.contains_key(&allowance.crate_name) {
            bail!(
                "{} allowlists unknown crate {:?}",
                resolve_allowlist_path(workspace_root, allowlist_path).display(),
                allowance.crate_name
            );
        }
    }

    let workspace_names = workspace_packages.keys().cloned().collect::<BTreeSet<_>>();
    let mut non_dev_reverse: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut dev_reverse: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (name, package) in &workspace_packages {
        for dependency in
            workspace_dependency_names(package, &workspace_names, DependencyReach::NonDev)?
        {
            non_dev_reverse
                .entry(dependency)
                .or_default()
                .push(name.clone());
        }
        for dependency in
            workspace_dependency_names(package, &workspace_names, DependencyReach::Dev)?
        {
            dev_reverse
                .entry(dependency)
                .or_default()
                .push(name.clone());
        }
    }

    let mut roots = workspace_packages
        .iter()
        .filter_map(|(name, package)| {
            (package_has_shipped_target(package) && !allowed_names.contains(name))
                .then_some(name.clone())
        })
        .collect::<Vec<_>>();
    roots.sort();

    let mut reachable = BTreeSet::new();
    let mut queue = VecDeque::new();
    for root in &roots {
        if reachable.insert(root.clone()) {
            queue.push_back(root.clone());
        }
    }
    while let Some(crate_name) = queue.pop_front() {
        let Some(package) = workspace_packages.get(&crate_name) else {
            continue;
        };
        for dependency in
            workspace_dependency_names(package, &workspace_names, DependencyReach::NonDev)?
        {
            if allowed_names.contains(&dependency) {
                continue;
            }
            if reachable.insert(dependency.clone()) {
                queue.push_back(dependency);
            }
        }
    }

    let mut findings = Vec::new();
    for name in workspace_packages.keys() {
        if reachable.contains(name) || allowed_names.contains(name) {
            continue;
        }
        findings.push(ConnectedFinding {
            crate_name: name.clone(),
            reason: connected_reason(name, &non_dev_reverse, &dev_reverse),
        });
    }

    Ok(ConnectedReport {
        roots,
        findings,
        allowed: allowlist,
    })
}

/// The same connectivity BFS as [`check_workspace_connected`], but with the
/// reported findings narrowed to the crates that belong to `family` — the
/// `lodestone-<family>` package itself, plus anything under
/// `crates/versions/<family>/`.
///
/// Connectivity is a whole-workspace property (a family crate can only be
/// judged reachable by walking the *entire* dependency graph from every
/// shipped root), so the BFS itself stays global — only the verdict handed
/// back to a `--family`-scoped caller is narrowed. Without this, `conformance
/// --family v340` fails on an unrelated orphan crate anywhere else in the
/// workspace, making a per-family tool hostage to unrelated workspace state.
/// This does not introduce a skip path: a real violation in `family`'s own
/// crates is still a finding here, so a subject that exists can still fail.
pub fn check_workspace_connected_for_family(
    workspace_root: &Path,
    family: &str,
) -> Result<ConnectedReport> {
    check_workspace_connected_for_family_with_allowlist(
        workspace_root,
        family,
        Path::new(DEFAULT_CONNECTED_ALLOWLIST),
    )
}

pub fn check_workspace_connected_for_family_with_allowlist(
    workspace_root: &Path,
    family: &str,
    allowlist_path: &Path,
) -> Result<ConnectedReport> {
    let report = check_workspace_connected_with_allowlist(workspace_root, allowlist_path)?;

    let metadata = cargo_metadata(workspace_root)?;
    let canonical_root = workspace_root
        .canonicalize()
        .with_context(|| format!("canonicalize workspace root {}", workspace_root.display()))?;
    let packages = metadata
        .get("packages")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("cargo metadata did not include packages"))?;
    let workspace_members = metadata
        .get("workspace_members")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("cargo metadata did not include workspace_members"))?
        .iter()
        .filter_map(Value::as_str)
        .collect::<BTreeSet<_>>();

    let mut family_names = BTreeSet::new();
    for package in packages {
        let Some(package_id) = package.get("id").and_then(Value::as_str) else {
            continue;
        };
        if !workspace_members.contains(package_id) {
            continue;
        }
        if package_belongs_to_family(&canonical_root, package, family)? {
            family_names.insert(package_name(package)?.to_owned());
        }
    }

    let findings = report
        .findings
        .iter()
        .filter(|finding| family_names.contains(&finding.crate_name))
        .cloned()
        .collect();

    Ok(ConnectedReport {
        roots: report.roots,
        findings,
        allowed: report.allowed,
    })
}

/// Whether a workspace package is part of protocol family `family`: either
/// its manifest lives under `crates/versions/<family>/`, or it is the
/// `lodestone-<family>` package by name (families whose crate is not nested
/// under `crates/versions` — none today, but the name check is the cheaper,
/// more durable identity and costs nothing to also check).
fn package_belongs_to_family(canonical_root: &Path, package: &Value, family: &str) -> Result<bool> {
    if package_name(package)? == format!("lodestone-{family}") {
        return Ok(true);
    }
    let manifest_path = package
        .get("manifest_path")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("workspace package is missing manifest_path"))?;
    let manifest_path = Path::new(manifest_path);
    let canonical_manifest = manifest_path
        .canonicalize()
        .with_context(|| format!("canonicalize manifest path {}", manifest_path.display()))?;
    let Ok(relative) = canonical_manifest.strip_prefix(canonical_root) else {
        // A workspace package outside the workspace root cannot be under
        // crates/versions/<family>/ either; not an error, just not this family.
        return Ok(false);
    };
    Ok(relative.starts_with(Path::new("crates/versions").join(family)))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DependencyReach {
    NonDev,
    Dev,
}

fn connected_reason(
    name: &str,
    non_dev_reverse: &BTreeMap<String, Vec<String>>,
    dev_reverse: &BTreeMap<String, Vec<String>>,
) -> ConnectedReason {
    let mut non_dev_dependents = non_dev_reverse.get(name).cloned().unwrap_or_default();
    non_dev_dependents.sort();
    non_dev_dependents.dedup();
    if !non_dev_dependents.is_empty() {
        return ConnectedReason::OnlyUnreachableDependents(non_dev_dependents);
    }
    let mut dev_dependents = dev_reverse.get(name).cloned().unwrap_or_default();
    dev_dependents.sort();
    dev_dependents.dedup();
    if !dev_dependents.is_empty() {
        return ConnectedReason::OnlyDevDependents(dev_dependents);
    }
    ConnectedReason::NoWorkspaceDependents
}

fn workspace_dependency_names(
    package: &Value,
    workspace_names: &BTreeSet<String>,
    reach: DependencyReach,
) -> Result<Vec<String>> {
    let Some(dependencies) = package.get("dependencies").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let mut names = Vec::new();
    for dependency in dependencies {
        let dependency_name = dependency
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("dependency is missing a name"))?;
        if !workspace_names.contains(dependency_name) {
            continue;
        }
        let kind = dependency_table_name(dependency.get("kind"));
        let include = match reach {
            DependencyReach::NonDev => kind != "dev-dependencies",
            DependencyReach::Dev => kind == "dev-dependencies",
        };
        if include {
            names.push(dependency_name.to_owned());
        }
    }
    Ok(names)
}

fn package_has_shipped_target(package: &Value) -> bool {
    package
        .get("targets")
        .and_then(Value::as_array)
        .is_some_and(|targets| targets.iter().any(target_is_shipped_artifact))
}

fn target_is_shipped_artifact(target: &Value) -> bool {
    target
        .get("kind")
        .and_then(Value::as_array)
        .is_some_and(|kinds| {
            kinds
                .iter()
                .filter_map(Value::as_str)
                .any(|kind| kind == "bin" || kind == "cdylib")
        })
        || target
            .get("crate_types")
            .and_then(Value::as_array)
            .is_some_and(|types| {
                types
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|ty| ty == "cdylib")
            })
}

fn package_name(package: &Value) -> Result<&str> {
    package
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("workspace package is missing a name"))
}

fn format_crate_list(crates: &[String]) -> String {
    match crates {
        [] => String::new(),
        [one] => one.clone(),
        [first, second] => format!("{first} and {second}"),
        many => {
            let mut list = many[..many.len() - 1].join(", ");
            let _ = write!(list, ", and {}", many[many.len() - 1]);
            list
        }
    }
}

fn load_connected_allowlist(
    workspace_root: &Path,
    allowlist_path: &Path,
) -> Result<Vec<ConnectedAllowance>> {
    let path = resolve_allowlist_path(workspace_root, allowlist_path);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let contents =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    parse_connected_allowlist(&contents)
        .map_err(|error| anyhow!("parse {}: {error:#}", path.display()))
}

fn resolve_allowlist_path(workspace_root: &Path, allowlist_path: &Path) -> PathBuf {
    if allowlist_path.is_absolute() {
        allowlist_path.to_owned()
    } else {
        workspace_root.join(allowlist_path)
    }
}

fn parse_connected_allowlist(contents: &str) -> Result<Vec<ConnectedAllowance>> {
    #[derive(Default)]
    struct Builder {
        crate_name: Option<String>,
        owner: Option<String>,
        reason: Option<String>,
    }

    fn finish(
        builder: Builder,
        index: usize,
        allowances: &mut Vec<ConnectedAllowance>,
    ) -> Result<()> {
        let crate_name = builder.crate_name.unwrap_or_default();
        let owner = builder.owner.unwrap_or_default();
        let reason = builder.reason.unwrap_or_default();
        let mut missing = Vec::new();
        if crate_name.trim().is_empty() {
            missing.push("crate");
        }
        if owner.trim().is_empty() {
            missing.push("owner");
        }
        if reason.trim().is_empty() {
            missing.push("reason");
        }
        if !missing.is_empty() {
            bail!(
                "allow entry {index} is missing non-empty {}",
                missing.join(", ")
            );
        }
        allowances.push(ConnectedAllowance {
            crate_name,
            owner,
            reason,
        });
        Ok(())
    }

    let mut allowances = Vec::new();
    let mut current: Option<Builder> = None;
    let mut entry_index = 0;
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if trimmed == "[[allow]]" {
            if let Some(builder) = current.take() {
                finish(builder, entry_index, &mut allowances)?;
            }
            entry_index += 1;
            current = Some(Builder::default());
            continue;
        }
        let Some(builder) = current.as_mut() else {
            bail!("allowlist entries must start with [[allow]]");
        };
        let (key, value) = parse_key_value_string(trimmed)?;
        match key {
            "crate" => builder.crate_name = Some(value),
            "owner" => builder.owner = Some(value),
            "reason" => builder.reason = Some(value),
            other => bail!("unsupported check-connected allowlist key {other:?}"),
        }
    }
    if let Some(builder) = current {
        finish(builder, entry_index, &mut allowances)?;
    }

    let mut seen = BTreeSet::new();
    for allowance in &allowances {
        if !seen.insert(allowance.crate_name.as_str()) {
            bail!(
                "duplicate check-connected allowlist entry for {:?}",
                allowance.crate_name
            );
        }
    }
    Ok(allowances)
}

fn parse_key_value_string(line: &str) -> Result<(&str, String)> {
    let (key, value) = line
        .split_once('=')
        .ok_or_else(|| anyhow!("expected key = \"value\", got {line:?}"))?;
    let key = key.trim();
    let value = value.trim();
    let Some(value) = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
    else {
        bail!("expected string value for {key:?}");
    };
    Ok((key, value.to_owned()))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConnectednessReport {
    pub families: Vec<ConnectednessFamily>,
    /// Families whose directory matched `is_protocol_family_name` but could
    /// not be scanned — missing `packet_ids.rs` or `adapter.rs` — with the
    /// reason. Named explicitly rather than dropped: the header claims
    /// "denominators from each family," and that must stay true even for a
    /// family that has bit-rotted to the point it no longer has these files.
    pub skipped: Vec<(String, String)>,
}

impl ConnectednessReport {
    #[must_use]
    pub fn has_unclassified(&self) -> bool {
        self.unclassified_count() > 0
    }

    /// Total unclassified arms across **both** axes this tool measures:
    /// clientbound dispatch (the original axis) and, per family capable of
    /// hosting, serverbound decode's own join against
    /// `crates/lodestone-server/src/server.rs`. Before this, the serverbound
    /// axis had no gate at all and was exactly as ignorable as the bare
    /// `53/69` encode count had been.
    #[must_use]
    pub fn unclassified_count(&self) -> usize {
        self.families
            .iter()
            .map(|family| {
                let mut count = family.unclassified.len();
                if let ServerboundDecodeAxis::Measured(summary) = &family.serverbound_decode {
                    count += summary.unclassified.len();
                }
                count
            })
            .sum()
    }

    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::from(
            "protocol connectedness (denominators from each family play::{clientbound,serverbound} packet_ids.rs):",
        );
        for family in &self.families {
            let _ = write!(
                out,
                "\n{}  clientbound decoded {}/{}; emits {}/{}; decoded-but-stranded {}",
                family.family,
                family.play_clientbound_decoded,
                family.play_clientbound_total,
                family.play_clientbound_emits,
                family.play_clientbound_total,
                family.play_clientbound_stranded_names.len()
            );
            if !family.play_clientbound_stranded_names.is_empty() {
                let _ = write!(
                    out,
                    " [{}]",
                    family.play_clientbound_stranded_names.join(", ")
                );
            }
            let _ = write!(
                out,
                "; serverbound encoded {}/{}; examined {} arm(s)",
                family.play_serverbound_encoded,
                family.play_serverbound_total,
                family.examined_clientbound_arms
            );
            match &family.serverbound_decode {
                ServerboundDecodeAxis::NotApplicable(reason) => {
                    let _ = write!(out, "; serverbound decode: not applicable ({reason})");
                }
                ServerboundDecodeAxis::Measured(summary) => {
                    let _ = write!(
                        out,
                        "; serverbound decoded {}/{}, connected {}/{}; examined {} arm(s)",
                        summary.decoded,
                        summary.total,
                        summary.connected,
                        summary.total,
                        summary.examined_arms
                    );
                    if !summary.stranded_names.is_empty() {
                        let _ = write!(
                            out,
                            "; decode-but-stranded {} [{}]",
                            summary.stranded_names.len(),
                            summary.stranded_names.join(", ")
                        );
                    }
                    if !summary.always_ignored_names.is_empty() {
                        let _ = write!(
                            out,
                            "; decodes-to-Ignored-only {} [{}]",
                            summary.always_ignored_names.len(),
                            summary.always_ignored_names.join(", ")
                        );
                    }
                }
            }
            if !family.play_clientbound_internal.is_empty() {
                let _ = write!(
                    out,
                    "\n  protocol-internal (decoded, no event by design — not islands):"
                );
                for (packet, reason) in &family.play_clientbound_internal {
                    let _ = write!(out, "\n    - {packet}: {reason}");
                }
            }
            if !family.unclassified.is_empty() {
                let _ = write!(out, "\n  UNCLASSIFIED (clientbound):");
                for arm in &family.unclassified {
                    let _ = write!(
                        out,
                        "\n    - {} at {}:{} ({})",
                        arm.packet, arm.file, arm.line, arm.reason
                    );
                }
            }
            if !family.depth_limited.is_empty() {
                let _ = write!(
                    out,
                    "\n  depth-limited at cap {} (clientbound):",
                    family.delegation_depth_cap
                );
                for arm in &family.depth_limited {
                    let _ = write!(out, "\n    - {} at {}:{}", arm.packet, arm.file, arm.line);
                }
            }
            if let ServerboundDecodeAxis::Measured(summary) = &family.serverbound_decode {
                if !summary.unclassified.is_empty() {
                    let _ = write!(out, "\n  UNCLASSIFIED (serverbound decode):");
                    for arm in &summary.unclassified {
                        let _ = write!(
                            out,
                            "\n    - {} at {}:{} ({})",
                            arm.packet, arm.file, arm.line, arm.reason
                        );
                    }
                }
                if !summary.depth_limited.is_empty() {
                    let _ = write!(out, "\n  depth-limited (serverbound decode):");
                    for arm in &summary.depth_limited {
                        let _ =
                            write!(out, "\n    - {} at {}:{}", arm.packet, arm.file, arm.line);
                    }
                }
            }
        }
        if !self.skipped.is_empty() {
            let _ = write!(out, "\nSKIPPED (could not be scanned):");
            for (family, reason) in &self.skipped {
                let _ = write!(out, "\n  - {family}: {reason}");
            }
        }
        out
    }
}

/// Clientbound packets that are **decoded and deliberately emit no `ClientEvent`**,
/// with the reason each one is legitimate.
///
/// The "decoded-but-stranded" verdict means an arm parses a packet and produces no
/// event, which is normally an island — the defect this whole tool exists to find.
/// But a handful of packets are *protocol-internal*: the client consumes them to
/// drive its own side of a handshake, and there is nothing for a renderer or a fold
/// to observe. Reporting those as stranded is a false positive, and a false positive
/// in an island detector is expensive here — Tier 1 item 9 has carried
/// `CHUNK_BATCH_START` as an open defect, and it was never one.
///
/// This is an allowlist with a **reason per entry**, printed in the report rather
/// than silently subtracted, so the exemption cannot itself become a hiding place.
/// Adding an entry needs the same standard as any other claim: say what consumes the
/// packet and why no event is right.
const PROTOCOL_INTERNAL_CLIENTBOUND: &[(&str, &str)] = &[
    (
        "CHUNK_BATCH_START",
        "empty marker; starts the batch rate timer (`begin_chunk_batch`). The client's \
         reply is emitted from CHUNK_BATCH_FINISHED as CHUNK_BATCH_RECEIVED carrying the \
         measured rate, and the server halts chunk delivery after ten unacknowledged \
         batches — so the handshake is load-bearing and complete, with nothing observable \
         at the START edge",
    ),
    (
        "UPDATE_TAGS",
        "issue #296: decodes the server's tag sync and installs the `minecraft:block` \
         registry's tags as a process-wide override consulted by \
         `lodestone_data::tool::block_tag_members` (`set_block_tag_overrides`), the single \
         lookup every tool-mining rule match goes through. That override is a side effect on \
         a global table, not a per-connection `ClientEvent` — there is nothing for a fold or \
         a renderer to observe at this packet's own edge, the same shape as \
         `CHUNK_BATCH_START` above",
    ),
];

/// The reason `packet` is exempt from the stranded verdict, if it is.
fn protocol_internal_reason(packet: &str) -> Option<&'static str> {
    PROTOCOL_INTERNAL_CLIENTBOUND
        .iter()
        .find(|(name, _)| *name == packet)
        .map(|(_, reason)| *reason)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConnectednessFamily {
    pub family: String,
    pub play_clientbound_total: usize,
    pub play_clientbound_decoded: usize,
    pub play_clientbound_emits: usize,
    pub play_clientbound_reaches_consumer: usize,
    pub play_clientbound_stranded_names: Vec<String>,
    /// Decoded, emitting no event, and **justified** — see
    /// [`PROTOCOL_INTERNAL_CLIENTBOUND`]. Held separately from
    /// `play_clientbound_stranded_names` so the exemption is visible in the report.
    pub play_clientbound_internal: Vec<(String, String)>,
    pub play_serverbound_total: usize,
    pub play_serverbound_encoded: usize,
    pub examined_clientbound_arms: usize,
    pub unclassified: Vec<ConnectednessUnknown>,
    pub depth_limited: Vec<ConnectednessUnknown>,
    pub delegation_depth_cap: usize,
    /// The serverbound **decode** axis — distinct from
    /// `play_serverbound_encoded` above, which is client-side encode. `None`
    /// families (no `src/server_protocol.rs`) don't implement
    /// `ServerProtocol` and so cannot host; see [`ServerboundDecodeAxis`].
    pub serverbound_decode: ServerboundDecodeAxis,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConnectednessUnknown {
    pub packet: String,
    pub file: String,
    pub line: usize,
    pub reason: String,
}

/// Whether a family's serverbound **decode** connectedness could be measured
/// at all, before asking how well-connected it is.
///
/// Only `v770` implements `ServerProtocol` today (`lodestone-registry` keeps
/// `Family` and `ServerFamily` as separate tables for exactly this reason —
/// joining and hosting are different sets). Reporting "0/69" for a family
/// that structurally cannot host would be exactly the kind of false claim
/// this tool exists to avoid making about itself.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ServerboundDecodeAxis {
    /// No `src/server_protocol.rs`, or one with no `impl ServerProtocol for`.
    NotApplicable(String),
    Measured(ServerboundDecodeSummary),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServerboundDecodeSummary {
    /// Denominator: total `play::serverbound` packet ids for this family.
    pub total: usize,
    /// Number of `State::Play if packet_id == play::serverbound::…` arms the
    /// scanner found in `server_protocol.rs`.
    pub examined_arms: usize,
    /// Arms that decode to at least one real (non-`Ignored`) `ServerBound`
    /// variant, or that decode but only ever produce `Ignored` — either way,
    /// "decoded" means the scanner reached a confident verdict about what
    /// the arm produces.
    pub decoded: usize,
    /// Of `decoded`, how many produce a variant that also has a
    /// **non-empty** match arm somewhere in
    /// `crates/lodestone-server/src/server.rs` — the second, cross-crate
    /// hop. A variant with only empty (`=> {}`) arms counts as stranded, not
    /// connected, even though the packet decoded successfully.
    pub connected: usize,
    /// Decoded to a real variant, but every arm handling that variant in
    /// `server.rs` is empty — decoded-but-stranded's serverbound analogue.
    pub stranded_names: Vec<String>,
    /// Decode arm exists and is unambiguous, but every branch of it produces
    /// `ServerBound::Ignored` — a vacuous decode, distinct from "no arm at
    /// all" (which is simply not in this list, since `examined_arms` is
    /// smaller than `total`).
    pub always_ignored_names: Vec<String>,
    pub unclassified: Vec<ConnectednessUnknown>,
    pub depth_limited: Vec<ConnectednessUnknown>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PlayPacketIdSummary {
    pub(crate) clientbound: Vec<PlayPacketEntry>,
    pub(crate) serverbound: Vec<PlayPacketEntry>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PlayPacketEntry {
    pub(crate) const_name: String,
    pub(crate) resource_name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ClientboundArm {
    packet: String,
    /// Adapter source file this arm's dispatch site lives in, relative to the
    /// workspace root. A protocol family's adapter can be one flat
    /// `src/adapter.rs` or a `src/adapter/` directory module (v770, since its
    /// split) — see [`read_adapter_sources`] — so this is per-arm rather than
    /// one path for the whole family.
    file: String,
    line: usize,
    pub(crate) verdict: ClientboundVerdict,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ClientboundVerdict {
    Emits {
        outlet: ConsumerOutlet,
        via: Option<String>,
    },
    DecodedButStranded,
    Unclassified {
        reason: String,
        depth_limited: bool,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ConsumerOutlet {
    ClientEvent,
    Directive,
    WorldSink,
}

pub fn connectedness_report(workspace_root: &Path) -> Result<ConnectednessReport> {
    let protocol_root = workspace_root.join("crates/versions");
    let mut families = Vec::new();
    let mut skipped = Vec::new();
    if !protocol_root.exists() {
        return Ok(ConnectednessReport { families, skipped });
    }

    for entry in std::fs::read_dir(&protocol_root)
        .with_context(|| format!("read protocol family directory {}", protocol_root.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let family = entry.file_name().to_string_lossy().into_owned();
        if !is_protocol_family_name(&family) {
            continue;
        }
        let family_dir = entry.path();
        let packet_ids_path = family_dir.join("src/generated/packet_ids.rs");
        // Every protocol family is scanned — there is no per-family opt-out
        // here. A family missing either file cannot be measured, and that is
        // reported by name rather than silently dropped from the family
        // list, per the false claim this replaced: the report header says
        // "denominators from each family," and a family that vanishes with
        // no trace makes that a lie for exactly the families most likely to
        // bit-rot unnoticed (the dormant v47/v340/v735 lines).
        if !packet_ids_path.exists() {
            skipped.push((
                family.clone(),
                format!(
                    "missing {}",
                    packet_ids_path
                        .strip_prefix(workspace_root)
                        .unwrap_or(&packet_ids_path)
                        .display()
                ),
            ));
            continue;
        }
        let adapter_sources = read_adapter_sources(&family_dir, workspace_root)?;
        if adapter_sources.is_empty() {
            skipped.push((
                family.clone(),
                format!(
                    "missing {} or {}",
                    family_dir
                        .join("src/adapter.rs")
                        .strip_prefix(workspace_root)
                        .unwrap_or(&family_dir.join("src/adapter.rs"))
                        .display(),
                    family_dir
                        .join("src/adapter/mod.rs")
                        .strip_prefix(workspace_root)
                        .unwrap_or(&family_dir.join("src/adapter/mod.rs"))
                        .display(),
                ),
            ));
            continue;
        }
        let packet_ids_source = std::fs::read_to_string(&packet_ids_path)
            .with_context(|| format!("read {}", packet_ids_path.display()))?;
        let play_ids = parse_play_packet_id_summary(&packet_ids_source)
            .with_context(|| format!("parse {}", packet_ids_path.display()))?;
        let depth_cap = 4;
        // The delegate-follow table is built across every file in the
        // adapter module together, since a dispatch arm in one file (e.g.
        // v770's `adapter/mod.rs`) can delegate to a helper defined in a
        // sibling submodule (`adapter/chat.rs`). Arms themselves are scanned
        // per file so each keeps its own correct `file`/`line`.
        let mut functions: BTreeMap<String, FunctionBody<'_>> = BTreeMap::new();
        for (_, content) in &adapter_sources {
            functions.extend(extract_functions(content)?);
        }
        let mut arms: BTreeMap<String, ClientboundArm> = BTreeMap::new();
        for (rel_path, content) in &adapter_sources {
            let mut file_arms =
                classify_clientbound_dispatch(content, &functions, rel_path, depth_cap)
                    .with_context(|| format!("classify {rel_path}"))?;
            // Families using a data-driven `dispatch::Table` carry no
            // `if packet_id ==` arms at all, so both shapes are scanned and
            // merged. A family is expected to use one or the other; scanning
            // both means a half-converted family still reports every arm
            // rather than silently losing the converted half.
            file_arms.extend(classify_clientbound_dispatch_table(
                content,
                &functions,
                rel_path,
                depth_cap,
                &play_ids.clientbound,
            ));
            for (packet, arm) in file_arms {
                if let Some(previous) = arms.insert(packet.clone(), arm) {
                    bail!(
                        "duplicate play clientbound dispatch arm {packet} in {rel_path} \
                         (already seen in {})",
                        previous.file
                    );
                }
            }
        }
        let combined_adapter_source = adapter_sources
            .iter()
            .map(|(_, content)| content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let serverbound_encoded =
            encoded_serverbound_packets(&combined_adapter_source, &play_ids.serverbound);
        let serverbound_decode =
            serverbound_decode_summary(workspace_root, &family_dir, &play_ids.serverbound)?;

        let mut stranded = Vec::new();
        let mut internal = Vec::new();
        let mut unclassified = Vec::new();
        let mut depth_limited = Vec::new();
        let mut decoded = 0;
        let mut emits = 0;
        for arm in arms.values() {
            match &arm.verdict {
                ClientboundVerdict::Emits { .. } => {
                    decoded += 1;
                    emits += 1;
                }
                ClientboundVerdict::DecodedButStranded => {
                    decoded += 1;
                    // Protocol-internal packets are decoded on purpose and have no
                    // event to emit; anything else with this verdict is an island.
                    match protocol_internal_reason(&arm.packet) {
                        Some(reason) => {
                            internal.push((arm.packet.clone(), reason.to_owned()));
                        }
                        None => stranded.push(arm.packet.clone()),
                    }
                }
                ClientboundVerdict::Unclassified {
                    reason,
                    depth_limited: limited,
                } => {
                    // Same allowlist as `DecodedButStranded` above, reached
                    // from the opposite direction: `UPDATE_TAGS` delegates
                    // to a helper returning `Result<(), _>` rather
                    // than `Result<Vec<Directive>, _>` (it has no directives
                    // to produce, only a side effect on a global table), so
                    // `classify_body`'s delegate-follow never finds a
                    // recognized outlet *or* the literal `Ok(Vec::new())` +
                    // `reader.`/`ensure_empty` pair `is_decoded_but_stranded`
                    // needs — that evidence lives in the callee, and a
                    // stranded verdict on a followed delegate is deliberately
                    // discarded, not propagated, a few lines above. The
                    // packet is still genuinely decoded and genuinely
                    // produces no event, the same claim `DecodedButStranded`
                    // makes; the allowlist entry carries the same "say what
                    // consumes it" bar either way.
                    match protocol_internal_reason(&arm.packet) {
                        Some(reason) => {
                            decoded += 1;
                            internal.push((arm.packet.clone(), reason.to_owned()));
                        }
                        None => {
                            let unknown = ConnectednessUnknown {
                                packet: arm.packet.clone(),
                                file: arm.file.clone(),
                                line: arm.line,
                                reason: reason.clone(),
                            };
                            if *limited {
                                depth_limited.push(unknown);
                            } else {
                                unclassified.push(unknown);
                            }
                        }
                    }
                }
            }
        }
        stranded.sort();
        internal.sort();
        unclassified.sort_by(|a, b| a.packet.cmp(&b.packet));
        depth_limited.sort_by(|a, b| a.packet.cmp(&b.packet));

        // A family whose adapter plainly dispatches, but from which this
        // scanner extracted nothing, is a scanner failure and must not be
        // reported as a score.
        //
        // Both of this function's arm scanners recognise dispatch by its
        // *spelling*: the if-chain scanner by literal `if packet_id ==`, the
        // table scanner by a literal `Handler::new(` with the resource name
        // in the preceding 400 bytes. Neither is a property of a correct
        // adapter, so a family is free to be correct and unreadable at the
        // same time — and the result of that is `decoded 0/N`, which reads
        // exactly like a family that decodes nothing. That has now happened
        // twice, for two different spellings: once when three families moved
        // to a data-driven table, and once when a family factored its
        // entries through a `const fn` helper instead of writing the call
        // literally.
        //
        // Zero is therefore only believable when there is nothing to find.
        // With dispatch evidence present and no arms parsed, fail loudly and
        // name what to look at, because the failure is otherwise indis-
        // tinguishable from the very defect the whole subcommand exists to
        // report.
        if arms.is_empty() && !play_ids.clientbound.is_empty() {
            const DISPATCH_MARKERS: [&str; 4] =
                ["if packet_id ==", "Handler::new(", "dispatch::Table", "Handler<"];
            if let Some(marker) = DISPATCH_MARKERS
                .iter()
                .find(|marker| combined_adapter_source.contains(**marker))
            {
                bail!(
                    "family {family} has {} play clientbound packet ids and its adapter \
                     contains `{marker}`, but no dispatch arm could be parsed from it. This \
                     is a failure of this scanner, not a score of zero: the if-chain arm \
                     reader anchors on the literal text `if packet_id ==`, and the table \
                     reader on a literal `Handler::new(` whose resource-name string literal \
                     lies within the preceding 400 bytes. An adapter that builds its entries \
                     through a helper, a macro, or any other indirection is correct and \
                     unreadable here. Either spell the entries so one anchor matches, or \
                     teach `classify_clientbound_dispatch_table` the new shape -- but do not \
                     leave it reporting zero.",
                    play_ids.clientbound.len()
                );
            }
        }

        families.push(ConnectednessFamily {
            family,
            play_clientbound_total: play_ids.clientbound.len(),
            play_clientbound_decoded: decoded,
            play_clientbound_emits: emits,
            play_clientbound_reaches_consumer: emits,
            play_clientbound_stranded_names: stranded,
            play_clientbound_internal: internal,
            play_serverbound_total: play_ids.serverbound.len(),
            play_serverbound_encoded: serverbound_encoded.len(),
            examined_clientbound_arms: arms.len(),
            unclassified,
            depth_limited,
            delegation_depth_cap: depth_cap,
            serverbound_decode,
        });
    }
    families.sort_by(|a, b| {
        protocol_family_sort_key(&a.family).cmp(&protocol_family_sort_key(&b.family))
    });
    skipped.sort();
    Ok(ConnectednessReport { families, skipped })
}

/// A version-family directory name is either the legacy `v<protocol-number>`
/// form (kept for any future family that stays symmetric) or the era-start
/// Minecraft-version form the four renamed families now use under
/// `crates/versions/` (dot-separated digit groups, e.g. `1.8`, `26.2`).
fn is_protocol_family_name(name: &str) -> bool {
    let is_legacy_v_number = name
        .strip_prefix('v')
        .is_some_and(|suffix| !suffix.is_empty() && suffix.chars().all(|ch| ch.is_ascii_digit()));
    let is_dotted_version = !name.is_empty()
        && name
            .split('.')
            .all(|part| !part.is_empty() && part.chars().all(|ch| ch.is_ascii_digit()));
    is_legacy_v_number || is_dotted_version
}

/// Orders family directory names for display: legacy `v<protocol-number>`
/// families first (by protocol number), then era-start Minecraft-version
/// directories compared component-wise (`1.8` < `1.9` < `1.14` < `26.2`).
fn protocol_family_sort_key(name: &str) -> Vec<u32> {
    if let Some(value) = name.strip_prefix('v').and_then(|suffix| suffix.parse::<u32>().ok()) {
        return vec![0, value];
    }
    let parts: Vec<u32> = name.split('.').filter_map(|part| part.parse::<u32>().ok()).collect();
    if parts.is_empty() {
        return vec![u32::MAX];
    }
    let mut key = vec![1];
    key.extend(parts);
    key
}

pub(crate) fn parse_play_packet_id_summary(source: &str) -> Result<PlayPacketIdSummary> {
    let play = extract_named_block(source, "pub mod play")
        .or_else(|| extract_named_block(source, "mod play"))
        .ok_or_else(|| anyhow!("packet_ids.rs is missing pub mod play"))?;
    let clientbound = extract_named_block(play, "pub mod clientbound")
        .or_else(|| extract_named_block(play, "mod clientbound"))
        .ok_or_else(|| anyhow!("packet_ids.rs play module is missing clientbound"))?;
    let serverbound = extract_named_block(play, "pub mod serverbound")
        .or_else(|| extract_named_block(play, "mod serverbound"))
        .ok_or_else(|| anyhow!("packet_ids.rs play module is missing serverbound"))?;

    Ok(PlayPacketIdSummary {
        clientbound: parse_packet_entries(clientbound, "play::clientbound")?,
        serverbound: parse_packet_entries(serverbound, "play::serverbound")?,
    })
}

fn parse_packet_entries(module_body: &str, label: &str) -> Result<Vec<PlayPacketEntry>> {
    if !module_body.contains("ENTRIES") {
        bail!("{label} is missing ENTRIES");
    }
    let mut entries = Vec::new();
    for line in module_body.lines() {
        let trimmed = line.trim();
        let Some(after_const) = trimmed.strip_prefix("pub const ") else {
            continue;
        };
        let const_name = after_const
            .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
            .next()
            .unwrap_or_default()
            .to_owned();
        if const_name.is_empty() {
            bail!("malformed {label} constant line {trimmed:?}");
        }
        let resource_name = format!("minecraft:{}", const_name.to_ascii_lowercase());
        entries.push(PlayPacketEntry {
            const_name,
            resource_name,
        });
    }
    if entries.is_empty() {
        bail!("{label} did not contain packet constants");
    }
    let mut seen = BTreeSet::new();
    for entry in &entries {
        if !seen.insert(entry.const_name.as_str()) {
            bail!("{label} contains duplicate packet {}", entry.const_name);
        }
    }
    Ok(entries)
}

/// Scans one adapter source file's text for `if packet_id ==
/// play::clientbound::X { .. }` dispatch arms and classifies each.
///
/// `functions` is the delegate-lookup table `classify_body` follows through —
/// callers pass one built across *every* file in the family's adapter module
/// (see [`read_adapter_sources`]), not just this file, because a dispatch arm
/// in one submodule can delegate to a helper defined in a sibling submodule
/// (v770's `src/adapter/mod.rs` calling into `src/adapter/chat.rs`, etc.).
/// `file` is the relative path recorded on each arm for reporting.
pub(crate) fn classify_clientbound_dispatch(
    adapter_source: &str,
    functions: &BTreeMap<String, FunctionBody<'_>>,
    file: &str,
    depth_cap: usize,
) -> Result<BTreeMap<String, ClientboundArm>> {
    let prefix = "if packet_id == play::clientbound::";
    let mut search_from = 0;
    let mut arms = BTreeMap::new();
    while let Some(relative) = adapter_source[search_from..].find(prefix) {
        let start = search_from + relative;
        let packet_start = start + prefix.len();
        let packet_end = adapter_source[packet_start..]
            .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
            .map(|offset| packet_start + offset)
            .ok_or_else(|| anyhow!("unterminated clientbound packet id at byte {packet_start}"))?;
        let packet = adapter_source[packet_start..packet_end].to_owned();
        let open = adapter_source[packet_end..]
            .find('{')
            .map(|offset| packet_end + offset)
            .ok_or_else(|| anyhow!("packet arm {packet} has no body"))?;
        let close = matching_brace(adapter_source, open)
            .ok_or_else(|| anyhow!("packet arm {packet} has an unclosed body"))?;
        let body = &adapter_source[open + 1..close];
        let line = line_number(adapter_source, start);
        let verdict = classify_body(body, functions, depth_cap, None);
        if arms
            .insert(
                packet.clone(),
                ClientboundArm {
                    packet,
                    file: file.to_owned(),
                    line,
                    verdict,
                },
            )
            .is_some()
        {
            bail!("duplicate play clientbound dispatch arm");
        }
        search_from = close + 1;
    }
    Ok(arms)
}

/// Scans one adapter source file for **data-driven dispatch tables**, and
/// classifies each handler exactly as [`classify_clientbound_dispatch`]
/// classifies an `if packet_id == ...` arm.
///
/// The legacy families moved from an if-chain to a
/// `lodestone_core::dispatch::Table` built from a `static` slice of
/// `(resource name, Handler::new(range, Type::fn))` pairs. That was a real
/// improvement — a terminal `_ =>` arm silently swallows an unhandled packet
/// forever, whereas the table makes every unhandled id an enumerated entry —
/// but it left this scanner blind, reporting **0 arms** for all three
/// converted families because it searched for the if-chain's literal text.
/// The code was fine and the instrument was not, which is the more dangerous
/// way round.
///
/// Anchors on `Handler::new(` rather than the enclosing `static`'s header,
/// because the families spell the table differently — one names it
/// `PLAY_CLIENTBOUND_HANDLERS` and two name it `CLIENTBOUND`; one puts an
/// entry on a single line and two wrap it across four. The anchor is the one
/// thing all three share.
///
/// `entries` maps a resource name back to its `const` name so a table keyed on
/// `"minecraft:login"` reports against the same `LOGIN` key the if-chain
/// scanner used, letting the two be merged.
fn classify_clientbound_dispatch_table(
    adapter_source: &str,
    functions: &BTreeMap<String, FunctionBody<'_>>,
    file: &str,
    depth_cap: usize,
    entries: &[PlayPacketEntry],
) -> BTreeMap<String, ClientboundArm> {
    let by_resource: BTreeMap<&str, &str> = entries
        .iter()
        .map(|e| (e.resource_name.as_str(), e.const_name.as_str()))
        .collect();
    let needle = "Handler::new(";
    let mut arms = BTreeMap::new();
    let mut search_from = 0;
    while let Some(relative) = adapter_source[search_from..].find(needle) {
        let start = search_from + relative;
        search_from = start + needle.len();

        // The resource name is the nearest string literal *before* the call,
        // searched in a bounded window so a stray `Handler::new` elsewhere
        // cannot reach back across the file and claim an unrelated literal.
        let window_start = start.saturating_sub(400);
        let window = &adapter_source[window_start..start];
        let Some(close_quote) = window.rfind('"') else {
            continue;
        };
        let Some(open_quote) = window[..close_quote].rfind('"') else {
            continue;
        };
        let Some(const_name) = by_resource.get(&window[open_quote + 1..close_quote]) else {
            continue;
        };

        // The handler is the final argument of `new(range, path)`. Parenthesis
        // matching rather than a comma split, since the range argument may
        // itself be a call.
        let open = start + needle.len() - 1;
        let Some(close) = matching_delim(adapter_source, open, b'(', b')') else {
            continue;
        };
        // The last *non-empty* comma-separated argument: a multi-line entry
        // carries a trailing comma, so a plain `rsplit(',').next()` yields the
        // whitespace after it and silently loses the whole family. Then strip
        // an `as <FnType>` cast, which two of the three families write and one
        // does not.
        let handler = adapter_source[open + 1..close]
            .rsplit(',')
            .map(str::trim)
            .find(|arg| !arg.is_empty())
            .unwrap_or("")
            .split(" as ")
            .next()
            .unwrap_or("")
            .trim()
            .rsplit("::")
            .next()
            .unwrap_or("")
            .trim();
        if handler.is_empty() {
            continue;
        }

        let verdict = match functions.get(handler) {
            Some(body) => classify_body(body.body, functions, depth_cap, None),
            // Reported, never silently dropped: a scanner that quietly skips
            // its own subject is precisely the failure this function exists
            // to correct.
            None => ClientboundVerdict::Unclassified {
                reason: format!("dispatch-table handler `{handler}` not found in adapter sources"),
                depth_limited: false,
            },
        };
        arms.insert(
            (*const_name).to_owned(),
            ClientboundArm {
                packet: (*const_name).to_owned(),
                file: file.to_owned(),
                line: line_number(adapter_source, start),
                verdict,
            },
        );
    }
    arms
}

/// Resolves a protocol family's adapter source to a list of `(path relative
/// to workspace root, file content)` pairs, in a deterministic order.
///
/// Two shapes are legal Rust module layouts and both are used in this repo:
/// a flat `src/adapter.rs`, or a `src/adapter/` directory module rooted at
/// `mod.rs` with any number of declared submodules (v770's shape, since its
/// dispatch code grew past one file — `chat.rs`, `chunk.rs`, `connection.rs`,
/// `entity.rs`, `inventory.rs`, `player.rs`, `scoreboard.rs`,
/// `serverbound.rs`). A connectedness scan that only ever looked for the flat
/// file silently skipped every family using the directory shape; this walks
/// whichever shape is actually on disk instead of assuming one.
///
/// Returns an empty `Vec` if neither shape exists, which the caller treats
/// the same as "missing adapter" for the skip report.
fn read_adapter_sources(family_dir: &Path, workspace_root: &Path) -> Result<Vec<(String, String)>> {
    let flat = family_dir.join("src/adapter.rs");
    if flat.exists() {
        let content = std::fs::read_to_string(&flat)
            .with_context(|| format!("read {}", flat.display()))?;
        let rel = flat
            .strip_prefix(workspace_root)
            .unwrap_or(&flat)
            .display()
            .to_string();
        return Ok(vec![(rel, content)]);
    }
    let dir = family_dir.join("src/adapter");
    if !dir.join("mod.rs").exists() {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    collect_rs_files(&dir, &mut paths)?;
    paths.sort();
    let mut sources = Vec::with_capacity(paths.len());
    for path in paths {
        let content =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let rel = path
            .strip_prefix(workspace_root)
            .unwrap_or(&path)
            .display()
            .to_string();
        sources.push((rel, content));
    }
    Ok(sources)
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("read dir {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect_rs_files(&path, out)?;
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub(crate) struct FunctionBody<'a> {
    pub(crate) body: &'a str,
}

pub(crate) fn extract_functions(source: &str) -> Result<BTreeMap<String, FunctionBody<'_>>> {
    let mut functions = BTreeMap::new();
    let mut search_from = 0;
    while let Some(relative) = source[search_from..].find("fn ") {
        let fn_pos = search_from + relative;
        if fn_pos > 0 {
            let prev = source.as_bytes()[fn_pos - 1];
            if prev.is_ascii_alphanumeric() || prev == b'_' {
                search_from = fn_pos + 3;
                continue;
            }
        }
        let name_start = fn_pos + 3;
        let name_end = source[name_start..]
            .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
            .map(|offset| name_start + offset)
            .ok_or_else(|| anyhow!("unterminated function name at byte {name_start}"))?;
        let name = &source[name_start..name_end];
        let Some(open) = source[name_end..].find('{').map(|offset| name_end + offset) else {
            search_from = name_end;
            continue;
        };
        let close = matching_brace(source, open)
            .ok_or_else(|| anyhow!("function {name} has an unclosed body"))?;
        functions.insert(
            name.to_owned(),
            FunctionBody {
                body: &source[open + 1..close],
            },
        );
        search_from = close + 1;
    }
    Ok(functions)
}

fn classify_body(
    body: &str,
    functions: &BTreeMap<String, FunctionBody<'_>>,
    remaining_depth: usize,
    via: Option<String>,
) -> ClientboundVerdict {
    if body.contains("ClientEvent::") {
        return ClientboundVerdict::Emits {
            outlet: ConsumerOutlet::ClientEvent,
            via,
        };
    }
    if body.contains("Directive::") || body.contains("send(") {
        return ClientboundVerdict::Emits {
            outlet: ConsumerOutlet::Directive,
            via,
        };
    }
    if body.contains("world.")
        || body.contains("sink.")
        || body.contains(".set_block(")
        || body.contains(".merge(")
    {
        return ClientboundVerdict::Emits {
            outlet: ConsumerOutlet::WorldSink,
            via,
        };
    }

    let delegates = delegate_function_calls(body, functions);
    if !delegates.is_empty() {
        if remaining_depth == 0 {
            return ClientboundVerdict::Unclassified {
                reason: format!(
                    "delegation depth cap reached while following {}",
                    delegates.join(", ")
                ),
                depth_limited: true,
            };
        }
        let mut saw_unclassified = None;
        for delegate in delegates {
            let Some(function) = functions.get(&delegate) else {
                continue;
            };
            match classify_body(
                function.body,
                functions,
                remaining_depth - 1,
                Some(delegate.clone()),
            ) {
                ClientboundVerdict::Emits { outlet, .. } => {
                    return ClientboundVerdict::Emits {
                        outlet,
                        via: Some(delegate),
                    };
                }
                ClientboundVerdict::DecodedButStranded => {}
                ClientboundVerdict::Unclassified {
                    reason,
                    depth_limited,
                } => {
                    saw_unclassified = Some((reason, depth_limited));
                }
            }
        }
        if let Some((reason, depth_limited)) = saw_unclassified {
            return ClientboundVerdict::Unclassified {
                reason,
                depth_limited,
            };
        }
    }

    if is_decoded_but_stranded(body) {
        return ClientboundVerdict::DecodedButStranded;
    }

    ClientboundVerdict::Unclassified {
        reason: "no recognized consumer outlet, explicit empty return, or classifiable delegate"
            .to_owned(),
        depth_limited: false,
    }
}

pub(crate) fn delegate_function_calls(
    body: &str,
    functions: &BTreeMap<String, FunctionBody<'_>>,
) -> Vec<String> {
    let mut delegates = Vec::new();
    let mut start = 0;
    while let Some(pos) = body[start..].find('(') {
        let open = start + pos;
        let name_end = open;
        // `rfind` hands back the byte index where the matching character
        // *starts*, which is only safe to step past with `+ 1` if that
        // character is one byte (ASCII). `body` is raw source text with no
        // comment-skipping (unlike `find_outside_comments`/`matching_brace`),
        // so a comment containing a multi-byte character directly against an
        // identifier -- no space, e.g. `note—decode(` -- lands `idx + 1`
        // mid-character and panics on the slice below. `char_indices` gives
        // the matched char itself, so `idx + ch.len_utf8()` is the byte
        // offset just past the *whole* character, which is always a valid
        // boundary.
        let name_start = body[..name_end]
            .char_indices()
            .rev()
            .find(|&(_, ch)| !(ch.is_ascii_alphanumeric() || ch == '_'))
            .map_or(0, |(idx, ch)| idx + ch.len_utf8());
        let name = body[name_start..name_end].trim();
        let receiver_call = name_start > 0
            && matches!(body.as_bytes().get(name_start - 1), Some(b'.') | Some(b':'));
        if functions.contains_key(name)
            && !receiver_call
            && !matches!(
                name,
                "send"
                    | "decode_body"
                    | "decode_and_validate"
                    | "encode_body"
                    | "Ok"
                    | "Err"
                    | "Some"
                    | "Vec"
                    | "Reader"
            )
            && !delegates.iter().any(|existing| existing == name)
        {
            delegates.push(name.to_owned());
        }
        start = open + 1;
    }
    delegates
}

fn is_decoded_but_stranded(body: &str) -> bool {
    let returns_empty = body.contains("Ok(Vec::new())")
        || body.contains("Ok(vec![])")
        || body.contains("Ok(vec![])")
        || body.contains("return Vec::new()")
        || body.contains("Vec::new()")
        || body.contains("vec![]");
    let validates_or_decodes = body.contains("decode_body")
        || body.contains("decode_and_validate")
        || body.contains("Reader::new")
        || body.contains("ensure_empty")
        || body.contains("reader.");
    returns_empty && validates_or_decodes
}

fn encoded_serverbound_packets(
    adapter_source: &str,
    serverbound: &[PlayPacketEntry],
) -> BTreeSet<String> {
    let valid = serverbound
        .iter()
        .map(|entry| entry.const_name.as_str())
        .collect::<BTreeSet<_>>();
    let prefix = "play::serverbound::";
    let mut encoded = BTreeSet::new();
    let mut search_from = 0;
    while let Some(relative) = adapter_source[search_from..].find(prefix) {
        let start = search_from + relative + prefix.len();
        let end = adapter_source[start..]
            .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
            .map_or(adapter_source.len(), |offset| start + offset);
        let const_name = &adapter_source[start..end];
        if valid.contains(const_name) {
            encoded.insert(const_name.to_owned());
        }
        search_from = end;
    }
    encoded
}

fn extract_named_block<'a>(source: &'a str, marker: &str) -> Option<&'a str> {
    let start = source.find(marker)?;
    let open = source[start..].find('{')? + start;
    let close = matching_brace(source, open)?;
    Some(&source[open + 1..close])
}

fn matching_brace(source: &str, open: usize) -> Option<usize> {
    matching_delim(source, open, b'{', b'}')
}

/// Matching-delimiter scan that skips comments, string literals and Rust
/// lifetimes. Generalised from the brace-only version so the dispatch-table
/// scanner can match parentheses with the same care: a hand-rolled scanner
/// that treats every `'` as opening a char literal gets stuck the first time
/// it meets a lifetime, which has bitten three separate scanners in this
/// workspace.
fn matching_delim(source: &str, open: usize, open_byte: u8, close_byte: u8) -> Option<usize> {
    let bytes = source.as_bytes();
    if bytes.get(open) != Some(&open_byte) {
        return None;
    }
    let mut depth = 0usize;
    let mut i = open;
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    let mut in_string = false;
    let mut escaped = false;
    while i < bytes.len() {
        let b = bytes[i];
        let next = bytes.get(i + 1).copied();
        if in_line_comment {
            if b == b'\n' {
                in_line_comment = false;
            }
            i += 1;
            continue;
        }
        if in_block_comment {
            if b == b'*' && next == Some(b'/') {
                in_block_comment = false;
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if b == b'/' && next == Some(b'/') {
            in_line_comment = true;
            i += 2;
            continue;
        }
        if b == b'/' && next == Some(b'*') {
            in_block_comment = true;
            i += 2;
            continue;
        }
        if b == b'"' {
            in_string = true;
            i += 1;
            continue;
        }
        if b == b'\'' {
            // A lifetime (`'a`, `'static`, `'_`) never closes, so treating
            // every `'` as entering a stateful "in a char literal" mode gets
            // stuck for the rest of the scan the first time one appears —
            // see `char_literal_span`'s doc comment for where this bit.
            i = char_literal_span(bytes, i).unwrap_or(i + 1);
            continue;
        }
        if b == open_byte {
            depth += 1;
        } else if b == close_byte {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// The byte offset just past a Rust char literal beginning at `quote_pos`
/// (which must index a `'`), or `None` if what follows isn't actually a
/// closed char literal.
///
/// This distinguishes a real char literal (`'a'`, `'\n'`, `'\u{1F600}'`)
/// from a lifetime or bare apostrophe (`'a`, `'static`, `'_`) by *looking
/// ahead* for a closing quote rather than tracking "am I inside a char
/// literal" as scan state. The stateful version is the trap: a lifetime
/// never closes, so the first one flips a scanner into "in a char literal"
/// for the rest of the file, silently disabling comment/string/brace
/// detection from that point on. Measured here: `find_outside_comments`
/// scanning `crates/lodestone-server/src/server.rs` — which has
/// `fn container_title(menu: &str) -> &'static str` — panicked on a
/// multi-byte character hundreds of lines later, because the stateful
/// version had been "inside a char literal" (and therefore blindly
/// advancing one byte at a time without checking for UTF-8 boundaries
/// before its next real slice) ever since `'static`.
fn char_literal_span(bytes: &[u8], quote_pos: usize) -> Option<usize> {
    let mut j = quote_pos + 1;
    if j >= bytes.len() {
        return None;
    }
    if bytes[j] == b'\\' {
        j += 1;
        match *bytes.get(j)? {
            b'u' => {
                j += 1;
                if bytes.get(j) != Some(&b'{') {
                    return None;
                }
                j += 1;
                while bytes.get(j).is_some_and(|b| *b != b'}') {
                    j += 1;
                }
                if j >= bytes.len() {
                    return None;
                }
                j += 1; // consume '}'
            }
            b'\'' | b'"' | b'\\' | b'n' | b'r' | b't' | b'0' => j += 1,
            b'x' => {
                j += 1;
                for _ in 0..2 {
                    if bytes.get(j).is_some_and(u8::is_ascii_hexdigit) {
                        j += 1;
                    }
                }
            }
            _ => return None,
        }
    } else {
        // A single (possibly multi-byte UTF-8) character.
        let width = match bytes[j] {
            b0 if b0 & 0x80 == 0 => 1,
            b0 if b0 & 0xE0 == 0xC0 => 2,
            b0 if b0 & 0xF0 == 0xE0 => 3,
            b0 if b0 & 0xF8 == 0xF0 => 4,
            _ => 1,
        };
        j += width;
    }
    if bytes.get(j) == Some(&b'\'') {
        Some(j + 1)
    } else {
        None
    }
}

fn line_number(source: &str, byte_offset: usize) -> usize {
    source[..byte_offset.min(source.len())]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        + 1
}

// ---------------------------------------------------------------------------
// Serverbound decode (Job 1b): a second connectedness axis, entirely
// separate from the clientbound scanner above.
//
// `server_protocol.rs`'s `ServerProtocol::decode` dispatches with match
// arms, not the clientbound adapter's `if packet_id == … { }` chain, and
// match arms are not reliably brace-delimited: `=> ServerBound::Ignored,` is
// a single expression ending at a comma, not a `{}` block. A scanner that
// reused the clientbound classifier's `find('{')` verbatim would silently
// swallow the *next* arm's body whenever the current one has no brace of
// its own — see `match_arm_body` below and its test with a deliberately
// unbraced arm.
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ServerboundDecodeArm {
    packet: String,
    line: usize,
    pub(crate) verdict: ServerboundDecodeVerdict,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ServerboundDecodeVerdict {
    /// Produces at least one real (non-`Ignored`) `ServerBound` variant,
    /// possibly only on some branches (e.g. `PLAYER_ACTION`, whose ordinals
    /// 3-7 fall through to `Ignored` but 0-2 produce `BlockAction`).
    Emits {
        variants: Vec<String>,
        #[allow(dead_code)]
        via: Option<String>,
    },
    /// A recognizable decode arm whose every branch produces
    /// `ServerBound::Ignored` — the serverbound analogue of
    /// `ClientboundVerdict::DecodedButStranded`.
    AlwaysIgnored,
    Unclassified {
        reason: String,
        depth_limited: bool,
    },
}

/// Finds the end of a match arm's body starting right after its `=>`.
///
/// Handles both `{ … }` bodies (delegating to [`matching_brace`], which is
/// already comment/string/char-aware) and bare expression bodies that end
/// at the next **top-level** comma — depth-aware across `(){}[]` so a bare
/// expression containing a struct literal, call, or index is not truncated
/// early. If no top-level comma is found before a closing bracket would
/// take the depth negative (the boundary of whatever encloses this arm —
/// typically the match's own closing brace), the scan stops there instead,
/// which also correctly handles a final arm with no trailing comma.
pub(crate) fn match_arm_body(source: &str, arrow_end: usize) -> Option<(usize, usize)> {
    let bytes = source.as_bytes();
    let mut i = arrow_end;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if bytes.get(i) == Some(&b'{') {
        let close = matching_brace(source, i)?;
        return Some((i + 1, close));
    }

    let start = i;
    let mut depth: i32 = 0;
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    let mut in_string = false;
    let mut escaped = false;
    while i < bytes.len() {
        let b = bytes[i];
        let next = bytes.get(i + 1).copied();
        if in_line_comment {
            if b == b'\n' {
                in_line_comment = false;
            }
            i += 1;
            continue;
        }
        if in_block_comment {
            if b == b'*' && next == Some(b'/') {
                in_block_comment = false;
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if b == b'/' && next == Some(b'/') {
            in_line_comment = true;
            i += 2;
            continue;
        }
        if b == b'/' && next == Some(b'*') {
            in_block_comment = true;
            i += 2;
            continue;
        }
        if b == b'"' {
            in_string = true;
            i += 1;
            continue;
        }
        if b == b'\'' {
            // See `char_literal_span`'s doc comment: a lifetime never
            // closes, so a stateful "in a char literal" flag here would get
            // stuck exactly the way it did in `matching_brace` before this
            // fix, this time swallowing braces/brackets/parens into the
            // depth count that were never meant to be counted.
            i = char_literal_span(bytes, i).unwrap_or(i + 1);
            continue;
        }
        match b {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                if depth == 0 {
                    return Some((start, i));
                }
                depth -= 1;
            }
            b',' if depth == 0 => return Some((start, i)),
            _ => {}
        }
        i += 1;
    }
    Some((start, i))
}

/// Scans `server_protocol.rs` for `State::Play if packet_id ==
/// play::serverbound::NAME` decode arms and classifies each one.
pub(crate) fn classify_serverbound_decode(
    source: &str,
    depth_cap: usize,
) -> Result<BTreeMap<String, ServerboundDecodeArm>> {
    let functions = extract_functions(source)?;
    let prefix = "if packet_id == play::serverbound::";
    let mut search_from = 0;
    let mut arms = BTreeMap::new();
    while let Some(relative) = source[search_from..].find(prefix) {
        let start = search_from + relative;
        let packet_start = start + prefix.len();
        let packet_end = source[packet_start..]
            .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
            .map(|offset| packet_start + offset)
            .ok_or_else(|| anyhow!("unterminated serverbound packet id at byte {packet_start}"))?;
        let packet = source[packet_start..packet_end].to_owned();
        let arrow = source[packet_end..]
            .find("=>")
            .map(|offset| packet_end + offset)
            .ok_or_else(|| anyhow!("packet arm {packet} has no `=>`"))?;
        let (body_start, body_end) = match_arm_body(source, arrow + 2)
            .ok_or_else(|| anyhow!("packet arm {packet} has an unterminated body"))?;
        let body = &source[body_start..body_end];
        let line = line_number(source, start);
        let verdict = classify_serverbound_body(body, &functions, depth_cap, None);
        if arms
            .insert(
                packet.clone(),
                ServerboundDecodeArm {
                    packet,
                    line,
                    verdict,
                },
            )
            .is_some()
        {
            bail!("duplicate play serverbound decode arm");
        }
        search_from = body_end;
    }
    Ok(arms)
}

/// All distinct `ServerBound::Name` variant names referenced in `body`, in
/// first-seen order.
fn serverbound_variants_in(body: &str) -> Vec<String> {
    let prefix = "ServerBound::";
    let mut names = Vec::new();
    let mut start = 0;
    while let Some(pos) = body[start..].find(prefix) {
        let begin = start + pos + prefix.len();
        let end = body[begin..]
            .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
            .map_or(body.len(), |offset| begin + offset);
        let name = body[begin..end].to_owned();
        if name.is_empty() {
            start = begin.max(start + pos + prefix.len());
            continue;
        }
        if !names.contains(&name) {
            names.push(name);
        }
        start = end;
    }
    names
}

fn classify_serverbound_body(
    body: &str,
    functions: &BTreeMap<String, FunctionBody<'_>>,
    remaining_depth: usize,
    via: Option<String>,
) -> ServerboundDecodeVerdict {
    let variants = serverbound_variants_in(body);
    let real: Vec<String> = variants
        .iter()
        .filter(|name| name.as_str() != "Ignored")
        .cloned()
        .collect();
    if !real.is_empty() {
        return ServerboundDecodeVerdict::Emits { variants: real, via };
    }

    let delegates = delegate_function_calls(body, functions);
    if !delegates.is_empty() {
        if remaining_depth == 0 {
            return ServerboundDecodeVerdict::Unclassified {
                reason: format!(
                    "delegation depth cap reached while following {}",
                    delegates.join(", ")
                ),
                depth_limited: true,
            };
        }
        let mut saw_unclassified = None;
        for delegate in delegates {
            let Some(function) = functions.get(&delegate) else {
                continue;
            };
            match classify_serverbound_body(
                function.body,
                functions,
                remaining_depth - 1,
                Some(delegate.clone()),
            ) {
                ServerboundDecodeVerdict::Emits { variants, .. } => {
                    return ServerboundDecodeVerdict::Emits {
                        variants,
                        via: Some(delegate),
                    };
                }
                ServerboundDecodeVerdict::AlwaysIgnored => {}
                ServerboundDecodeVerdict::Unclassified {
                    reason,
                    depth_limited,
                } => {
                    saw_unclassified = Some((reason, depth_limited));
                }
            }
        }
        if let Some((reason, depth_limited)) = saw_unclassified {
            return ServerboundDecodeVerdict::Unclassified {
                reason,
                depth_limited,
            };
        }
    }

    if !variants.is_empty() {
        // Every branch we could see produced `ServerBound::Ignored` and
        // nothing else — a recognized, vacuous decode.
        return ServerboundDecodeVerdict::AlwaysIgnored;
    }

    ServerboundDecodeVerdict::Unclassified {
        reason: "no recognized ServerBound variant, explicit Ignored, or classifiable delegate"
            .to_owned(),
        depth_limited: false,
    }
}

/// Finds the next occurrence of `needle` in `source` at or after `from`,
/// skipping any occurrence inside a `//`/`/* */` comment or a string/char
/// literal.
///
/// This is the piece the clientbound scanner never needed: `adapter.rs`
/// doesn't carry doc comments that quote its own dispatch tokens, but
/// `crates/lodestone-server/src/server.rs` has several
/// (`/// … [`ServerBound::LoginStart`] …`), and a plain substring search
/// would let prose manufacture a false match/connection.
pub(crate) fn find_outside_comments(source: &str, from: usize, needle: &str) -> Option<usize> {
    let bytes = source.as_bytes();
    let needle_bytes = needle.as_bytes();
    let mut i = from;
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    let mut in_string = false;
    while i < bytes.len() {
        let b = bytes[i];
        let next = bytes.get(i + 1).copied();
        if in_line_comment {
            if b == b'\n' {
                in_line_comment = false;
            }
            i += 1;
            continue;
        }
        if in_block_comment {
            if b == b'*' && next == Some(b'/') {
                in_block_comment = false;
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        if in_string {
            // `escaped` only needs to survive one iteration, so it's local
            // rather than hoisted — unlike `matching_brace`'s, this loop
            // never needs to re-enter the string state mid-escape from
            // elsewhere.
            let mut j = i;
            let mut escaped = false;
            while j < bytes.len() {
                if escaped {
                    escaped = false;
                } else if bytes[j] == b'\\' {
                    escaped = true;
                } else if bytes[j] == b'"' {
                    j += 1;
                    break;
                }
                j += 1;
            }
            i = j;
            in_string = false;
            continue;
        }
        if b == b'/' && next == Some(b'/') {
            in_line_comment = true;
            i += 2;
            continue;
        }
        if b == b'/' && next == Some(b'*') {
            in_block_comment = true;
            i += 2;
            continue;
        }
        if b == b'"' {
            in_string = true;
            i += 1;
            continue;
        }
        if b == b'\'' {
            // See `char_literal_span`'s doc comment for why this can't be a
            // stateful "in a char literal" flag: a lifetime never closes.
            i = char_literal_span(bytes, i).unwrap_or(i + 1);
            continue;
        }
        // Byte-level comparison, never `source[i..].starts_with(needle)`:
        // `i` is a valid char boundary in every branch above, but a needle
        // match is checked on every remaining byte including UTF-8
        // continuation bytes from any multi-byte character elsewhere in the
        // file, and `str` indexing panics on those. Comparing bytes against
        // an ASCII needle can't panic and gives the identical answer.
        if bytes[i..].starts_with(needle_bytes) {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Whether `ServerBound::{variant}` has at least one **connected** (i.e.
/// non-empty-bodied) consumer anywhere in `dispatch_source`
/// (`crates/lodestone-server/src/server.rs`) — the cross-crate second hop.
///
/// A variant can legitimately appear more than once (the Play-state
/// dispatcher has real arms; an earlier handshake/login-state dispatcher
/// no-ops every Play variant, and vice versa for its own variants) — so this
/// takes the **best** verdict across all occurrences, not the first. Most
/// consumers are match arms, but a gate that must run before every ordinary
/// packet is an `if let` early return; `TeleportationAccepted` clears the
/// movement gate in exactly that shape. Looking only for `=>` would mistake
/// its later exhaustiveness no-op for the real consumer.
pub(crate) fn serverbound_variant_is_connected(dispatch_source: &str, variant: &str) -> Result<bool> {
    let early_return = format!("if let ServerBound::{variant}");
    let mut guard_search_from = 0;
    while let Some(pos) =
        find_outside_comments(dispatch_source, guard_search_from, &early_return)
    {
        let after = pos + early_return.len();
        let Some(equals) = find_outside_comments(dispatch_source, after, "=") else {
            bail!("ServerBound::{variant} early-return guard at byte {pos} has no `=`");
        };
        let Some(body_open) = find_outside_comments(dispatch_source, equals + 1, "{") else {
            bail!("ServerBound::{variant} early-return guard at byte {pos} has no body");
        };
        let Some(body_close) = matching_brace(dispatch_source, body_open) else {
            bail!(
                "ServerBound::{variant} early-return guard at byte {pos} has an unterminated body"
            );
        };
        if !dispatch_source[body_open + 1..body_close].trim().is_empty() {
            return Ok(true);
        }
        guard_search_from = body_close + 1;
    }

    let needle = format!("ServerBound::{variant}");
    let mut search_from = 0;
    let mut found_any = false;
    while let Some(pos) = find_outside_comments(dispatch_source, search_from, &needle) {
        found_any = true;
        let after = pos + needle.len();
        let Some(arrow) = find_outside_comments(dispatch_source, after, "=>") else {
            bail!("ServerBound::{variant} pattern at byte {pos} has no `=>`");
        };
        let (body_start, body_end) = match_arm_body(dispatch_source, arrow + 2).ok_or_else(
            || anyhow!("ServerBound::{variant} arm at byte {pos} has an unterminated body"),
        )?;
        if !dispatch_source[body_start..body_end].trim().is_empty() {
            return Ok(true);
        }
        search_from = body_end;
    }
    if !found_any {
        bail!("ServerBound::{variant} has no match arm anywhere in the dispatch source");
    }
    Ok(false)
}

/// Builds the serverbound decode axis for one family: `NotApplicable` if it
/// has no `src/server_protocol.rs` (only `v770` implements `ServerProtocol`
/// today), otherwise a full [`ServerboundDecodeSummary`] joined against
/// `crates/lodestone-server/src/server.rs`.
fn serverbound_decode_summary(
    workspace_root: &Path,
    family_dir: &Path,
    serverbound: &[PlayPacketEntry],
) -> Result<ServerboundDecodeAxis> {
    let server_protocol_path = family_dir.join("src/server_protocol.rs");
    if !server_protocol_path.exists() {
        return Ok(ServerboundDecodeAxis::NotApplicable(
            "no src/server_protocol.rs — family does not implement ServerProtocol, so it \
             cannot host"
                .to_owned(),
        ));
    }
    let source = std::fs::read_to_string(&server_protocol_path)
        .with_context(|| format!("read {}", server_protocol_path.display()))?;
    if !source.contains("impl ServerProtocol for") {
        return Ok(ServerboundDecodeAxis::NotApplicable(
            "src/server_protocol.rs exists but has no `impl ServerProtocol for` — not wired \
             as a host"
                .to_owned(),
        ));
    }

    let rel_path = server_protocol_path
        .strip_prefix(workspace_root)
        .unwrap_or(&server_protocol_path)
        .display()
        .to_string();
    let depth_cap = 4;
    let arms = classify_serverbound_decode(&source, depth_cap)
        .with_context(|| format!("classify {}", server_protocol_path.display()))?;

    let dispatch_path = workspace_root.join("crates/lodestone-server/src/server.rs");
    let dispatch_source = if dispatch_path.exists() {
        Some(
            std::fs::read_to_string(&dispatch_path)
                .with_context(|| format!("read {}", dispatch_path.display()))?,
        )
    } else {
        None
    };

    let mut decoded = 0usize;
    let mut connected = 0usize;
    let mut stranded = Vec::new();
    let mut always_ignored = Vec::new();
    let mut unclassified = Vec::new();
    let mut depth_limited = Vec::new();
    let mut connectivity_cache: BTreeMap<String, bool> = BTreeMap::new();

    for arm in arms.values() {
        match &arm.verdict {
            ServerboundDecodeVerdict::Emits { variants, .. } => {
                decoded += 1;
                let Some(dispatch_source) = dispatch_source.as_deref() else {
                    unclassified.push(ConnectednessUnknown {
                        packet: arm.packet.clone(),
                        file: rel_path.clone(),
                        line: arm.line,
                        reason: "decodes to a real ServerBound variant, but \
                                 crates/lodestone-server/src/server.rs is absent so the \
                                 second hop cannot be measured"
                            .to_owned(),
                    });
                    continue;
                };
                let mut any_connected = false;
                for variant in variants {
                    let is_connected = if let Some(cached) = connectivity_cache.get(variant) {
                        *cached
                    } else {
                        let joined = serverbound_variant_is_connected(dispatch_source, variant)
                            .with_context(|| {
                                format!("join ServerBound::{variant} against {}", dispatch_path.display())
                            })?;
                        connectivity_cache.insert(variant.clone(), joined);
                        joined
                    };
                    any_connected |= is_connected;
                }
                if any_connected {
                    connected += 1;
                } else {
                    stranded.push(arm.packet.clone());
                }
            }
            ServerboundDecodeVerdict::AlwaysIgnored => {
                decoded += 1;
                always_ignored.push(arm.packet.clone());
            }
            ServerboundDecodeVerdict::Unclassified {
                reason,
                depth_limited: limited,
            } => {
                let unknown = ConnectednessUnknown {
                    packet: arm.packet.clone(),
                    file: rel_path.clone(),
                    line: arm.line,
                    reason: reason.clone(),
                };
                if *limited {
                    depth_limited.push(unknown);
                } else {
                    unclassified.push(unknown);
                }
            }
        }
    }
    stranded.sort();
    always_ignored.sort();
    unclassified.sort_by(|a, b| a.packet.cmp(&b.packet));
    depth_limited.sort_by(|a, b| a.packet.cmp(&b.packet));

    Ok(ServerboundDecodeAxis::Measured(ServerboundDecodeSummary {
        total: serverbound.len(),
        examined_arms: arms.len(),
        decoded,
        connected,
        stranded_names: stranded,
        always_ignored_names: always_ignored,
        unclassified,
        depth_limited,
    }))
}
