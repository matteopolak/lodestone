use super::*;

/// Where a packet report is sourced from.
///
/// Modern versions carry Mojang's own authoritative `reports/packets.json`.
/// Protocol 47 predates that generator, so its ids come from the
/// community-maintained `minecraft-data` project instead.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PacketSource {
    /// Mojang's `.cache/mc/<version>/generated/reports/packets.json`.
    Mojang,
    /// `vendor/minecraft-data/data/pc/<version>/protocol.json`.
    MinecraftData,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum PacketState {
    Handshaking,
    Status,
    Login,
    Configuration,
    Play,
}

impl PacketState {
    pub const ALL: [Self; 5] = [
        Self::Handshaking,
        Self::Status,
        Self::Login,
        Self::Configuration,
        Self::Play,
    ];

    #[must_use]
    pub const fn report_key(self) -> &'static str {
        match self {
            Self::Handshaking => "handshake",
            Self::Status => "status",
            Self::Login => "login",
            Self::Configuration => "configuration",
            Self::Play => "play",
        }
    }

    #[must_use]
    pub const fn module_name(self) -> &'static str {
        match self {
            Self::Handshaking => "handshaking",
            Self::Status => "status",
            Self::Login => "login",
            Self::Configuration => "configuration",
            Self::Play => "play",
        }
    }

    #[must_use]
    pub const fn code_const(self) -> &'static str {
        match self {
            Self::Handshaking => "STATE_HANDSHAKING",
            Self::Status => "STATE_STATUS",
            Self::Login => "STATE_LOGIN",
            Self::Configuration => "STATE_CONFIGURATION",
            Self::Play => "STATE_PLAY",
        }
    }

    #[must_use]
    pub const fn code_value(self) -> u8 {
        match self {
            Self::Handshaking => 0,
            Self::Status => 1,
            Self::Login => 2,
            Self::Configuration => 3,
            Self::Play => 4,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum PacketBound {
    Clientbound,
    Serverbound,
}

impl PacketBound {
    pub const ALL: [Self; 2] = [Self::Clientbound, Self::Serverbound];

    #[must_use]
    pub const fn report_key(self) -> &'static str {
        match self {
            Self::Clientbound => "clientbound",
            Self::Serverbound => "serverbound",
        }
    }

    #[must_use]
    pub const fn module_name(self) -> &'static str {
        self.report_key()
    }

    #[must_use]
    pub const fn code_const(self) -> &'static str {
        match self {
            Self::Clientbound => "BOUND_CLIENTBOUND",
            Self::Serverbound => "BOUND_SERVERBOUND",
        }
    }

    #[must_use]
    pub const fn code_value(self) -> u8 {
        match self {
            Self::Clientbound => 0,
            Self::Serverbound => 1,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PacketEntry {
    pub state: PacketState,
    pub bound: PacketBound,
    pub name: String,
    pub protocol_id: i32,
    pub const_ident: String,
    /// The packet's canonical (Mojang) resource name, when known.
    ///
    /// Always `Some(name)` for a Mojang-sourced report: Mojang's own report
    /// names already are canonical. For a minecraft-data-sourced report this
    /// is `Some` only when [`minecraft_data_canonical_alias`] has a verified
    /// mapping for `name`, and `None` otherwise -- an unverified guess would
    /// be worse than an absent one, and every legacy table already works
    /// today without this field. This is the join key later multi-version
    /// stages use to line up a legacy packet with its v770 equivalent.
    pub canonical_name: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PacketReport {
    pub minecraft_version: String,
    pub protocol_version: i32,
    entries: Vec<PacketEntry>,
}

impl PacketReport {
    #[must_use]
    pub fn all_entries(&self) -> &[PacketEntry] {
        &self.entries
    }

    pub fn entries(
        &self,
        state: PacketState,
        bound: PacketBound,
    ) -> impl Iterator<Item = &PacketEntry> {
        self.entries
            .iter()
            .filter(move |entry| entry.state == state && entry.bound == bound)
    }

    #[must_use]
    pub fn count(&self, state: PacketState, bound: PacketBound) -> usize {
        self.entries(state, bound).count()
    }

    #[must_use]
    pub fn id_for(&self, state: PacketState, bound: PacketBound, name: &str) -> Option<i32> {
        self.entries(state, bound)
            .find(|entry| entry.name == name)
            .map(|entry| entry.protocol_id)
    }

    #[must_use]
    pub fn name_for(&self, state: PacketState, bound: PacketBound, id: i32) -> Option<&str> {
        self.entries(state, bound)
            .find(|entry| entry.protocol_id == id)
            .map(|entry| entry.name.as_str())
    }
}

pub fn parse_packet_report(
    json: &str,
    minecraft_version: impl Into<String>,
    protocol_version: i32,
) -> Result<PacketReport> {
    let root: Value = serde_json::from_str(json).context("parse packets.json")?;
    let root = root
        .as_object()
        .ok_or_else(|| anyhow!("packets.json root must be an object"))?;

    let mut entries = Vec::new();
    for state in PacketState::ALL {
        let Some(state_value) = root.get(state.report_key()) else {
            bail!("missing packet state {:?} ({})", state, state.report_key());
        };
        let state_object = state_value.as_object().ok_or_else(|| {
            anyhow!(
                "packet state {:?} ({}) must be an object",
                state,
                state.report_key()
            )
        })?;

        for bound in PacketBound::ALL {
            let Some(bound_value) = state_object.get(bound.report_key()) else {
                continue;
            };
            let bound_object = bound_value.as_object().ok_or_else(|| {
                anyhow!("packet direction {:?}/{:?} must be an object", state, bound)
            })?;

            for (name, packet_value) in sorted_object_entries(bound_object) {
                let protocol_id = packet_value
                    .get("protocol_id")
                    .and_then(Value::as_i64)
                    .ok_or_else(|| anyhow!("packet {name} is missing integer protocol_id"))?;
                let protocol_id = i32::try_from(protocol_id)
                    .with_context(|| format!("packet {name} protocol_id is out of i32 range"))?;

                entries.push(PacketEntry {
                    state,
                    bound,
                    // Mojang's report is the canonical name source, so a
                    // Mojang-sourced entry is trivially its own canonical
                    // name -- no lookup needed, unlike the minecraft-data
                    // path below.
                    canonical_name: Some(name.to_owned()),
                    name: name.to_owned(),
                    protocol_id,
                    const_ident: sanitize_packet_const_name(name),
                });
            }
        }
    }

    entries.sort_by_key(|entry| {
        (
            entry.state,
            entry.bound,
            entry.protocol_id,
            entry.name.clone(),
        )
    });

    Ok(PacketReport {
        minecraft_version: minecraft_version.into(),
        protocol_version,
        entries,
    })
}

/// State keys as they appear in a `minecraft-data` `protocol.json`.
///
/// Unlike Mojang's report (`handshake`), `minecraft-data` names the first state
/// `handshaking`, matching the generated module name.
const fn minecraft_data_state_key(state: PacketState) -> &'static str {
    match state {
        PacketState::Handshaking => "handshaking",
        PacketState::Status => "status",
        PacketState::Login => "login",
        PacketState::Configuration => "configuration",
        PacketState::Play => "play",
    }
}

/// Bound keys as they appear in a `minecraft-data` `protocol.json`.
const fn minecraft_data_bound_key(bound: PacketBound) -> &'static str {
    match bound {
        PacketBound::Clientbound => "toClient",
        PacketBound::Serverbound => "toServer",
    }
}

/// Parses a `minecraft-data` `protocol.json` into the shared [`PacketReport`].
///
/// `minecraft-data` stores packet ids inside each `<state>.<bound>.types.packet`
/// container: the `name` field is a `["mapper", { mappings: { "0x00": ".." } }]`
/// mapping hex ids to `minecraft-data`'s short packet names. Those names are the
/// community project's own vocabulary (for example `set_protocol`,
/// `kick_disconnect`, `map_chunk`), not Mojang resource identifiers; they are
/// namespaced here as `minecraft:<name>` purely to keep the generated table's
/// shape identical to the Mojang-sourced crates.
pub fn parse_minecraft_data_report(
    json: &str,
    minecraft_version: impl Into<String>,
    protocol_version: i32,
) -> Result<PacketReport> {
    let root: Value = serde_json::from_str(json).context("parse minecraft-data protocol.json")?;
    let root = root
        .as_object()
        .ok_or_else(|| anyhow!("protocol.json root must be an object"))?;

    let mut entries = Vec::new();
    for state in PacketState::ALL {
        let state_key = minecraft_data_state_key(state);
        let Some(state_value) = root.get(state_key) else {
            // minecraft-data omits states a version does not use (e.g. no
            // configuration state before 1.20.2). That is expected, not an error.
            continue;
        };
        let state_object = state_value
            .as_object()
            .ok_or_else(|| anyhow!("protocol state {state_key} must be an object"))?;

        for bound in PacketBound::ALL {
            let bound_key = minecraft_data_bound_key(bound);
            let Some(bound_value) = state_object.get(bound_key) else {
                continue;
            };
            let mappings = minecraft_data_packet_mappings(bound_value)
                .with_context(|| format!("read packet mappings for {state_key}/{bound_key}"))?;

            for (hex_id, name) in mappings {
                let protocol_id = parse_hex_packet_id(&hex_id).with_context(|| {
                    format!("parse packet id {hex_id:?} in {state_key}/{bound_key}")
                })?;
                let namespaced = format!("minecraft:{name}");
                let canonical_name =
                    minecraft_data_canonical_alias(&namespaced).map(str::to_owned);
                entries.push(PacketEntry {
                    state,
                    bound,
                    const_ident: sanitize_packet_const_name(&namespaced),
                    canonical_name,
                    name: namespaced,
                    protocol_id,
                });
            }
        }
    }

    entries.sort_by_key(|entry| {
        (
            entry.state,
            entry.bound,
            entry.protocol_id,
            entry.name.clone(),
        )
    });

    Ok(PacketReport {
        minecraft_version: minecraft_version.into(),
        protocol_version,
        entries,
    })
}

/// Verified minecraft-data name -> canonical (Mojang 26.2) name aliases.
///
/// Empty today: cross-referencing a legacy protocol's minecraft-data names
/// against Mojang's own report requires either a Mojang `--reports` run
/// against that old server jar, or a captured-bytes comparison against a
/// modern client -- real oracle work this stage deliberately does not
/// fabricate: v735 and v770 agree on only 7 of 88 `ENTRIES` names as plain
/// strings, so nothing here can be guessed from spelling. Each verified
/// pair is a one-line addition to this table; nothing else in the generator
/// needs to change to pick it up -- see [`minecraft_data_canonical_alias`]
/// and [`resolve_canonical_alias`].
const MINECRAFT_DATA_CANONICAL_ALIASES: &[(&str, &str)] = &[];

/// Looks up `name` (an already-namespaced minecraft-data packet name) in
/// [`MINECRAFT_DATA_CANONICAL_ALIASES`].
#[must_use]
fn minecraft_data_canonical_alias(name: &str) -> Option<&'static str> {
    resolve_canonical_alias(MINECRAFT_DATA_CANONICAL_ALIASES, name)
}

/// Pure lookup an alias table by exact name match, kept separate from
/// [`MINECRAFT_DATA_CANONICAL_ALIASES`] so the lookup logic itself is
/// testable against a synthetic table without depending on that table ever
/// being non-empty.
#[must_use]
pub(crate) fn resolve_canonical_alias<'a>(table: &[(&str, &'a str)], name: &str) -> Option<&'a str> {
    table
        .iter()
        .find(|(from, _)| *from == name)
        .map(|(_, to)| *to)
}

/// Finds first-party package-license declarations that do not preserve the
/// workspace's GPL-3.0-or-later policy. The scan deliberately reads only the
/// `[package]` and root `[workspace.package]` sections, so dependency metadata
/// and explanatory prose about third-party licenses are outside its scope.
#[cfg(test)]
pub(crate) fn first_party_manifest_license_violations(workspace_root: &Path) -> Result<Vec<String>> {
    let root_manifest = workspace_root.join("Cargo.toml");
    let mut manifests = vec![root_manifest.clone()];
    for directory in ["crates", "fuzz", "web", "xtask"] {
        collect_cargo_manifests(&workspace_root.join(directory), &mut manifests)?;
    }
    manifests.sort();
    manifests.dedup();

    let mut violations = Vec::new();
    for manifest_path in manifests {
        let manifest = std::fs::read_to_string(&manifest_path)
            .with_context(|| format!("read {}", manifest_path.display()))?;
        let declarations = manifest_license_declarations(&manifest);
        let package_name = declarations
            .iter()
            .find_map(|declaration| match declaration {
                ManifestDeclaration::PackageName(name) => Some(name.as_str()),
                ManifestDeclaration::License { .. } => None,
            });
        let first_party_package = package_name
            .is_some_and(|name| name.starts_with("lodestone-") || name == "xtask");
        let is_root = manifest_path == root_manifest;
        let has_scoped_license = declarations.iter().any(|declaration| {
            matches!(
                declaration,
                ManifestDeclaration::License {
                    workspace_package,
                    ..
                } if first_party_package || (is_root && *workspace_package)
            )
        });
        if (first_party_package || is_root) && !has_scoped_license {
            let relative = manifest_path
                .strip_prefix(workspace_root)
                .unwrap_or(&manifest_path)
                .display();
            violations.push(format!(
                "{relative}: {FIRST_PARTY_LICENSE} required, found no license declaration"
            ));
            continue;
        }

        for declaration in declarations {
            let ManifestDeclaration::License {
                workspace_package,
                value,
            } = declaration
            else {
                continue;
            };
            if !(first_party_package || (is_root && workspace_package)) {
                continue;
            }
            if value == "workspace" || value == FIRST_PARTY_LICENSE {
                continue;
            }
            let relative = manifest_path
                .strip_prefix(workspace_root)
                .unwrap_or(&manifest_path)
                .display();
            violations.push(format!(
                "{relative}: {FIRST_PARTY_LICENSE} required, found {value}"
            ));
        }
    }
    Ok(violations)
}

#[cfg(test)]
fn collect_cargo_manifests(directory: &Path, manifests: &mut Vec<PathBuf>) -> Result<()> {
    if !directory.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(directory)
        .with_context(|| format!("read {}", directory.display()))?
    {
        let entry = entry.with_context(|| format!("read entry under {}", directory.display()))?;
        let path = entry.path();
        if path.is_dir() {
            collect_cargo_manifests(&path, manifests)?;
        } else if path.file_name().is_some_and(|name| name == "Cargo.toml") {
            manifests.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
enum ManifestDeclaration {
    PackageName(String),
    License {
        workspace_package: bool,
        value: String,
    },
}

#[cfg(test)]
fn manifest_license_declarations(manifest: &str) -> Vec<ManifestDeclaration> {
    #[derive(Clone, Copy)]
    enum Section {
        Other,
        Package,
        WorkspacePackage,
    }

    let mut section = Section::Other;
    let mut declarations = Vec::new();
    for line in manifest.lines() {
        let line = line.trim();
        section = match line {
            "[package]" => Section::Package,
            "[workspace.package]" => Section::WorkspacePackage,
            line if line.starts_with('[') && line.ends_with(']') => Section::Other,
            _ => section,
        };
        if line.starts_with('#') {
            continue;
        }
        match section {
            Section::Package if line.starts_with("name =") => {
                if let Some(name) = manifest_string_value(line) {
                    declarations.push(ManifestDeclaration::PackageName(name.to_owned()));
                }
            }
            Section::Package | Section::WorkspacePackage if line.starts_with("license =") => {
                if let Some(license) = manifest_string_value(line) {
                    declarations.push(ManifestDeclaration::License {
                        workspace_package: matches!(section, Section::WorkspacePackage),
                        value: license.to_owned(),
                    });
                }
            }
            Section::Package if line == "license.workspace = true" => {
                declarations.push(ManifestDeclaration::License {
                    workspace_package: false,
                    value: "workspace".to_owned(),
                });
            }
            _ => {}
        }
    }
    declarations
}

#[cfg(test)]
fn manifest_string_value(line: &str) -> Option<&str> {
    let (_, value) = line.split_once('=')?;
    value.trim().strip_prefix('"')?.strip_suffix('"')
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum PacketShapeChangeKind {
    Added,
    Removed,
    Changed,
}

impl PacketShapeChangeKind {
    const fn as_str(&self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Removed => "removed",
            Self::Changed => "changed",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct PacketShapeChange {
    pub state: PacketState,
    pub bound: PacketBound,
    pub packet_name: String,
    pub kind: PacketShapeChangeKind,
}

impl PacketShapeChange {
    pub(crate) fn render(&self) -> String {
        format!(
            "{}/{}/{} {}",
            self.state.module_name(),
            self.bound.module_name(),
            self.packet_name,
            self.kind.as_str()
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShapeReviewManifest {
    pub source_family: String,
    pub target_family: String,
    pub source_minecraft_version: String,
    pub source_protocol_version: i32,
    pub target_minecraft_version: String,
    pub target_protocol_version: i32,
    pub entries: Vec<PacketShapeChange>,
}

pub fn render_shape_review_toml(review: &ShapeReviewManifest) -> Result<String> {
    let mut out = String::new();
    writeln!(
        out,
        "# Generated by `cargo xtask new-version`. DO NOT DELETE."
    )?;
    writeln!(
        out,
        "# Set `reviewed = true` only after auditing the packet codec against the target protocol."
    )?;
    writeln!(
        out,
        "# While any packet remains unreviewed, the family must not be registered as supported."
    )?;
    writeln!(
        out,
        "source_family = {:?}",
        toml_string(&review.source_family)
    )?;
    writeln!(
        out,
        "target_family = {:?}",
        toml_string(&review.target_family)
    )?;
    writeln!(
        out,
        "source_minecraft = {:?}",
        toml_string(&review.source_minecraft_version)
    )?;
    writeln!(out, "source_protocol = {}", review.source_protocol_version)?;
    writeln!(
        out,
        "target_minecraft = {:?}",
        toml_string(&review.target_minecraft_version)
    )?;
    writeln!(out, "target_protocol = {}", review.target_protocol_version)?;
    for entry in &review.entries {
        writeln!(out)?;
        writeln!(out, "[[packet]]")?;
        writeln!(out, "state = {:?}", entry.state.module_name())?;
        writeln!(out, "bound = {:?}", entry.bound.module_name())?;
        writeln!(out, "name = {:?}", toml_string(&entry.packet_name))?;
        writeln!(out, "change = {:?}", entry.kind.as_str())?;
        writeln!(out, "reviewed = false")?;
    }
    Ok(out)
}

fn toml_string(value: &str) -> String {
    value.to_owned()
}

pub fn check_shape_reviews(workspace_root: &Path) -> Result<()> {
    let protocol_dir = workspace_root.join("crates/versions");
    if !protocol_dir.exists() {
        return Ok(());
    }

    let mut violations = Vec::new();
    for entry in std::fs::read_dir(&protocol_dir)
        .with_context(|| format!("read {}", protocol_dir.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let family = entry.file_name().to_string_lossy().into_owned();
        let review_path = entry.path().join("SHAPE_REVIEW.toml");
        if !review_path.exists() {
            continue;
        }
        violations.extend(shape_review_violations(&family, &review_path)?);
    }

    if !violations.is_empty() {
        bail!(
            "undischarged packet shape review entries:\n{}",
            violations.join("\n")
        );
    }
    Ok(())
}

pub(crate) fn shape_review_violations(family: &str, review_path: &Path) -> Result<Vec<String>> {
    let contents = std::fs::read_to_string(review_path)
        .with_context(|| format!("read {}", review_path.display()))?;
    let mut current_packet = String::from("<unknown packet>");
    let mut violations = Vec::new();
    for line in contents.lines() {
        let trimmed = line.trim();
        if let Some(value) = trimmed.strip_prefix("name = ") {
            current_packet = value.trim_matches('"').to_owned();
        } else if trimmed == "reviewed = false" {
            violations.push(format!(
                "- {family}: {current_packet} in {} still has reviewed = false; set reviewed = true only after the codec has been audited",
                review_path.display()
            ));
        }
    }
    Ok(violations)
}

pub fn compare_minecraft_data_packet_shapes(
    source_json: &str,
    target_json: &str,
) -> Result<Vec<PacketShapeChange>> {
    let source = minecraft_data_packet_shapes(source_json).context("read source packet shapes")?;
    let target = minecraft_data_packet_shapes(target_json).context("read target packet shapes")?;
    let keys = source
        .keys()
        .chain(target.keys())
        .cloned()
        .collect::<BTreeSet<_>>();

    let mut changes = Vec::new();
    for (state, bound, packet_name) in keys {
        let key = (state, bound, packet_name.clone());
        match (source.get(&key), target.get(&key)) {
            (None, Some(_)) => changes.push(PacketShapeChange {
                state,
                bound,
                packet_name,
                kind: PacketShapeChangeKind::Added,
            }),
            (Some(_), None) => changes.push(PacketShapeChange {
                state,
                bound,
                packet_name,
                kind: PacketShapeChangeKind::Removed,
            }),
            (Some(source_shape), Some(target_shape)) if source_shape != target_shape => {
                changes.push(PacketShapeChange {
                    state,
                    bound,
                    packet_name,
                    kind: PacketShapeChangeKind::Changed,
                });
            }
            _ => {}
        }
    }
    Ok(changes)
}

fn minecraft_data_packet_shapes(
    json: &str,
) -> Result<BTreeMap<(PacketState, PacketBound, String), String>> {
    let root: Value = serde_json::from_str(json).context("parse minecraft-data protocol.json")?;
    let root = root
        .as_object()
        .ok_or_else(|| anyhow!("protocol.json root must be an object"))?;
    let mut shapes = BTreeMap::new();

    for state in PacketState::ALL {
        let state_key = minecraft_data_state_key(state);
        let Some(state_value) = root.get(state_key) else {
            continue;
        };
        let state_object = state_value
            .as_object()
            .ok_or_else(|| anyhow!("protocol state {state_key} must be an object"))?;
        for bound in PacketBound::ALL {
            let bound_key = minecraft_data_bound_key(bound);
            let Some(bound_value) = state_object.get(bound_key) else {
                continue;
            };
            let types = bound_value
                .get("types")
                .and_then(Value::as_object)
                .ok_or_else(|| anyhow!("{state_key}/{bound_key} is missing types object"))?;
            let mappings = minecraft_data_packet_mappings(bound_value)
                .with_context(|| format!("read packet mappings for {state_key}/{bound_key}"))?;
            for (_, name) in mappings {
                let namespaced = format!("minecraft:{name}");
                let type_key = format!("packet_{name}");
                let shape = types
                    .get(&type_key)
                    .map(canonical_json)
                    .unwrap_or_else(|| format!("<missing {type_key}>"));
                shapes.insert((state, bound, namespaced), shape);
            }
        }
    }
    Ok(shapes)
}

fn canonical_json(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut out = String::from("{");
            for (index, (key, value)) in sorted_object_entries(map).into_iter().enumerate() {
                if index != 0 {
                    out.push(',');
                }
                let _ = write!(out, "{key:?}:{}", canonical_json(value));
            }
            out.push('}');
            out
        }
        Value::Array(values) => {
            let mut out = String::from("[");
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    out.push(',');
                }
                out.push_str(&canonical_json(value));
            }
            out.push(']');
            out
        }
        other => other.to_string(),
    }
}

/// Extracts the `{ hex_id: name }` mapping from a `<state>.<bound>` section.
fn minecraft_data_packet_mappings(bound_value: &Value) -> Result<BTreeMap<String, String>> {
    let types = bound_value
        .get("types")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("packet direction is missing a `types` object"))?;
    let packet = types
        .get("packet")
        .ok_or_else(|| anyhow!("packet direction is missing a `packet` type"))?;

    // `packet` is `["container", [ { name: "name", type: ["mapper", {..}] }, .. ]]`.
    let fields = packet
        .as_array()
        .and_then(|entry| entry.get(1))
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("`packet` type is not a container"))?;

    for field in fields {
        if field.get("name").and_then(Value::as_str) != Some("name") {
            continue;
        }
        let mapper = field
            .get("type")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("packet `name` field has no type array"))?;
        if mapper.first().and_then(Value::as_str) != Some("mapper") {
            bail!("packet `name` field is not a mapper");
        }
        let mappings = mapper
            .get(1)
            .and_then(|options| options.get("mappings"))
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow!("packet `name` mapper has no mappings"))?;

        return Ok(mappings
            .iter()
            .filter_map(|(id, name)| Some((id.clone(), name.as_str()?.to_owned())))
            .collect());
    }

    // A direction with no packets (e.g. handshaking toClient) is legitimately empty.
    Ok(BTreeMap::new())
}

/// Parses a `minecraft-data` hex id such as `"0x00"` or `"0xfe"`.
pub(crate) fn parse_hex_packet_id(hex_id: &str) -> Result<i32> {
    let digits = hex_id
        .strip_prefix("0x")
        .or_else(|| hex_id.strip_prefix("0X"))
        .unwrap_or(hex_id);
    i32::from_str_radix(digits, 16).with_context(|| format!("invalid hex packet id {hex_id:?}"))
}

#[must_use]
pub fn sanitize_packet_const_name(name: &str) -> String {
    let raw_name = name.strip_prefix("minecraft:").unwrap_or(name);
    let mut ident = String::with_capacity(raw_name.len());

    for ch in raw_name.chars() {
        if ch.is_ascii_alphanumeric() {
            ident.push(ch.to_ascii_uppercase());
        } else {
            ident.push('_');
        }
    }

    if ident.is_empty() {
        ident.push_str("PACKET");
    }

    if ident.as_bytes()[0].is_ascii_digit() {
        ident.insert(0, '_');
    }

    ident
}

pub fn generate_packet_ids_source(report: &PacketReport) -> Result<String> {
    ensure_unique_generated_identifiers(report)?;

    let mut source = String::new();
    writeln!(
        source,
        "// @generated by `cargo xtask gen-packet-ids` from Minecraft {} (protocol {}). DO NOT EDIT.",
        report.minecraft_version, report.protocol_version
    )?;
    source.push('\n');
    writeln!(
        source,
        "pub const PROTOCOL_VERSION: i32 = {};",
        report.protocol_version
    )?;
    writeln!(
        source,
        "pub const MINECRAFT_VERSION: &str = {:?};",
        report.minecraft_version
    )?;
    source.push('\n');
    // Written from `PacketState`/`PacketBound`'s own `code_const`/`code_value`
    // rather than transcribed literals, so this table and the `match` arms
    // that define "handshaking is state 0" cannot drift apart -- they are
    // now one source of truth instead of two hand-kept in sync by eye.
    for state in PacketState::ALL {
        writeln!(
            source,
            "pub const {}: u8 = {};",
            state.code_const(),
            state.code_value()
        )?;
    }
    source.push('\n');
    for bound in PacketBound::ALL {
        writeln!(
            source,
            "pub const {}: u8 = {};",
            bound.code_const(),
            bound.code_value()
        )?;
    }

    for state in PacketState::ALL {
        writeln!(source, "\npub mod {} {{", state.module_name())?;
        for bound in PacketBound::ALL {
            writeln!(source, "    pub mod {} {{", bound.module_name())?;
            let entries: Vec<&PacketEntry> = report.entries(state, bound).collect();
            for entry in &entries {
                writeln!(
                    source,
                    "        pub const {}: i32 = {};",
                    entry.const_ident, entry.protocol_id
                )?;
            }
            if entries.is_empty() {
                source.push_str("        pub static ENTRIES: &[(&str, i32)] = &[];\n");
            } else {
                source.push('\n');
                source.push_str("        pub static ENTRIES: &[(&str, i32)] = &[\n");
                for entry in &entries {
                    writeln!(
                        source,
                        "            ({:?}, {}),",
                        entry.name, entry.const_ident
                    )?;
                }
                source.push_str("        ];\n");
            }
            source.push_str("    }\n");
        }
        source.push_str("}\n");
    }

    // The canonical-name join column deliverable 3 adds: for every entry
    // whose canonical (Mojang) name is known -- always true for a
    // Mojang-sourced report, only for a verified alias on a
    // minecraft-data-sourced one -- record the (source name, canonical name)
    // pair so a later stage can join a legacy table against v770's without
    // guessing at spelling. Self-referential pairs (a Mojang-sourced report
    // naming itself) are included too, so the table has one uniform shape
    // regardless of source.
    let canonical_pairs: Vec<(&str, &str)> = report
        .all_entries()
        .iter()
        .filter_map(|entry| {
            entry
                .canonical_name
                .as_deref()
                .map(|canonical| (entry.name.as_str(), canonical))
        })
        .collect();
    source.push('\n');
    if canonical_pairs.is_empty() {
        source.push_str("pub static CANONICAL_NAMES: &[(&str, &str)] = &[];\n");
    } else {
        source.push_str("pub static CANONICAL_NAMES: &[(&str, &str)] = &[\n");
        for (name, canonical) in &canonical_pairs {
            writeln!(source, "    ({name:?}, {canonical:?}),")?;
        }
        source.push_str("];\n");
    }

    source.push_str(
        "\npub fn id_for(state: u8, bound: u8, name: &str) -> Option<i32> {\n    match (state, bound, name) {\n",
    );
    for state in PacketState::ALL {
        for bound in PacketBound::ALL {
            for entry in report.entries(state, bound) {
                writeln!(
                    source,
                    "        ({}, {}, {:?}) => Some({}::{}::{}),",
                    state.code_const(),
                    bound.code_const(),
                    entry.name,
                    state.module_name(),
                    bound.module_name(),
                    entry.const_ident
                )?;
            }
        }
    }
    source.push_str("        _ => None,\n    }\n}\n");

    source.push_str(
        "\npub fn name_for(state: u8, bound: u8, id: i32) -> Option<&'static str> {\n    let entries = entries_for(state, bound)?;\n    entries\n        .binary_search_by_key(&id, |&(_, protocol_id)| protocol_id)\n        .ok()\n        .map(|index| entries[index].0)\n}\n",
    );

    source.push_str(
        "\nfn entries_for(state: u8, bound: u8) -> Option<&'static [(&'static str, i32)]> {\n    match (state, bound) {\n",
    );
    for state in PacketState::ALL {
        for bound in PacketBound::ALL {
            writeln!(
                source,
                "        ({}, {}) => Some({}::{}::ENTRIES),",
                state.code_const(),
                bound.code_const(),
                state.module_name(),
                bound.module_name()
            )?;
        }
    }
    source.push_str("        _ => None,\n    }\n}\n");

    format_rust_source(&source)
}

pub(crate) fn format_rust_source(source: &str) -> Result<String> {
    let mut child = std::process::Command::new("rustfmt")
        .arg("--edition")
        .arg("2024")
        .arg("--emit")
        .arg("stdout")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawn rustfmt to format generated packet IDs")?;

    child
        .stdin
        .as_mut()
        .expect("rustfmt stdin is piped")
        .write_all(source.as_bytes())
        .context("write generated packet IDs to rustfmt")?;

    let output = child
        .wait_with_output()
        .context("wait for rustfmt to format generated packet IDs")?;
    if !output.status.success() {
        bail!(
            "rustfmt failed while formatting generated packet IDs: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    String::from_utf8(output.stdout).context("rustfmt emitted non-UTF-8 output")
}
