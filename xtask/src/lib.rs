use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use sha1::Digest;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fmt::Write as _,
    fs::File,
    io::{Read as _, Write as _},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
};

pub const DEFAULT_PACKET_IDS_OUT: &str = "crates/protocol/v770/src/generated/packet_ids.rs";
/// Default output for the minecraft-data-sourced protocol 47 (Minecraft 1.8.x).
pub const DEFAULT_PACKET_IDS_OUT_V47: &str = "crates/protocol/v47/src/generated/packet_ids.rs";
pub const DEFAULT_CONNECTED_ALLOWLIST: &str = "xtask/check-connected.toml";

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
                entries.push(PacketEntry {
                    state,
                    bound,
                    const_ident: sanitize_packet_const_name(&namespaced),
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
    fn render(&self) -> String {
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
    let protocol_dir = workspace_root.join("crates/protocol");
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

fn shape_review_violations(family: &str, review_path: &Path) -> Result<Vec<String>> {
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
fn parse_hex_packet_id(hex_id: &str) -> Result<i32> {
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
    source.push_str("pub const STATE_HANDSHAKING: u8 = 0;\n");
    source.push_str("pub const STATE_STATUS: u8 = 1;\n");
    source.push_str("pub const STATE_LOGIN: u8 = 2;\n");
    source.push_str("pub const STATE_CONFIGURATION: u8 = 3;\n");
    source.push_str("pub const STATE_PLAY: u8 = 4;\n");
    source.push('\n');
    source.push_str("pub const BOUND_CLIENTBOUND: u8 = 0;\n");
    source.push_str("pub const BOUND_SERVERBOUND: u8 = 1;\n");

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

fn format_rust_source(source: &str) -> Result<String> {
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CliCommand {
    Help,
    GenPacketIds {
        minecraft_version: String,
        protocol_version: i32,
        check: bool,
        out: Option<PathBuf>,
        source: PacketSource,
    },
    FetchAssets {
        minecraft_version: String,
        force: bool,
    },
    FetchVersion {
        minecraft_version: String,
        force: bool,
    },
    GenRegistries {
        options: GenRegistriesOptions,
    },
    CheckIsolation,
    CheckConnected {
        allowlist: PathBuf,
    },
    CheckDeletable {
        version: String,
    },
    CodegenRatio,
    NewVersion {
        options: NewVersionOptions,
    },
    Conformance {
        options: ConformanceOptions,
    },
    Planned {
        name: &'static str,
    },
}

#[must_use]
pub const fn root_help() -> &'static str {
    "xtask\n\nUsage:\n    cargo run -p xtask -- <command> [options]\n\nCommands:\n    gen-packet-ids   Generate Rust packet ID tables from a Mojang report or minecraft-data\n    fetch-assets     Download and verify vanilla client.jar plus asset index into .cache/mc/<version>/\n    fetch-version    Download and verify vanilla server.jar into .cache/mc/<version>/\n    gen-registries   Generate selected registry id->ResourceKey tables from registries.json\n    check-isolation  Enforce protocol version crate dependency isolation\n    check-connected  Enforce workspace crates are reachable from shipped binary/cdylib roots\n    check-deletable  Simulate deleting a version family's folder and report the fallout\n    codegen-ratio    Report generated-vs-hand-written codec metrics per protocol family\n    new-version      Scaffold a protocol family; registry support is withheld until SHAPE_REVIEW.toml is discharged\n    gen-reports      Not implemented yet\n    conformance      Run packet-id, registry, isolation, deletability, test, and clippy checks for a family\n\nOptions for gen-packet-ids:\n    --version <version>   Minecraft version, e.g. 26.2 (Mojang) or 1.8 (minecraft-data dir)\n    --protocol <id>       Protocol version, e.g. 776 or 47\n    --source <source>     Report source: mojang (default) or minecraft-data\n    --out <path>          Output path under crates/protocol/*/src/generated/\n    --check               Compare generated output against disk and fail on drift without writing\n\nOptions for gen-registries:\n    --version <version>       Minecraft version, e.g. 26.2\n    --protocol <id>           Protocol version, e.g. 776\n    --out-dir <path>          Output directory under crates/protocol/*/src/generated\n    --registries <csv>        Registry keys to generate (default: sound_event,particle_type,menu,item)\n    --check                   Compare generated registry tables against disk without writing\n\nOptions for check-connected:\n    --allowlist <path>    TOML file of explicit exceptions (default: xtask/check-connected.toml)\n\nOptions for check-deletable:\n    <version>             Version family to simulate deleting: package name (lodestone-v47), folder (v47), or path\n\nOptions for codegen-ratio:\n    Reports both the optimistic per-struct derive/manual ratio and the more decision-useful absolute hand-written source lines.\n\nOptions for new-version:\n    --protocol <id>       Protocol number for the new family (required)\n    --minecraft <ver>    Minecraft version key for the packet-id oracle (required)\n    --from <family>       Existing family to copy from, e.g. v770 (default) or v47\n    --source <source>     Oracle: mojang or minecraft-data (default inferred from --from)\n    --name <vNNN>         Family folder/label (default v<protocol>)\n    --force               Overwrite the target folder if it already exists\n    SHAPE_REVIEW.toml     Generated when packet shapes differ; every entry must be reviewed before registry support may be added\n\nOptions for conformance:\n    --family <vNNN>       Version family folder/label to check, e.g. v735\n    --minecraft <ver>     Minecraft version key for packet-id/registry checks\n    --protocol <id>       Protocol number for the family\n    --source <source>     Packet-id oracle: mojang or minecraft-data (default mojang)\n    --skip-cargo          Only run xtask structural checks; skip cargo test/clippy\n\nOptions for fetch-version:\n    --version <version>   Minecraft version, e.g. 1.16.5\n    --force               Re-download even when cached server.jar already matches its SHA-1\n\nOptions for fetch-assets:\n    --version <version>   Minecraft version, e.g. 26.2\n    --force               Re-download even when cached files already match their SHA-1\n    -h, --help            Print help\n"
}

pub fn parse_cli_args<I, S>(args: I) -> Result<CliCommand>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let args: Vec<String> = args
        .into_iter()
        .map(|arg| arg.as_ref().to_owned())
        .collect();
    let Some(command) = args.first().map(String::as_str) else {
        return Ok(CliCommand::Help);
    };

    match command {
        "-h" | "--help" | "help" => Ok(CliCommand::Help),
        "gen-packet-ids" => parse_gen_packet_ids_args(&args[1..]),
        "fetch-assets" => parse_fetch_assets_args(&args[1..]),
        "gen-registries" => parse_gen_registries_args(&args[1..]),
        "check-isolation" => Ok(CliCommand::CheckIsolation),
        "check-connected" => parse_check_connected_args(&args[1..]),
        "check-deletable" => parse_check_deletable_args(&args[1..]),
        "codegen-ratio" => Ok(CliCommand::CodegenRatio),
        "new-version" => parse_new_version_args(&args[1..]),
        "fetch-version" => parse_fetch_version_args(&args[1..]),
        "conformance" => parse_conformance_args(&args[1..]),
        "gen-reports" => Ok(CliCommand::Planned {
            name: planned_command_name(command).expect("matched planned command has a name"),
        }),
        unknown => bail!("unknown xtask command {unknown:?}\n\n{}", root_help()),
    }
}

pub fn run_cli_command(command: CliCommand) -> Result<()> {
    match command {
        CliCommand::Help => {
            print!("{}", root_help());
            Ok(())
        }
        CliCommand::GenPacketIds {
            minecraft_version,
            protocol_version,
            check,
            out,
            source,
        } => {
            let workspace_root =
                std::env::current_dir().context("determine current workspace directory")?;
            if check {
                let check = check_packet_ids(
                    &workspace_root,
                    &minecraft_version,
                    protocol_version,
                    out.as_deref(),
                    source,
                )?;
                if !check.is_identical() {
                    bail!("{}", check.summary);
                }
                println!("{} is up to date", check.out_path.display());
            } else {
                let path = generate_packet_ids(
                    &workspace_root,
                    &minecraft_version,
                    protocol_version,
                    out.as_deref(),
                    source,
                )?;
                println!("generated {}", path.display());
            }
            Ok(())
        }
        CliCommand::FetchAssets {
            minecraft_version,
            force,
        } => {
            let workspace_root =
                std::env::current_dir().context("determine current workspace directory")?;
            let summary = fetch_assets(&workspace_root, &minecraft_version, force)?;
            print!("{}", summary.render());
            Ok(())
        }
        CliCommand::FetchVersion {
            minecraft_version,
            force,
        } => {
            let workspace_root =
                std::env::current_dir().context("determine current workspace directory")?;
            let summary = fetch_version(&workspace_root, &minecraft_version, force)?;
            println!("{}", summary.render());
            Ok(())
        }
        CliCommand::GenRegistries { options } => {
            let workspace_root =
                std::env::current_dir().context("determine current workspace directory")?;
            if options.check {
                check_registries(&workspace_root, &options)?;
                println!("generated registry tables are up to date");
            } else {
                let paths = generate_registries(&workspace_root, &options)?;
                for path in paths {
                    println!("generated {}", path.display());
                }
            }
            Ok(())
        }
        CliCommand::CheckIsolation => {
            let workspace_root =
                std::env::current_dir().context("determine current workspace directory")?;
            let report = check_workspace_isolation(&workspace_root)?;
            if let Some(infos) = report.info_summary() {
                println!("{infos}");
            }
            if let Some(warnings) = report.warning_summary() {
                eprintln!("{warnings}");
            }
            if report.has_violations() {
                bail!("{}", report.violation_summary());
            }
            println!("protocol version crate isolation check passed");
            Ok(())
        }
        CliCommand::CheckConnected { allowlist } => {
            let workspace_root =
                std::env::current_dir().context("determine current workspace directory")?;
            let report = check_workspace_connected_with_allowlist(&workspace_root, &allowlist)?;
            if report.has_violations() {
                bail!("{}", report.violation_summary());
            }
            println!("{}", report.success_summary());
            Ok(())
        }
        CliCommand::CheckDeletable { version } => {
            let workspace_root =
                std::env::current_dir().context("determine current workspace directory")?;
            let report = check_workspace_deletable(&workspace_root, &version)?;
            println!("{}", report.render());
            if !report.is_cleanly_deletable() {
                bail!(
                    "{} is not cleanly deletable: {} blocking dependency(ies) would break the build",
                    report.target_crate,
                    report.blockers.len()
                );
            }
            Ok(())
        }
        CliCommand::CodegenRatio => {
            let workspace_root =
                std::env::current_dir().context("determine current workspace directory")?;
            println!("{}", codegen_ratio_report(&workspace_root)?.render());
            Ok(())
        }
        CliCommand::NewVersion { options } => {
            let workspace_root =
                std::env::current_dir().context("determine current workspace directory")?;
            let report = scaffold_new_version(&workspace_root, &options)?;
            println!("{}", report.render());
            Ok(())
        }
        CliCommand::Conformance { options } => {
            let workspace_root =
                std::env::current_dir().context("determine current workspace directory")?;
            let report = run_conformance(&workspace_root, &options)?;
            println!("{}", report.render());
            Ok(())
        }
        CliCommand::Planned { name } => bail!("xtask command {name:?} is not implemented yet"),
    }
}

fn parse_gen_packet_ids_args(args: &[String]) -> Result<CliCommand> {
    let mut minecraft_version = None;
    let mut protocol_version = None;
    let mut check = false;
    let mut out = None;
    let mut source = PacketSource::Mojang;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => return Ok(CliCommand::Help),
            "--check" => {
                check = true;
            }
            "--version" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow!("--version requires a value"))?;
                minecraft_version = Some(value.clone());
            }
            "--protocol" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow!("--protocol requires a value"))?;
                protocol_version = Some(
                    value
                        .parse::<i32>()
                        .with_context(|| format!("parse protocol version {value:?}"))?,
                );
            }
            "--source" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow!("--source requires a value"))?;
                source = match value.as_str() {
                    "mojang" => PacketSource::Mojang,
                    "minecraft-data" => PacketSource::MinecraftData,
                    other => bail!(
                        "unknown packet source {other:?}; expected \"mojang\" or \"minecraft-data\""
                    ),
                };
            }
            "--out" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow!("--out requires a value"))?;
                out = Some(PathBuf::from(value));
            }
            unknown => bail!("unknown gen-packet-ids option {unknown:?}"),
        }
        index += 1;
    }

    Ok(CliCommand::GenPacketIds {
        minecraft_version: minecraft_version.ok_or_else(|| anyhow!("--version is required"))?,
        protocol_version: protocol_version.ok_or_else(|| anyhow!("--protocol is required"))?,
        check,
        out,
        source,
    })
}

fn parse_fetch_assets_args(args: &[String]) -> Result<CliCommand> {
    let mut minecraft_version = None;
    let mut force = false;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => return Ok(CliCommand::Help),
            "--force" => force = true,
            "--version" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow!("--version requires a value"))?;
                minecraft_version = Some(value.clone());
            }
            unknown => bail!("unknown fetch-assets option {unknown:?}"),
        }
        index += 1;
    }

    Ok(CliCommand::FetchAssets {
        minecraft_version: minecraft_version.ok_or_else(|| anyhow!("--version is required"))?,
        force,
    })
}

fn parse_fetch_version_args(args: &[String]) -> Result<CliCommand> {
    let mut minecraft_version = None;
    let mut force = false;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => return Ok(CliCommand::Help),
            "--force" => force = true,
            "--version" => {
                index += 1;
                minecraft_version = Some(
                    args.get(index)
                        .ok_or_else(|| anyhow!("--version requires a value"))?
                        .clone(),
                );
            }
            unknown => bail!("unknown fetch-version option {unknown:?}"),
        }
        index += 1;
    }

    Ok(CliCommand::FetchVersion {
        minecraft_version: minecraft_version.ok_or_else(|| anyhow!("--version is required"))?,
        force,
    })
}

fn parse_gen_registries_args(args: &[String]) -> Result<CliCommand> {
    let mut minecraft_version = None;
    let mut protocol_version = None;
    let mut check = false;
    let mut out_dir = PathBuf::from("crates/protocol/v770/src/generated");
    let mut registries: Option<Vec<String>> = None;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => return Ok(CliCommand::Help),
            "--check" => {
                check = true;
            }
            "--version" => {
                index += 1;
                minecraft_version = Some(
                    args.get(index)
                        .ok_or_else(|| anyhow!("--version requires a value"))?
                        .clone(),
                );
            }
            "--protocol" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow!("--protocol requires a value"))?;
                protocol_version = Some(
                    value
                        .parse::<i32>()
                        .with_context(|| format!("parse protocol version {value:?}"))?,
                );
            }
            "--out-dir" => {
                index += 1;
                out_dir = PathBuf::from(
                    args.get(index)
                        .ok_or_else(|| anyhow!("--out-dir requires a value"))?,
                );
            }
            "--registries" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow!("--registries requires a value"))?;
                registries = Some(
                    value
                        .split(',')
                        .map(str::trim)
                        .filter(|part| !part.is_empty())
                        .map(normalize_registry_key)
                        .collect(),
                );
            }
            unknown => bail!("unknown gen-registries option {unknown:?}"),
        }
        index += 1;
    }

    Ok(CliCommand::GenRegistries {
        options: GenRegistriesOptions {
            minecraft_version: minecraft_version.ok_or_else(|| anyhow!("--version is required"))?,
            protocol_version: protocol_version.ok_or_else(|| anyhow!("--protocol is required"))?,
            check,
            out_dir,
            registries: registries.unwrap_or_else(|| {
                default_registry_specs()
                    .iter()
                    .map(|spec| spec.registry_key.to_owned())
                    .collect()
            }),
        },
    })
}

fn normalize_registry_key(key: &str) -> String {
    if key.contains(':') {
        key.to_owned()
    } else {
        format!("minecraft:{key}")
    }
}

fn parse_check_connected_args(args: &[String]) -> Result<CliCommand> {
    let mut allowlist = PathBuf::from(DEFAULT_CONNECTED_ALLOWLIST);
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => return Ok(CliCommand::Help),
            "--allowlist" => {
                index += 1;
                allowlist = PathBuf::from(
                    args.get(index)
                        .ok_or_else(|| anyhow!("--allowlist requires a value"))?,
                );
            }
            unknown => bail!("unknown check-connected option {unknown:?}"),
        }
        index += 1;
    }
    Ok(CliCommand::CheckConnected { allowlist })
}

fn parse_check_deletable_args(args: &[String]) -> Result<CliCommand> {
    let mut version = None;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => return Ok(CliCommand::Help),
            "--version" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow!("--version requires a value"))?;
                version = Some(value.clone());
            }
            value if !value.starts_with('-') && version.is_none() => {
                version = Some(value.to_owned());
            }
            unknown => bail!("unknown check-deletable option {unknown:?}"),
        }
        index += 1;
    }

    Ok(CliCommand::CheckDeletable {
        version: version.ok_or_else(|| {
            anyhow!("check-deletable requires a version, e.g. `cargo xtask check-deletable v47`")
        })?,
    })
}

fn parse_new_version_args(args: &[String]) -> Result<CliCommand> {
    let mut name = None;
    let mut protocol = None;
    let mut minecraft_version = None;
    let mut source = None;
    let mut from = None;
    let mut force = false;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => return Ok(CliCommand::Help),
            "--force" => force = true,
            "--name" => {
                index += 1;
                name = Some(
                    args.get(index)
                        .ok_or_else(|| anyhow!("--name requires a value"))?
                        .clone(),
                );
            }
            "--protocol" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow!("--protocol requires a value"))?;
                protocol = Some(
                    value
                        .parse::<i32>()
                        .with_context(|| format!("parse protocol version {value:?}"))?,
                );
            }
            "--minecraft" | "--version" => {
                index += 1;
                minecraft_version = Some(
                    args.get(index)
                        .ok_or_else(|| anyhow!("--minecraft requires a value"))?
                        .clone(),
                );
            }
            "--source" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow!("--source requires a value"))?;
                source = Some(match value.as_str() {
                    "mojang" => PacketSource::Mojang,
                    "minecraft-data" => PacketSource::MinecraftData,
                    other => bail!(
                        "unknown packet source {other:?}; expected \"mojang\" or \"minecraft-data\""
                    ),
                });
            }
            "--from" => {
                index += 1;
                from = Some(
                    args.get(index)
                        .ok_or_else(|| anyhow!("--from requires a value"))?
                        .clone(),
                );
            }
            unknown => bail!("unknown new-version option {unknown:?}"),
        }
        index += 1;
    }

    let protocol = protocol.ok_or_else(|| anyhow!("--protocol is required"))?;
    let minecraft_version =
        minecraft_version.ok_or_else(|| anyhow!("--minecraft is required (oracle lookup key)"))?;
    let from = from.unwrap_or_else(|| "v770".to_owned());
    // Default the source from the family we copy: the legacy `v47` family is fed
    // by minecraft-data, everything modern by Mojang's report.
    let source = source.unwrap_or(if from == "v47" {
        PacketSource::MinecraftData
    } else {
        PacketSource::Mojang
    });
    let name = name.unwrap_or_else(|| format!("v{protocol}"));

    Ok(CliCommand::NewVersion {
        options: NewVersionOptions {
            name,
            protocol,
            minecraft_version,
            source,
            from,
            force,
        },
    })
}

fn parse_conformance_args(args: &[String]) -> Result<CliCommand> {
    let mut family = None;
    let mut minecraft_version = None;
    let mut protocol_version = None;
    let mut source = PacketSource::Mojang;
    let mut skip_cargo = false;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => return Ok(CliCommand::Help),
            "--family" => {
                index += 1;
                family = Some(
                    args.get(index)
                        .ok_or_else(|| anyhow!("--family requires a value"))?
                        .clone(),
                );
            }
            "--minecraft" | "--version" => {
                index += 1;
                minecraft_version = Some(
                    args.get(index)
                        .ok_or_else(|| anyhow!("--minecraft requires a value"))?
                        .clone(),
                );
            }
            "--protocol" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow!("--protocol requires a value"))?;
                protocol_version = Some(
                    value
                        .parse::<i32>()
                        .with_context(|| format!("parse protocol version {value:?}"))?,
                );
            }
            "--source" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow!("--source requires a value"))?;
                source = match value.as_str() {
                    "mojang" => PacketSource::Mojang,
                    "minecraft-data" => PacketSource::MinecraftData,
                    other => bail!(
                        "unknown packet source {other:?}; expected \"mojang\" or \"minecraft-data\""
                    ),
                };
            }
            "--skip-cargo" => {
                skip_cargo = true;
            }
            unknown => bail!("unknown conformance option {unknown:?}"),
        }
        index += 1;
    }

    Ok(CliCommand::Conformance {
        options: ConformanceOptions {
            family: family.ok_or_else(|| anyhow!("--family is required"))?,
            minecraft_version: minecraft_version
                .ok_or_else(|| anyhow!("--minecraft is required"))?,
            protocol_version: protocol_version.ok_or_else(|| anyhow!("--protocol is required"))?,
            source,
            skip_cargo,
        },
    })
}

fn planned_command_name(command: &str) -> Option<&'static str> {
    match command {
        "fetch-version" => Some("fetch-version"),
        "gen-reports" => Some("gen-reports"),
        "conformance" => Some("conformance"),
        _ => None,
    }
}

/// Loads and parses a packet report from the configured [`PacketSource`].
fn load_packet_report(
    workspace_root: &Path,
    minecraft_version: &str,
    protocol_version: i32,
    source: PacketSource,
) -> Result<PacketReport> {
    match source {
        PacketSource::Mojang => {
            let report_path = workspace_root
                .join(".cache")
                .join("mc")
                .join(minecraft_version)
                .join("generated")
                .join("reports")
                .join("packets.json");
            let json = std::fs::read_to_string(&report_path)
                .with_context(|| format!("read packet report at {}", report_path.display()))?;
            parse_packet_report(&json, minecraft_version, protocol_version)
        }
        PacketSource::MinecraftData => {
            let protocol = load_minecraft_data_protocol_json(
                workspace_root,
                minecraft_version,
                protocol_version,
            )?;
            parse_minecraft_data_report(
                &protocol.json,
                protocol.minecraft_version,
                protocol_version,
            )
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MinecraftDataProtocolJson {
    pub json: String,
    pub minecraft_version: String,
    pub protocol_data_version: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct MinecraftDataVersionInfo {
    minecraft_version: String,
    protocol_version: i32,
    major_version: String,
}

fn load_minecraft_data_protocol_json(
    workspace_root: &Path,
    minecraft_version: &str,
    protocol_version: i32,
) -> Result<MinecraftDataProtocolJson> {
    let pc_dir = workspace_root
        .join("vendor")
        .join("minecraft-data")
        .join("data")
        .join("pc");
    let requested_dir = pc_dir.join(minecraft_version);
    let requested = minecraft_data_version_info(&requested_dir, minecraft_version)?;
    if requested.protocol_version != protocol_version {
        bail!(
            "minecraft-data at {} declares protocol {} but --protocol is {}",
            requested_dir.join("version.json").display(),
            requested.protocol_version,
            protocol_version
        );
    }

    let protocol_dir = if requested_dir.join("protocol.json").is_file() {
        requested_dir.clone()
    } else {
        minecraft_data_fallback_protocol_dir(&pc_dir, &requested)?
    };
    let json = std::fs::read_to_string(protocol_dir.join("protocol.json")).with_context(|| {
        format!(
            "read minecraft-data protocol at {}",
            protocol_dir.join("protocol.json").display()
        )
    })?;
    let protocol_data_version =
        minecraft_data_version_info(&protocol_dir, minecraft_version)?.minecraft_version;
    Ok(MinecraftDataProtocolJson {
        json,
        minecraft_version: requested.minecraft_version,
        protocol_data_version,
    })
}

fn minecraft_data_fallback_protocol_dir(
    pc_dir: &Path,
    requested: &MinecraftDataVersionInfo,
) -> Result<PathBuf> {
    let mut candidates = Vec::new();
    for entry in std::fs::read_dir(pc_dir)
        .with_context(|| format!("read minecraft-data versions under {}", pc_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !path.join("protocol.json").is_file() || !path.join("version.json").is_file() {
            continue;
        }
        let info = minecraft_data_version_info(&path, "")?;
        if info.major_version == requested.major_version
            && info.protocol_version <= requested.protocol_version
        {
            candidates.push((info.protocol_version, path));
        }
    }
    candidates.sort_by_key(|(protocol, _)| *protocol);
    candidates
        .pop()
        .map(|(_, path)| path)
        .ok_or_else(|| {
            anyhow!(
                "minecraft-data has no protocol.json for {} and no same-major fallback with protocol <= {}",
                requested.minecraft_version,
                requested.protocol_version
            )
        })
}

/// Reads minecraft-data `version.json`, returning the precise Minecraft version
/// string and declared protocol number.
fn minecraft_data_version_info(
    dir: &Path,
    fallback_version: &str,
) -> Result<MinecraftDataVersionInfo> {
    let version_path = dir.join("version.json");
    let Ok(json) = std::fs::read_to_string(&version_path) else {
        return Ok(MinecraftDataVersionInfo {
            minecraft_version: fallback_version.to_owned(),
            protocol_version: -1,
            major_version: fallback_version.to_owned(),
        });
    };
    let value: Value = serde_json::from_str(&json).context("parse minecraft-data version.json")?;
    let minecraft_version = value
        .get("minecraftVersion")
        .and_then(Value::as_str)
        .unwrap_or(fallback_version)
        .to_owned();
    let protocol_version = value
        .get("version")
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(|| {
            anyhow!(
                "minecraft-data {} is missing integer version",
                version_path.display()
            )
        })?;
    let major_version = value
        .get("majorVersion")
        .and_then(Value::as_str)
        .unwrap_or(&minecraft_version)
        .to_owned();
    Ok(MinecraftDataVersionInfo {
        minecraft_version,
        protocol_version,
        major_version,
    })
}

/// Returns the default generated-file path for a [`PacketSource`].
const fn default_out_for_source(source: PacketSource) -> &'static str {
    match source {
        PacketSource::Mojang => DEFAULT_PACKET_IDS_OUT,
        PacketSource::MinecraftData => DEFAULT_PACKET_IDS_OUT_V47,
    }
}

pub fn generate_packet_ids(
    workspace_root: &Path,
    minecraft_version: &str,
    protocol_version: i32,
    out: Option<&Path>,
    source: PacketSource,
) -> Result<PathBuf> {
    let report = load_packet_report(workspace_root, minecraft_version, protocol_version, source)?;
    let generated = generate_packet_ids_source(&report)?;

    let out_path = resolve_output_path(workspace_root, out, default_out_for_source(source))?;
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create output directory {}", parent.display()))?;
    }
    std::fs::write(&out_path, generated)
        .with_context(|| format!("write generated packet ids to {}", out_path.display()))?;
    Ok(out_path)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PacketIdCheck {
    pub out_path: PathBuf,
    pub summary: String,
    identical: bool,
}

impl PacketIdCheck {
    #[must_use]
    pub const fn is_identical(&self) -> bool {
        self.identical
    }
}

pub fn check_packet_ids(
    workspace_root: &Path,
    minecraft_version: &str,
    protocol_version: i32,
    out: Option<&Path>,
    source: PacketSource,
) -> Result<PacketIdCheck> {
    let report = load_packet_report(workspace_root, minecraft_version, protocol_version, source)?;
    let expected = generate_packet_ids_source(&report)?;
    let out_path = resolve_output_path(workspace_root, out, default_out_for_source(source))?;
    let actual = std::fs::read_to_string(&out_path)
        .with_context(|| format!("read generated packet ids at {}", out_path.display()))?;

    if actual == expected {
        return Ok(PacketIdCheck {
            out_path,
            summary: "packet_ids.rs is up to date".to_owned(),
            identical: true,
        });
    }

    Ok(PacketIdCheck {
        summary: packet_id_diff_summary(&out_path, &expected, &actual),
        out_path,
        identical: false,
    })
}

fn packet_id_diff_summary(path: &Path, expected: &str, actual: &str) -> String {
    let expected_lines: Vec<&str> = expected.lines().collect();
    let actual_lines: Vec<&str> = actual.lines().collect();
    let max_len = expected_lines.len().max(actual_lines.len());

    for index in 0..max_len {
        let expected_line = expected_lines.get(index).copied().unwrap_or("<missing>");
        let actual_line = actual_lines.get(index).copied().unwrap_or("<missing>");
        if expected_line != actual_line {
            return format!(
                "{} is out of date: first difference at line {}\nexpected: {}\nactual:   {}",
                path.display(),
                index + 1,
                expected_line,
                actual_line
            );
        }
    }

    format!(
        "{} is out of date: generated contents differ",
        path.display()
    )
}

/// The result of an isolation check.
///
/// The lint exists to protect one concrete user requirement: **dropping support
/// for a version must mean deleting a single `crates/protocol/<version>` folder
/// and having it be mostly all gone.** Two dependency shapes break that promise,
/// and this report is expressed directly in those terms rather than in terms of
/// an allowlist of "blessed" shared crates (which rots every time a new
/// version-free crate such as `lodestone-world` is added):
///
/// 1. A version crate depending on **another version crate** — deleting either
///    folder would break the other. Always fatal ([`Severity::Violation`]).
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
/// registry -> version edge, and any version -> version edge, remain fatal, so
/// the role can never be abused to silence a build-breaking violation.
///
/// Whether a crate *is* a version crate is derived structurally from its
/// location under `crates/protocol/`, so a brand-new version family is covered
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
            "protocol version crate isolation violations found (a version must stay deletable as a single folder):",
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
    /// A version crate depends on another version crate.
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
                "version crates must never depend on another version crate, or deleting one version's folder would break the other"
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
        // under crates/protocol/, never by name, so a new version family is
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
                // Rule 1: version -> version is always fatal, regardless of
                // whether the edge is optional or dev-only, because either
                // folder's deletion would break the other.
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
                let is_soft = optional || dependency_table != "dependencies";
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
/// version must mean deleting a single `crates/protocol/<version>` folder and
/// having it be mostly all gone.** This report is the continuously-checkable
/// form of the manual deletion drill — it enumerates every crate that depends on
/// the target and classifies each edge as either a *blocker* (something that
/// would fail to compile and therefore breaks the "just delete the folder"
/// promise) or a *manual edit* (a one-line, feature-gated reference that is
/// expected to be removed alongside the folder).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeletabilityReport {
    /// The resolved package name, e.g. `lodestone-v47`.
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
/// `requested` may be the package name (`lodestone-v47`), the folder name
/// (`v47`), or a path under `crates/protocol/`. Dependency-graph edges catch
/// every crate that could reference the version in source (a crate can only
/// `use lodestone_v47` if it declares a dependency on it). Cargo *feature*
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
            {
                target = Some((package_name.to_owned(), dir));
            }
        }
        member_packages.push(package);
    }

    let (target_crate, target_dir) = target.ok_or_else(|| {
        anyhow!(
            "no version crate matched {requested:?}; expected a package name (lodestone-v47), folder (v47), or path under crates/protocol/"
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

/// Scans the designated version registry's source tree for lines that gate on
/// the family being deleted (a `#[cfg(feature = "v47")]` entry or a
/// `lodestone_v47::` path). These stay behind `#[cfg]` so they never break the
/// build, but the dead cfg emits an `unexpected_cfgs` warning once the feature
/// is gone. The registry is identified structurally by its metadata role, never
/// by name, so this cannot be pointed at an arbitrary crate.
fn registry_source_lines_mentioning(
    canonical_root: &Path,
    member_packages: &[&Value],
    target_crate: &str,
    target_dir: &str,
) -> Result<Vec<ManifestLine>> {
    let folder_token = Path::new(target_dir)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(target_dir);
    let snake_name = target_crate.replace('-', "_");
    let cfg_needle = format!("feature = \"{folder_token}\"");

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
/// (for example `crates/protocol/v47`).
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
    // The family folder token (e.g. `v47`) is how *feature* references name the
    // family, as in `live-v47 = ["lodestone-registry/v47"]`. Cargo validates
    // these feature strings at resolve time, so a dangling one breaks even the
    // default build — yet it is invisible to the dependency graph. We therefore
    // scan manifests for both the package name and this token.
    let folder_token = Path::new(target_dir)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(target_dir);
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
/// deleted, e.g. `live-v47 = ["lodestone-registry/v47"]`. Such references are
/// validated by Cargo at resolve time (a dangling one fails the whole build) but
/// are not dependency-graph edges, so they must be caught textually. Matches the
/// folder token only as a `/<token>` path segment ending at a feature-string
/// boundary, so `v47` never matches inside a longer token such as `v470`.
fn line_forwards_to_family_feature(line: &str, folder_token: &str) -> bool {
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodegenRatioReport {
    pub families: Vec<CodegenRatioFamily>,
}

impl CodegenRatioReport {
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::from(
            "protocol codegen ratio\n\
             note: the per-struct ratio is optimistic because one derive block can replace many lines, while adapter dispatch and bespoke codecs dominate hand-written source.\n\
             family  derive-blocks  manual-impls  struct-derived  generated-lines  hand-written-lines\n",
        );
        for family in &self.families {
            let _ = writeln!(
                out,
                "{:<7} {:>13} {:>13} {:>14} {:>16} {:>19}",
                family.family,
                family.derive_blocks,
                family.manual_impls,
                percent(
                    family.derive_blocks,
                    family.derive_blocks + family.manual_impls
                ),
                family.generated_lines,
                family.hand_written_lines,
            );
        }
        out
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodegenRatioFamily {
    pub family: String,
    pub derive_blocks: usize,
    pub manual_impls: usize,
    pub generated_lines: usize,
    pub hand_written_lines: usize,
}

pub fn codegen_ratio_report(workspace_root: &Path) -> Result<CodegenRatioReport> {
    let protocol_root = workspace_root.join("crates/protocol");
    let mut families = Vec::new();
    if !protocol_root.is_dir() {
        return Ok(CodegenRatioReport { families });
    }

    for entry in std::fs::read_dir(&protocol_root)
        .with_context(|| format!("read {}", protocol_root.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let family = entry.file_name().to_string_lossy().into_owned();
        let src = entry.path().join("src");
        if !src.is_dir() {
            continue;
        }
        let mut metrics = CodegenRatioFamily {
            family,
            derive_blocks: 0,
            manual_impls: 0,
            generated_lines: 0,
            hand_written_lines: 0,
        };
        collect_codegen_ratio_source_metrics(&src, &src, &mut metrics)?;
        families.push(metrics);
    }

    families.sort_by(|left, right| {
        natural_family_key(&left.family).cmp(&natural_family_key(&right.family))
    });
    Ok(CodegenRatioReport { families })
}

fn collect_codegen_ratio_source_metrics(
    src_root: &Path,
    dir: &Path,
    metrics: &mut CodegenRatioFamily,
) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect_codegen_ratio_source_metrics(src_root, &path, metrics)?;
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        let source =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let line_count = source.lines().count();
        if is_under_generated(src_root, &path) {
            metrics.generated_lines += line_count;
        } else {
            metrics.hand_written_lines += line_count;
            metrics.derive_blocks += count_codec_derive_blocks(&source);
            metrics.manual_impls += count_manual_codec_impls(&source);
        }
    }
    Ok(())
}

fn is_under_generated(src_root: &Path, path: &Path) -> bool {
    path.strip_prefix(src_root)
        .ok()
        .and_then(|relative| relative.components().next())
        .is_some_and(
            |component| matches!(component, Component::Normal(name) if name == "generated"),
        )
}

fn count_manual_codec_impls(source: &str) -> usize {
    source.matches("impl Encode for").count() + source.matches("impl Decode for").count()
}

fn count_codec_derive_blocks(source: &str) -> usize {
    let mut count = 0;
    let mut rest = source;
    while let Some(start) = rest.find("#[derive") {
        rest = &rest[start + "#[derive".len()..];
        let Some(end) = rest.find(")]") else {
            break;
        };
        let derive_body = &rest[..end];
        if derive_body
            .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
            .any(|token| token == "Encode" || token == "Decode")
        {
            count += 1;
        }
        rest = &rest[end + 2..];
    }
    count
}

fn percent(numerator: usize, denominator: usize) -> String {
    if denominator == 0 {
        "n/a".to_owned()
    } else {
        format!("{:.0}%", (numerator as f64 / denominator as f64) * 100.0)
    }
}

fn natural_family_key(family: &str) -> (u8, u32, &str) {
    if let Some(digits) = family.strip_prefix('v')
        && let Ok(value) = digits.parse::<u32>()
    {
        return (0, value, family);
    }
    (1, 0, family)
}

/// Options for scaffolding a new protocol version family (`xtask new-version`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewVersionOptions {
    /// Family label / folder name under `crates/protocol/`, e.g. `v340`.
    pub name: String,
    /// Protocol number the new family targets.
    pub protocol: i32,
    /// Minecraft version key used to locate the packet-id oracle.
    pub minecraft_version: String,
    /// Which oracle produces the generated packet ids.
    pub source: PacketSource,
    /// Existing family to copy the module skeleton from, e.g. `v770`.
    pub from: String,
    /// Overwrite the target folder if it already exists.
    pub force: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConformanceOptions {
    pub family: String,
    pub minecraft_version: String,
    pub protocol_version: i32,
    pub source: PacketSource,
    pub skip_cargo: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConformanceReport {
    pub family: String,
    pub steps: Vec<ConformanceStep>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConformanceStep {
    pub name: String,
    pub outcome: ConformanceOutcome,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConformanceOutcome {
    Passed,
    Skipped(String),
}

impl ConformanceReport {
    #[must_use]
    pub fn render(&self) -> String {
        let mut rendered = format!("conformance checks for {}:\n", self.family);
        for step in &self.steps {
            match &step.outcome {
                ConformanceOutcome::Passed => {
                    let _ = writeln!(rendered, "  PASS {}", step.name);
                }
                ConformanceOutcome::Skipped(reason) => {
                    let _ = writeln!(rendered, "  SKIP {}: {reason}", step.name);
                }
            }
        }
        rendered
    }
}

/// What `new-version` did, and — crucially — the residue a human must still
/// finish by hand.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewVersionReport {
    /// New family label.
    pub name: String,
    /// New crate package name.
    pub crate_name: String,
    /// Family copied from.
    pub from: String,
    /// Protocol number.
    pub protocol: i32,
    /// Files written into the new crate directory (workspace-relative).
    pub created_files: Vec<String>,
    /// Manifests/sources edited to wire the new family in (workspace-relative).
    pub wired_files: Vec<String>,
    /// Packet shapes that changed according to minecraft-data protocol.json.
    pub shape_changes: Vec<PacketShapeChange>,
    /// Manual follow-up items the scaffold cannot do for you.
    pub residue: Vec<String>,
}

impl NewVersionReport {
    /// A human-readable summary of the scaffold and its residue.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = format!(
            "scaffolded protocol family {} (crate {}, protocol {}) from {}\n",
            self.name, self.crate_name, self.protocol, self.from
        );
        let _ = write!(out, "\ncreated {} file(s):", self.created_files.len());
        for file in &self.created_files {
            let _ = write!(out, "\n  + {file}");
        }
        let _ = write!(
            out,
            "\n\nwired into {} existing file(s):",
            self.wired_files.len()
        );
        for file in &self.wired_files {
            let _ = write!(out, "\n  ~ {file}");
        }
        let _ = write!(
            out,
            "\n\npacket shape changes reported by oracle ({} item(s)):",
            self.shape_changes.len()
        );
        for change in self.shape_changes.iter().take(50) {
            let _ = write!(out, "\n  ? {}", change.render());
        }
        if self.shape_changes.len() > 50 {
            let _ = write!(
                out,
                "\n  ? ... {} more not shown",
                self.shape_changes.len() - 50
            );
        }
        let _ = write!(
            out,
            "\n\nresidue — finish these by hand ({} item(s)):",
            self.residue.len()
        );
        for item in &self.residue {
            let _ = write!(out, "\n  ! {item}");
        }
        out.push('\n');
        out
    }
}

/// Capitalises a family label's leading `v` to match the adapter-type prefix,
/// e.g. `v770` -> `V770`.
fn capitalize_family(token: &str) -> String {
    let mut chars = token.chars();
    match chars.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

/// Scaffolds a new protocol version family end to end: copies the skeleton from
/// `--from`, regenerates the packet-id table from the relevant oracle, sets the
/// protocol constant, and wires the family into the workspace and the registry.
pub fn scaffold_new_version(
    workspace_root: &Path,
    options: &NewVersionOptions,
) -> Result<NewVersionReport> {
    let from_token = options.from.as_str();
    let to_token = options.name.as_str();
    if from_token == to_token {
        bail!("--from and --name must differ (both are {to_token:?})");
    }

    let protocol_dir = workspace_root.join("crates/protocol");
    let from_dir = protocol_dir.join(from_token);
    if !from_dir.is_dir() {
        bail!(
            "--from family {from_token:?} not found at {}",
            from_dir.display()
        );
    }
    let target_dir = protocol_dir.join(to_token);
    if target_dir.exists() {
        if options.force {
            std::fs::remove_dir_all(&target_dir).with_context(|| {
                format!("remove existing target directory {}", target_dir.display())
            })?;
        } else {
            bail!(
                "target family {to_token:?} already exists at {} (pass --force to overwrite)",
                target_dir.display()
            );
        }
    }

    let from_cap = capitalize_family(from_token);
    let to_cap = capitalize_family(to_token);
    // Two case-sensitive rewrites carry every crate-identifier form: the lower
    // token covers `lodestone-v770`, `lodestone_v770`, `mod v770`, doc refs; the
    // capitalised one covers the adapter type prefix `V770Adapter`.
    let substitutions = [
        (from_token.to_owned(), to_token.to_owned()),
        (from_cap, to_cap),
    ];

    let mut created_files = Vec::new();
    copy_tree_with_substitutions(
        &from_dir,
        &target_dir,
        &substitutions,
        workspace_root,
        &mut created_files,
    )?;

    // Regenerate the packet-id table from the correct oracle, replacing the
    // copied-over table (which still described the source family).
    let relative_out = format!("crates/protocol/{to_token}/src/generated/packet_ids.rs");
    generate_packet_ids(
        workspace_root,
        &options.minecraft_version,
        options.protocol,
        Some(Path::new(&relative_out)),
        options.source,
    )
    .context("generate packet-id table for the new family")?;
    if !created_files.contains(&relative_out) {
        created_files.push(relative_out.clone());
    }

    // Set the protocol constant in the copied adapter.
    let adapter_path = target_dir.join("src/adapter.rs");
    let mut residue = Vec::new();
    let shape_review = new_version_shape_review(
        workspace_root,
        &from_dir,
        from_token,
        to_token,
        &options.minecraft_version,
        options.protocol,
        options.source,
    )
    .map(Some)
    .unwrap_or_else(|error| {
        residue.push(format!(
            "packet shape diff unavailable for {from_token}->{to_token}: {error}"
        ));
        None
    });
    let shape_changes = shape_review
        .as_ref()
        .map(|review| review.entries.clone())
        .unwrap_or_default();
    let block_registry_for_shape_review = shape_review.is_none() || !shape_changes.is_empty();
    if let Some(review) = &shape_review
        && !review.entries.is_empty()
    {
        write_shape_review_files(&target_dir, review, workspace_root, &mut created_files)?;
        residue.push(format!(
            "registry wiring skipped for {to_token}: {} packet shape review entr{} must be marked reviewed = true in crates/protocol/{to_token}/SHAPE_REVIEW.toml first",
            review.entries.len(),
            if review.entries.len() == 1 { "y" } else { "ies" }
        ));
    } else if shape_review.is_none() {
        residue.push(format!(
            "registry wiring skipped for {to_token}: packet shape diff is unavailable, so support cannot be advertised safely"
        ));
    }
    if adapter_path.is_file() {
        set_protocol_constant(&adapter_path, options.protocol)?;
    } else {
        residue.push(format!(
            "no src/adapter.rs found in {from_token}; set `pub const PROTOCOL` and the adapter by hand"
        ));
    }

    // Wire the family into the workspace. Registry support is withheld until
    // field-shape deltas have been explicitly reviewed.
    let mut wired_files = Vec::new();
    wire_workspace_dependency(workspace_root, to_token, &mut wired_files)?;
    if !block_registry_for_shape_review {
        wire_registry(
            workspace_root,
            to_token,
            options.protocol,
            &mut wired_files,
            &mut residue,
        )?;
    }

    created_files.sort();
    created_files.dedup();
    wired_files.sort();
    wired_files.dedup();

    // The unavoidable manual residue: everything semantic the scaffold cannot
    // know. Copying is explicitly acceptable here, so these are edits, not
    // re-abstractions.
    residue.push(format!(
        "review packet structs under crates/protocol/{to_token}/src/packets/ — they are {from_token}'s wire shapes; change the ones that differ for protocol {}",
        options.protocol
    ));
    residue.push(format!(
        "update `minecraft_versions()` and crate docs in crates/protocol/{to_token} to name {}",
        options.minecraft_version
    ));
    residue.push(format!(
        "update the login/play choreography in crates/protocol/{to_token}/src/adapter.rs if it differs from {from_token}"
    ));
    residue.push(format!(
        "run `cargo test -p lodestone-{to_token} && cargo clippy -p lodestone-{to_token} --all-targets` and fix fallout"
    ));

    Ok(NewVersionReport {
        name: to_token.to_owned(),
        crate_name: format!("lodestone-{to_token}"),
        from: from_token.to_owned(),
        protocol: options.protocol,
        created_files,
        wired_files,
        shape_changes,
        residue,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct GeneratedPacketIdsMetadata {
    minecraft_version: String,
    protocol_version: i32,
}

fn new_version_shape_review(
    workspace_root: &Path,
    from_dir: &Path,
    from_family: &str,
    target_family: &str,
    target_minecraft_version: &str,
    target_protocol: i32,
    source: PacketSource,
) -> Result<ShapeReviewManifest> {
    if source != PacketSource::MinecraftData {
        bail!(
            "Mojang packets.json contains packet ids only; use minecraft-data when field-shape diffing is required"
        );
    }
    let source_meta =
        parse_generated_packet_ids_metadata(&from_dir.join("src/generated/packet_ids.rs"))?;
    let source_protocol = load_minecraft_data_protocol_json(
        workspace_root,
        &source_meta.minecraft_version,
        source_meta.protocol_version,
    )?;
    let target_protocol_json = load_minecraft_data_protocol_json(
        workspace_root,
        target_minecraft_version,
        target_protocol,
    )?;
    let entries =
        compare_minecraft_data_packet_shapes(&source_protocol.json, &target_protocol_json.json)?;
    Ok(ShapeReviewManifest {
        source_family: from_family.to_owned(),
        target_family: target_family.to_owned(),
        source_minecraft_version: source_meta.minecraft_version,
        source_protocol_version: source_meta.protocol_version,
        target_minecraft_version: target_protocol_json.minecraft_version,
        target_protocol_version: target_protocol,
        entries,
    })
}

fn write_shape_review_files(
    target_dir: &Path,
    review: &ShapeReviewManifest,
    workspace_root: &Path,
    created_files: &mut Vec<String>,
) -> Result<()> {
    let review_path = target_dir.join("SHAPE_REVIEW.toml");
    std::fs::write(&review_path, render_shape_review_toml(review)?)
        .with_context(|| format!("write {}", review_path.display()))?;
    push_relative(created_files, workspace_root, &review_path);

    let tests_dir = target_dir.join("tests");
    std::fs::create_dir_all(&tests_dir)
        .with_context(|| format!("create {}", tests_dir.display()))?;
    let test_path = tests_dir.join("shape_review.rs");
    std::fs::write(&test_path, shape_review_test_source())
        .with_context(|| format!("write {}", test_path.display()))?;
    push_relative(created_files, workspace_root, &test_path);
    Ok(())
}

fn push_relative(files: &mut Vec<String>, workspace_root: &Path, path: &Path) {
    if let Ok(relative) = path.strip_prefix(workspace_root) {
        files.push(relative.to_string_lossy().into_owned());
    }
}

fn shape_review_test_source() -> &'static str {
    r#"const SHAPE_REVIEW: &str = include_str!("../SHAPE_REVIEW.toml");

#[test]
fn packet_shape_review_is_complete() {
    let mut current_packet = "<unknown packet>";
    for line in SHAPE_REVIEW.lines() {
        let trimmed = line.trim();
        if let Some(value) = trimmed.strip_prefix("name = ") {
            current_packet = value.trim_matches('"');
        } else if trimmed == "reviewed = false" {
            panic!(
                "packet shape review is incomplete for {current_packet}; audit the codec against this protocol, then set reviewed = true in SHAPE_REVIEW.toml"
            );
        }
    }
}
"#
}

fn parse_generated_packet_ids_metadata(path: &Path) -> Result<GeneratedPacketIdsMetadata> {
    let contents =
        std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let minecraft_version = contents
        .lines()
        .find_map(|line| line.strip_prefix("pub const MINECRAFT_VERSION: &str = \""))
        .and_then(|rest| rest.strip_suffix("\";"))
        .ok_or_else(|| anyhow!("{} is missing MINECRAFT_VERSION", path.display()))?
        .to_owned();
    let protocol_version = contents
        .lines()
        .find_map(|line| line.strip_prefix("pub const PROTOCOL_VERSION: i32 = "))
        .and_then(|rest| rest.strip_suffix(';'))
        .ok_or_else(|| anyhow!("{} is missing PROTOCOL_VERSION", path.display()))?
        .parse()
        .with_context(|| format!("parse PROTOCOL_VERSION in {}", path.display()))?;
    Ok(GeneratedPacketIdsMetadata {
        minecraft_version,
        protocol_version,
    })
}

/// Recursively copies `from_dir` to `target_dir`, applying textual
/// substitutions to every file's contents and skipping `target/` build output.
fn copy_tree_with_substitutions(
    from_dir: &Path,
    target_dir: &Path,
    substitutions: &[(String, String)],
    workspace_root: &Path,
    created: &mut Vec<String>,
) -> Result<()> {
    std::fs::create_dir_all(target_dir)
        .with_context(|| format!("create directory {}", target_dir.display()))?;
    for entry in std::fs::read_dir(from_dir)
        .with_context(|| format!("read directory {}", from_dir.display()))?
    {
        let entry = entry?;
        let file_name = entry.file_name();
        // Never copy build output.
        if file_name == "target" {
            continue;
        }
        let source = entry.path();
        let destination = target_dir.join(&file_name);
        if entry.file_type()?.is_dir() {
            copy_tree_with_substitutions(
                &source,
                &destination,
                substitutions,
                workspace_root,
                created,
            )?;
        } else {
            if is_live_test_file(&source) {
                continue;
            }
            let contents = std::fs::read_to_string(&source)
                .with_context(|| format!("read {}", source.display()))?;
            let mut rewritten = contents;
            for (needle, replacement) in substitutions {
                rewritten = rewritten.replace(needle, replacement);
            }
            std::fs::write(&destination, rewritten)
                .with_context(|| format!("write {}", destination.display()))?;
            if let Ok(relative) = destination.strip_prefix(workspace_root) {
                created.push(relative.to_string_lossy().into_owned());
            }
        }
    }
    Ok(())
}

fn is_live_test_file(path: &Path) -> bool {
    path.parent()
        .and_then(Path::file_name)
        .is_some_and(|name| name == "tests")
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("live_") && name.ends_with(".rs"))
}

/// Rewrites the `pub const PROTOCOL: i32 = <n>;` line in an adapter file.
fn set_protocol_constant(adapter_path: &Path, protocol: i32) -> Result<()> {
    let contents = std::fs::read_to_string(adapter_path)
        .with_context(|| format!("read {}", adapter_path.display()))?;
    let mut replaced = false;
    let rewritten = contents
        .lines()
        .map(|line| {
            let trimmed = line.trim_start();
            if !replaced && trimmed.starts_with("pub const PROTOCOL: i32 = ") {
                replaced = true;
                let indent = &line[..line.len() - trimmed.len()];
                format!("{indent}pub const PROTOCOL: i32 = {protocol};")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let rewritten = if contents.ends_with('\n') {
        format!("{rewritten}\n")
    } else {
        rewritten
    };
    if !replaced {
        bail!(
            "could not find `pub const PROTOCOL: i32 = ...;` in {}",
            adapter_path.display()
        );
    }
    std::fs::write(adapter_path, rewritten)
        .with_context(|| format!("write {}", adapter_path.display()))?;
    Ok(())
}

/// Adds the new family's `[workspace.dependencies]` line, after the last
/// existing `lodestone-v* = { path = ... }` entry.
fn wire_workspace_dependency(
    workspace_root: &Path,
    name: &str,
    wired: &mut Vec<String>,
) -> Result<()> {
    let manifest_path = workspace_root.join("Cargo.toml");
    let contents = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;
    let new_line = format!("lodestone-{name} = {{ path = \"crates/protocol/{name}\" }}");
    if contents.contains(&new_line) {
        return Ok(());
    }
    let lines: Vec<&str> = contents.lines().collect();
    let insert_at = lines
        .iter()
        .rposition(|line| {
            line.trim_start().starts_with("lodestone-v")
                && line.contains("path = \"crates/protocol/")
        })
        .map(|index| index + 1)
        .ok_or_else(|| anyhow!("no existing lodestone-v* workspace dependency to anchor after"))?;
    let mut rebuilt = lines[..insert_at].join("\n");
    rebuilt.push('\n');
    rebuilt.push_str(&new_line);
    rebuilt.push('\n');
    rebuilt.push_str(&lines[insert_at..].join("\n"));
    if contents.ends_with('\n') {
        rebuilt.push('\n');
    }
    std::fs::write(&manifest_path, rebuilt)
        .with_context(|| format!("write {}", manifest_path.display()))?;
    wired.push("Cargo.toml".to_owned());
    Ok(())
}

/// Wires the new family into the registry: an optional dependency, a feature,
/// and a `FAMILIES` entry. Best-effort — records residue if the registry's shape
/// is not what we expect rather than corrupting it.
fn wire_registry(
    workspace_root: &Path,
    name: &str,
    protocol: i32,
    wired: &mut Vec<String>,
    residue: &mut Vec<String>,
) -> Result<()> {
    let manifest_path = workspace_root.join("crates/lodestone-registry/Cargo.toml");
    let lib_path = workspace_root.join("crates/lodestone-registry/src/lib.rs");
    if !manifest_path.is_file() || !lib_path.is_file() {
        residue.push(format!(
            "add lodestone-{name} to the version registry by hand (registry crate not found)"
        ));
        return Ok(());
    }

    // Cargo.toml: optional dependency + feature line.
    let manifest = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;
    let dep_line = format!("lodestone-{name} = {{ workspace = true, optional = true }}");
    let feature_line = format!("{name} = [\"dep:lodestone-{name}\"]");
    let mut manifest_lines: Vec<String> = manifest.lines().map(str::to_owned).collect();
    let mut manifest_changed = false;
    let dep_present = manifest.contains(&dep_line);
    let feature_present = manifest.contains(&feature_line);
    let mut manifest_residue = Vec::new();
    if !dep_present {
        if let Some(index) = manifest_lines.iter().rposition(|line| {
            line.trim_start().starts_with("lodestone-v") && line.contains("optional = true")
        }) {
            manifest_lines.insert(index + 1, dep_line);
            manifest_changed = true;
        } else {
            manifest_residue.push(format!(
                "add lodestone-{name} optional dependency to crates/lodestone-registry/Cargo.toml by hand"
            ));
        }
    }
    if !feature_present {
        if let Some(index) = manifest_lines.iter().rposition(|line| {
            line.trim_start().starts_with("v") && line.contains("= [\"dep:lodestone-v")
        }) {
            manifest_lines.insert(index + 1, feature_line);
            manifest_changed = true;
        } else {
            manifest_residue.push(format!(
                "add `{name}` feature to crates/lodestone-registry/Cargo.toml by hand"
            ));
        }
    }
    if manifest_changed {
        let mut rebuilt = manifest_lines.join("\n");
        if manifest.ends_with('\n') {
            rebuilt.push('\n');
        }
        std::fs::write(&manifest_path, rebuilt)
            .with_context(|| format!("write {}", manifest_path.display()))?;
        wired.push("crates/lodestone-registry/Cargo.toml".to_owned());
    }
    residue.extend(manifest_residue);

    // lib.rs: FAMILIES entry, inserted before the closing `];`.
    let lib = std::fs::read_to_string(&lib_path)
        .with_context(|| format!("read {}", lib_path.display()))?;
    let entry = format!(
        "    #[cfg(feature = \"{name}\")]\n    Family {{\n        label: \"{name}\",\n        make: || Box::new(lodestone_{name}::adapter()),\n    }},\n"
    );
    if lib.contains(&format!("label: \"{name}\"")) {
        // already present
    } else if let Some(marker) = lib.find("const FAMILIES: &[Family] = &[") {
        if let Some(close_rel) = lib[marker..].find("];") {
            let close = marker + close_rel;
            let mut rebuilt = String::with_capacity(lib.len() + entry.len());
            rebuilt.push_str(&lib[..close]);
            rebuilt.push_str(&entry);
            rebuilt.push_str(&lib[close..]);
            std::fs::write(&lib_path, rebuilt)
                .with_context(|| format!("write {}", lib_path.display()))?;
            wired.push("crates/lodestone-registry/src/lib.rs".to_owned());
        } else {
            residue.push(format!(
                "add a FAMILIES entry for {name} (protocol {protocol}) to crates/lodestone-registry/src/lib.rs by hand"
            ));
        }
    } else {
        residue.push(format!(
            "add a FAMILIES entry for {name} (protocol {protocol}) to crates/lodestone-registry/src/lib.rs by hand"
        ));
    }
    Ok(())
}

pub fn run_conformance(
    workspace_root: &Path,
    options: &ConformanceOptions,
) -> Result<ConformanceReport> {
    let mut steps = Vec::new();
    let generated_dir = PathBuf::from(format!("crates/protocol/{}/src/generated", options.family));
    let packet_ids = generated_dir.join("packet_ids.rs");
    let packet_check = check_packet_ids(
        workspace_root,
        &options.minecraft_version,
        options.protocol_version,
        Some(&packet_ids),
        options.source,
    )?;
    if !packet_check.is_identical() {
        bail!("{}", packet_check.summary);
    }
    steps.push(ConformanceStep {
        name: "gen-packet-ids --check".to_owned(),
        outcome: ConformanceOutcome::Passed,
    });

    let registry_report = workspace_root
        .join(".cache")
        .join("mc")
        .join(&options.minecraft_version)
        .join("generated")
        .join("reports")
        .join("registries.json");
    if registry_report.exists() {
        let registry_options = GenRegistriesOptions {
            minecraft_version: options.minecraft_version.clone(),
            protocol_version: options.protocol_version,
            check: true,
            out_dir: generated_dir.clone(),
            registries: default_registry_specs()
                .iter()
                .map(|spec| spec.registry_key.to_owned())
                .collect(),
        };
        check_registries(workspace_root, &registry_options)?;
        steps.push(ConformanceStep {
            name: "gen-registries --check".to_owned(),
            outcome: ConformanceOutcome::Passed,
        });
    } else {
        steps.push(ConformanceStep {
            name: "gen-registries --check".to_owned(),
            outcome: ConformanceOutcome::Skipped(format!(
                "{} is absent; older server jars such as 1.16.5 do not emit Mojang registry reports",
                registry_report.display()
            )),
        });
    }

    let isolation = check_workspace_isolation(workspace_root)?;
    if isolation.has_violations() {
        bail!("{}", isolation.violation_summary());
    }
    steps.push(ConformanceStep {
        name: "check-isolation".to_owned(),
        outcome: ConformanceOutcome::Passed,
    });

    let deletability = check_workspace_deletable(workspace_root, &options.family)?;
    if !deletability.is_cleanly_deletable() {
        bail!(
            "{} is not cleanly deletable: {} blocking dependency(ies) would break the build",
            deletability.target_crate,
            deletability.blockers.len()
        );
    }
    steps.push(ConformanceStep {
        name: "check-deletable".to_owned(),
        outcome: ConformanceOutcome::Passed,
    });

    check_shape_reviews(workspace_root)?;
    steps.push(ConformanceStep {
        name: "shape-review".to_owned(),
        outcome: ConformanceOutcome::Passed,
    });

    let connected = check_workspace_connected(workspace_root)?;
    if connected.has_violations() {
        bail!("{}", connected.violation_summary());
    }
    steps.push(ConformanceStep {
        name: "check-connected".to_owned(),
        outcome: ConformanceOutcome::Passed,
    });

    if options.skip_cargo {
        steps.push(ConformanceStep {
            name: "cargo test/clippy".to_owned(),
            outcome: ConformanceOutcome::Skipped("--skip-cargo was provided".to_owned()),
        });
    } else {
        let package = format!("lodestone-{}", options.family);
        run_cargo_command(workspace_root, ["test", "-p", package.as_str()])?;
        steps.push(ConformanceStep {
            name: format!("cargo test -p {package}"),
            outcome: ConformanceOutcome::Passed,
        });
        run_cargo_command(
            workspace_root,
            [
                "clippy",
                "-p",
                package.as_str(),
                "--all-targets",
                "--",
                "-D",
                "warnings",
            ],
        )?;
        steps.push(ConformanceStep {
            name: format!("cargo clippy -p {package} --all-targets -- -D warnings"),
            outcome: ConformanceOutcome::Passed,
        });
    }

    Ok(ConformanceReport {
        family: options.family.clone(),
        steps,
    })
}

fn run_cargo_command<'a, I>(workspace_root: &Path, args: I) -> Result<()>
where
    I: IntoIterator<Item = &'a str>,
{
    let args: Vec<&str> = args.into_iter().collect();
    let status = Command::new("cargo")
        .args(&args)
        .current_dir(workspace_root)
        .status()
        .with_context(|| format!("run cargo {}", args.join(" ")))?;
    if !status.success() {
        bail!("cargo {} failed with {status}", args.join(" "));
    }
    Ok(())
}

fn cargo_metadata(workspace_root: &Path) -> Result<Value> {
    let manifest_path = workspace_root.join("Cargo.toml");
    let output = Command::new("cargo")
        .arg("metadata")
        .arg("--format-version")
        .arg("1")
        .arg("--no-deps")
        .arg("--manifest-path")
        .arg(&manifest_path)
        .output()
        .with_context(|| format!("run cargo metadata for {}", manifest_path.display()))?;
    if !output.status.success() {
        bail!(
            "cargo metadata failed for {}: {}",
            manifest_path.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    serde_json::from_slice(&output.stdout).context("parse cargo metadata JSON")
}

fn package_manifest_is_under_protocol(canonical_root: &Path, package: &Value) -> Result<bool> {
    let manifest_path = package
        .get("manifest_path")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("workspace package is missing manifest_path"))?;
    let manifest_path = Path::new(manifest_path);
    let canonical_manifest = manifest_path
        .canonicalize()
        .with_context(|| format!("canonicalize manifest path {}", manifest_path.display()))?;
    let relative = canonical_manifest
        .strip_prefix(canonical_root)
        .with_context(|| {
            format!(
                "manifest path {} is not under workspace root {}",
                canonical_manifest.display(),
                canonical_root.display()
            )
        })?;
    Ok(relative.starts_with("crates/protocol"))
}

/// Whether a workspace package has opted in to the version-registry role via
/// `[package.metadata.lodestone-isolation] role = "version-registry"`.
///
/// This is the single, deliberate hook that lets the isolation lint recognise
/// the intended version-aggregation crate. It is safe by construction: the role
/// only ever downgrades an *optional* shared -> version edge (already a
/// non-fatal warning) to an informational note. Every fatal rule — version ->
/// version, and a *required* shared -> version edge even on the registry itself
/// — is unaffected, so stamping this metadata on some other crate can at most
/// silence a warning it was already entitled to have as an optional dependency,
/// never a real, build-breaking violation.
fn package_is_version_registry(package: &Value) -> bool {
    package
        .get("metadata")
        .and_then(|metadata| metadata.get("lodestone-isolation"))
        .and_then(|table| table.get("role"))
        .and_then(Value::as_str)
        == Some("version-registry")
}

fn dependency_table_name(kind: Option<&Value>) -> &'static str {
    match kind.and_then(Value::as_str) {
        Some("dev") => "dev-dependencies",
        Some("build") => "build-dependencies",
        _ => "dependencies",
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GenRegistriesOptions {
    pub minecraft_version: String,
    pub protocol_version: i32,
    pub check: bool,
    pub out_dir: PathBuf,
    pub registries: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RegistryCodegenSpec {
    pub registry_key: &'static str,
    pub file_name: &'static str,
    pub module_stem: &'static str,
    pub count_const: &'static str,
    pub names_const: &'static str,
    pub noun: &'static str,
    pub packet_context: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistryTable {
    pub spec: RegistryCodegenSpec,
    pub names: Vec<String>,
    pub fixed_ranges: Option<Vec<Option<String>>>,
}

#[must_use]
pub fn default_registry_specs() -> Vec<RegistryCodegenSpec> {
    vec![
        RegistryCodegenSpec {
            registry_key: "minecraft:sound_event",
            file_name: "sound_events.rs",
            module_stem: "sound_events",
            count_const: "SOUND_EVENT_COUNT",
            names_const: "SOUND_EVENT_NAMES",
            noun: "sound event",
            packet_context: "sound packets carry Holder<SoundEvent>; direct ids map to this table after subtracting one from positive holder ids",
        },
        RegistryCodegenSpec {
            registry_key: "minecraft:particle_type",
            file_name: "particle_types.rs",
            module_stem: "particle_types",
            count_const: "PARTICLE_TYPE_COUNT",
            names_const: "PARTICLE_TYPE_NAMES",
            noun: "particle type",
            packet_context: "level_particles carries a particle-type registry id before any per-particle payload data",
        },
        RegistryCodegenSpec {
            registry_key: "minecraft:menu",
            file_name: "menus.rs",
            module_stem: "menus",
            count_const: "MENU_COUNT",
            names_const: "MENU_NAMES",
            noun: "menu",
            packet_context: "open_screen carries a menu registry id",
        },
        RegistryCodegenSpec {
            registry_key: "minecraft:item",
            file_name: "items.rs",
            module_stem: "items",
            count_const: "ITEM_COUNT",
            names_const: "ITEM_NAMES",
            noun: "item",
            packet_context: "item stacks carry an item registry id (Holder<Item> via id-mapper) before the data-component patch",
        },
    ]
}

pub fn parse_registry_report(
    json: &str,
    specs: &[RegistryCodegenSpec],
) -> Result<Vec<RegistryTable>> {
    let root: Value = serde_json::from_str(json).context("parse registries.json")?;
    let root = root
        .as_object()
        .ok_or_else(|| anyhow!("registries.json root must be an object"))?;

    let mut tables = Vec::with_capacity(specs.len());
    for spec in specs {
        let registry = root
            .get(spec.registry_key)
            .ok_or_else(|| anyhow!("registries.json is missing {}", spec.registry_key))?;
        let entries = registry
            .get("entries")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow!("registry {} is missing entries object", spec.registry_key))?;
        let mut names_by_id = vec![None; entries.len()];
        let mut fixed_ranges_by_id = if spec.registry_key == "minecraft:sound_event" {
            Some(vec![None; entries.len()])
        } else {
            None
        };

        for (name, entry) in sorted_object_entries(entries) {
            let id = entry
                .get("protocol_id")
                .and_then(Value::as_u64)
                .ok_or_else(|| anyhow!("registry entry {name} is missing integer protocol_id"))?;
            let id = usize::try_from(id)
                .with_context(|| format!("registry entry {name} protocol_id is too large"))?;
            let Some(slot) = names_by_id.get_mut(id) else {
                bail!(
                    "registry {} entry {name} has protocol_id {id}, outside 0..{}",
                    spec.registry_key,
                    entries.len()
                );
            };
            if slot.replace(name.to_owned()).is_some() {
                bail!(
                    "registry {} has duplicate protocol_id {id}",
                    spec.registry_key
                );
            }

            if let Some(fixed_ranges) = &mut fixed_ranges_by_id {
                fixed_ranges[id] = parse_sound_fixed_range(entry)
                    .with_context(|| format!("parse fixed_range for sound event {name}"))?;
            }
        }

        let names = names_by_id
            .into_iter()
            .enumerate()
            .map(|(id, name)| {
                name.ok_or_else(|| {
                    anyhow!(
                        "registry {} is missing contiguous protocol_id {id}",
                        spec.registry_key
                    )
                })
            })
            .collect::<Result<Vec<_>>>()?;
        tables.push(RegistryTable {
            spec: *spec,
            names,
            fixed_ranges: fixed_ranges_by_id,
        });
    }

    Ok(tables)
}

fn parse_sound_fixed_range(entry: &Value) -> Result<Option<String>> {
    let value = entry
        .get("fixed_range")
        .or_else(|| entry.get("fixedRange"))
        .or_else(|| {
            entry
                .get("value")
                .and_then(|value| value.get("fixed_range"))
        })
        .or_else(|| entry.get("value").and_then(|value| value.get("fixedRange")));
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let range = value
        .as_f64()
        .ok_or_else(|| anyhow!("fixed_range must be a JSON number"))?;
    if !range.is_finite() || range > f32::MAX as f64 || range < f32::MIN as f64 {
        bail!("fixed_range {range} is outside finite f32 range");
    }
    Ok(Some(float_literal(range)))
}

fn float_literal(value: f64) -> String {
    let mut literal = value.to_string();
    if !literal.contains(['.', 'e', 'E']) {
        literal.push_str(".0");
    }
    literal
}

pub fn generate_registry_source(
    table: &RegistryTable,
    minecraft_version: &str,
    protocol_version: i32,
) -> Result<String> {
    let count = table.names.len();
    let mut source = String::new();
    writeln!(
        source,
        "// @generated by `cargo xtask gen-registries` from Minecraft {minecraft_version} (protocol {protocol_version}). DO NOT EDIT."
    )?;
    writeln!(
        source,
        "// Source: .cache/mc/{minecraft_version}/generated/reports/registries.json registry {}.",
        table.spec.registry_key
    )?;
    writeln!(
        source,
        "//! Generated {} id->ResourceKey table for protocol {protocol_version} (Minecraft {minecraft_version}).",
        table.spec.noun
    )?;
    source.push_str("//!\n");
    writeln!(source, "//! {}.", table.spec.packet_context)?;
    source.push('\n');
    writeln!(
        source,
        "/// Number of {} entries (network ids are `0..{}`).",
        table.spec.noun, table.spec.count_const
    )?;
    writeln!(
        source,
        "pub const {}: u32 = {count};",
        table.spec.count_const
    )?;
    source.push('\n');
    writeln!(
        source,
        "/// Canonical {} identifier, indexed by network registry id.",
        table.spec.noun
    )?;
    if let Some(fixed_ranges) = &table.fixed_ranges {
        let entries_const = table.spec.names_const.replace("_NAMES", "_ENTRIES");
        writeln!(
            source,
            "pub static {entries_const}: [(&str, Option<f32>); {count}] = ["
        )?;
        for (name, fixed_range) in table.names.iter().zip(fixed_ranges) {
            match fixed_range {
                Some(range) => writeln!(source, "    ({name:?}, Some({range})),")?,
                None => writeln!(source, "    ({name:?}, None),")?,
            }
        }
        source.push_str("];\n\n");
        writeln!(
            source,
            "/// Canonical {} identifier only, indexed by network registry id.",
            table.spec.noun
        )?;
    }
    writeln!(
        source,
        "pub static {}: [&str; {count}] = [",
        table.spec.names_const
    )?;
    for name in &table.names {
        writeln!(source, "    {name:?},")?;
    }
    source.push_str("];\n");

    format_rust_source(&source)
}

pub fn generate_registries(
    workspace_root: &Path,
    options: &GenRegistriesOptions,
) -> Result<Vec<PathBuf>> {
    let (tables, out_dir) = load_registry_tables(workspace_root, options)?;
    std::fs::create_dir_all(&out_dir)
        .with_context(|| format!("create generated registry directory {}", out_dir.display()))?;

    let mut written = Vec::with_capacity(tables.len());
    for table in tables {
        let source =
            generate_registry_source(&table, &options.minecraft_version, options.protocol_version)?;
        let path = out_dir.join(table.spec.file_name);
        std::fs::write(&path, source)
            .with_context(|| format!("write generated registry table {}", path.display()))?;
        written.push(path);
    }
    Ok(written)
}

pub fn check_registries(workspace_root: &Path, options: &GenRegistriesOptions) -> Result<()> {
    let (tables, out_dir) = load_registry_tables(workspace_root, options)?;
    let mut summaries = Vec::new();
    for table in tables {
        let expected =
            generate_registry_source(&table, &options.minecraft_version, options.protocol_version)?;
        let path = out_dir.join(table.spec.file_name);
        let actual = std::fs::read_to_string(&path)
            .with_context(|| format!("read generated registry table {}", path.display()))?;
        if actual != expected {
            summaries.push(packet_id_diff_summary(&path, &expected, &actual));
        }
    }
    if !summaries.is_empty() {
        bail!("{}", summaries.join("\n\n"));
    }
    Ok(())
}

fn load_registry_tables(
    workspace_root: &Path,
    options: &GenRegistriesOptions,
) -> Result<(Vec<RegistryTable>, PathBuf)> {
    let report_path = workspace_root
        .join(".cache")
        .join("mc")
        .join(&options.minecraft_version)
        .join("generated")
        .join("reports")
        .join("registries.json");
    let json = std::fs::read_to_string(&report_path)
        .with_context(|| format!("read registry report at {}", report_path.display()))?;
    let specs = resolve_registry_specs(&options.registries)?;
    let tables = parse_registry_report(&json, &specs)?;
    let out_dir = resolve_generated_dir(workspace_root, &options.out_dir)?;
    Ok((tables, out_dir))
}

fn resolve_registry_specs(registry_keys: &[String]) -> Result<Vec<RegistryCodegenSpec>> {
    let defaults = default_registry_specs();
    registry_keys
        .iter()
        .map(|key| {
            let normalized = normalize_registry_key(key);
            defaults
                .iter()
                .copied()
                .find(|spec| spec.registry_key == normalized)
                .ok_or_else(|| {
                    anyhow!(
                        "unsupported registry {normalized:?}; supported registries are sound_event, particle_type, menu, item"
                    )
                })
        })
        .collect()
}

fn resolve_generated_dir(workspace_root: &Path, requested: &Path) -> Result<PathBuf> {
    let relative = if requested.is_absolute() {
        requested.strip_prefix(workspace_root).with_context(|| {
            format!(
                "output path {} must be inside workspace {}",
                requested.display(),
                workspace_root.display()
            )
        })?
    } else {
        requested
    };

    validate_relative_child_path(relative)?;
    if !path_is_generated_dir(relative) {
        bail!(
            "refusing to write outside crates/protocol/*/src/generated; requested {}",
            requested.display()
        );
    }

    Ok(workspace_root.join(relative))
}

fn path_is_generated_dir(relative: &Path) -> bool {
    let components: Vec<&std::ffi::OsStr> = relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part),
            _ => None,
        })
        .collect();

    matches!(
        components.as_slice(),
        [crates, protocol, _crate_name, src, generated]
            if *crates == "crates"
                && *protocol == "protocol"
                && *src == "src"
                && *generated == "generated"
    )
}

pub fn registry_table<'a>(
    tables: &'a [RegistryTable],
    registry_key: &str,
) -> Result<&'a RegistryTable> {
    tables
        .iter()
        .find(|table| table.spec.registry_key == registry_key)
        .ok_or_else(|| anyhow!("missing parsed registry table {registry_key}"))
}

const VERSION_MANIFEST_URL: &str =
    "https://launchermeta.mojang.com/mc/game/version_manifest_v2.json";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssetDownloads {
    pub client: DownloadSpec,
    pub server: DownloadSpec,
    pub asset_index: AssetIndexSpec,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DownloadSpec {
    pub url: String,
    pub sha1: String,
    pub size: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssetIndexSpec {
    pub id: String,
    pub url: String,
    pub sha1: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DownloadDecision {
    Download,
    SkipValid,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JarAssetCounts {
    pub block_textures: usize,
    pub block_models: usize,
    pub blockstates: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FetchAssetsSummary {
    pub client_path: PathBuf,
    pub client_size: u64,
    pub client_downloaded: bool,
    pub asset_index_path: PathBuf,
    pub asset_index_size: u64,
    pub asset_index_downloaded: bool,
    pub jar_counts: JarAssetCounts,
}

impl FetchAssetsSummary {
    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "client.jar: {} ({} bytes, {})\nasset index: {} ({} bytes, {})\njar assets: block textures={}, block models={}, blockstates={}\n",
            self.client_path.display(),
            self.client_size,
            if self.client_downloaded {
                "downloaded"
            } else {
                "cached"
            },
            self.asset_index_path.display(),
            self.asset_index_size,
            if self.asset_index_downloaded {
                "downloaded"
            } else {
                "cached"
            },
            self.jar_counts.block_textures,
            self.jar_counts.block_models,
            self.jar_counts.blockstates
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FetchVersionSummary {
    pub server_path: PathBuf,
    pub server_size: u64,
    pub server_downloaded: bool,
}

impl FetchVersionSummary {
    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "server.jar: {} ({} bytes, {})",
            self.server_path.display(),
            self.server_size,
            if self.server_downloaded {
                "downloaded"
            } else {
                "cached"
            }
        )
    }
}

pub fn parse_version_manifest(json: &str, minecraft_version: &str) -> Result<String> {
    let root: Value = serde_json::from_str(json).context("parse Mojang version manifest")?;
    let versions = root
        .get("versions")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("version manifest is missing versions array"))?;

    for version in versions {
        if version.get("id").and_then(Value::as_str) == Some(minecraft_version) {
            return version
                .get("url")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .ok_or_else(|| anyhow!("version {minecraft_version} is missing url"));
        }
    }

    bail!("Minecraft version {minecraft_version} was not found in Mojang version manifest")
}

pub fn parse_asset_downloads(json: &str) -> Result<AssetDownloads> {
    let root: Value = serde_json::from_str(json).context("parse Mojang per-version JSON")?;
    let client = root
        .get("downloads")
        .and_then(|downloads| downloads.get("client"))
        .ok_or_else(|| anyhow!("version JSON is missing downloads.client"))?;
    let server = root
        .get("downloads")
        .and_then(|downloads| downloads.get("server"))
        .ok_or_else(|| anyhow!("version JSON is missing downloads.server"))?;
    let asset_index = root
        .get("assetIndex")
        .ok_or_else(|| anyhow!("version JSON is missing assetIndex"))?;

    Ok(AssetDownloads {
        client: DownloadSpec {
            url: required_string(client, "downloads.client.url")?,
            sha1: required_string(client, "downloads.client.sha1")?,
            size: client
                .get("size")
                .and_then(Value::as_u64)
                .ok_or_else(|| anyhow!("version JSON is missing downloads.client.size"))?,
        },
        server: DownloadSpec {
            url: required_string(server, "downloads.server.url")?,
            sha1: required_string(server, "downloads.server.sha1")?,
            size: server
                .get("size")
                .and_then(Value::as_u64)
                .ok_or_else(|| anyhow!("version JSON is missing downloads.server.size"))?,
        },
        asset_index: AssetIndexSpec {
            id: required_string(asset_index, "assetIndex.id")?,
            url: required_string(asset_index, "assetIndex.url")?,
            sha1: required_string(asset_index, "assetIndex.sha1")?,
        },
    })
}

fn required_string(object: &Value, path: &str) -> Result<String> {
    let key = path
        .rsplit('.')
        .next()
        .ok_or_else(|| anyhow!("invalid JSON path {path}"))?;
    object
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow!("version JSON is missing {path}"))
}

pub fn verify_sha1(path: &Path, expected_sha1: &str) -> Result<()> {
    let actual_sha1 = file_sha1_hex(path)?;
    if actual_sha1.eq_ignore_ascii_case(expected_sha1) {
        return Ok(());
    }

    bail!(
        "SHA-1 mismatch for {}: expected {}, got {}",
        path.display(),
        expected_sha1,
        actual_sha1
    )
}

pub fn download_decision(
    path: &Path,
    expected_sha1: &str,
    force: bool,
) -> Result<DownloadDecision> {
    if force || !path.exists() {
        return Ok(DownloadDecision::Download);
    }

    match verify_sha1(path, expected_sha1) {
        Ok(()) => Ok(DownloadDecision::SkipValid),
        Err(error) => {
            let message = error.to_string();
            if message.contains("SHA-1 mismatch") {
                Ok(DownloadDecision::Download)
            } else {
                Err(error)
            }
        }
    }
}

pub fn fetch_assets(
    workspace_root: &Path,
    minecraft_version: &str,
    force: bool,
) -> Result<FetchAssetsSummary> {
    let manifest_json = curl_to_string(VERSION_MANIFEST_URL)?;
    let version_json_url = parse_version_manifest(&manifest_json, minecraft_version)?;
    let version_json = curl_to_string(&version_json_url)?;
    let downloads = parse_asset_downloads(&version_json)?;

    let version_cache = workspace_root
        .join(".cache")
        .join("mc")
        .join(minecraft_version);
    std::fs::create_dir_all(&version_cache)
        .with_context(|| format!("create asset cache directory {}", version_cache.display()))?;

    let client_path = version_cache.join("client.jar");
    let client_downloaded = download_verified_file(
        &downloads.client.url,
        &client_path,
        &downloads.client.sha1,
        force,
    )
    .context("download and verify client.jar")?;

    let asset_index_path =
        version_cache.join(format!("asset-index-{}.json", downloads.asset_index.id));
    // We intentionally do not download the asset object store yet. For Lodestone's current
    // renderer work, textures, models, and blockstates come from client.jar; the object store is
    // primarily sounds and language files that we do not need at this stage.
    let asset_index_downloaded = download_verified_file(
        &downloads.asset_index.url,
        &asset_index_path,
        &downloads.asset_index.sha1,
        force,
    )
    .context("download and verify asset index")?;

    let client_size = std::fs::metadata(&client_path)
        .with_context(|| format!("stat {}", client_path.display()))?
        .len();
    if client_size != downloads.client.size {
        bail!(
            "client.jar size mismatch for {}: expected {} bytes, got {} bytes",
            client_path.display(),
            downloads.client.size,
            client_size
        );
    }

    let asset_index_size = std::fs::metadata(&asset_index_path)
        .with_context(|| format!("stat {}", asset_index_path.display()))?
        .len();
    let jar_counts = count_client_jar_assets(&client_path)?;
    if jar_counts.block_textures == 0 || jar_counts.block_models == 0 || jar_counts.blockstates == 0
    {
        bail!(
            "client.jar did not contain the expected vanilla asset layout: block textures={}, block models={}, blockstates={}",
            jar_counts.block_textures,
            jar_counts.block_models,
            jar_counts.blockstates
        );
    }

    Ok(FetchAssetsSummary {
        client_path,
        client_size,
        client_downloaded,
        asset_index_path,
        asset_index_size,
        asset_index_downloaded,
        jar_counts,
    })
}

pub fn fetch_version(
    workspace_root: &Path,
    minecraft_version: &str,
    force: bool,
) -> Result<FetchVersionSummary> {
    let manifest_json = curl_to_string(VERSION_MANIFEST_URL)?;
    let version_json_url = parse_version_manifest(&manifest_json, minecraft_version)?;
    let version_json = curl_to_string(&version_json_url)?;
    let downloads = parse_asset_downloads(&version_json)?;

    let version_cache = workspace_root
        .join(".cache")
        .join("mc")
        .join(minecraft_version);
    let server_path = version_cache.join("server.jar");
    let server_downloaded = download_verified_file(
        &downloads.server.url,
        &server_path,
        &downloads.server.sha1,
        force,
    )
    .context("download and verify server.jar")?;
    let server_size = std::fs::metadata(&server_path)
        .with_context(|| format!("stat {}", server_path.display()))?
        .len();
    if server_size != downloads.server.size {
        bail!(
            "server.jar size mismatch for {}: expected {} bytes, got {} bytes",
            server_path.display(),
            downloads.server.size,
            server_size
        );
    }

    Ok(FetchVersionSummary {
        server_path,
        server_size,
        server_downloaded,
    })
}

fn file_sha1_hex(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = sha1::Sha1::new();
    let mut buffer = [0_u8; 16 * 1024];

    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("read {}", path.display()))?;
        if read == 0 {
            break;
        }
        sha1::Digest::update(&mut hasher, &buffer[..read]);
    }

    Ok(bytes_to_lower_hex(&sha1::Digest::finalize(hasher)))
}

fn bytes_to_lower_hex(bytes: &[u8]) -> String {
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

fn download_verified_file(
    url: &str,
    destination: &Path,
    expected_sha1: &str,
    force: bool,
) -> Result<bool> {
    if download_decision(destination, expected_sha1, force)? == DownloadDecision::SkipValid {
        return Ok(false);
    }

    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create download directory {}", parent.display()))?;
    }
    let partial = destination.with_extension("download");
    if partial.exists() {
        std::fs::remove_file(&partial)
            .with_context(|| format!("remove stale partial download {}", partial.display()))?;
    }

    if let Err(error) = curl_to_file(url, &partial) {
        let _ = std::fs::remove_file(&partial);
        return Err(error);
    }

    if let Err(error) = verify_sha1(&partial, expected_sha1) {
        let _ = std::fs::remove_file(&partial);
        return Err(error);
    }

    std::fs::rename(&partial, destination).with_context(|| {
        format!(
            "rename verified download {} to {}",
            partial.display(),
            destination.display()
        )
    })?;
    Ok(true)
}

fn curl_to_string(url: &str) -> Result<String> {
    let output = Command::new("curl")
        .arg("--fail")
        .arg("--location")
        .arg("--silent")
        .arg("--show-error")
        .arg(url)
        .output()
        .with_context(|| format!("run curl for {url}"))?;
    if !output.status.success() {
        bail!(
            "curl failed for {url}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    String::from_utf8(output.stdout).with_context(|| format!("curl output for {url} was not UTF-8"))
}

fn curl_to_file(url: &str, destination: &Path) -> Result<()> {
    let output = Command::new("curl")
        .arg("--fail")
        .arg("--location")
        .arg("--silent")
        .arg("--show-error")
        .arg("--output")
        .arg(destination)
        .arg(url)
        .output()
        .with_context(|| format!("run curl for {url}"))?;
    if !output.status.success() {
        bail!(
            "curl failed for {url}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

pub fn count_client_jar_assets(path: &Path) -> Result<JarAssetCounts> {
    let file = File::open(path).with_context(|| format!("open client jar {}", path.display()))?;
    let archive =
        zip::ZipArchive::new(file).with_context(|| format!("read zip {}", path.display()))?;
    let mut counts = JarAssetCounts {
        block_textures: 0,
        block_models: 0,
        blockstates: 0,
    };

    for name in archive.file_names() {
        if name.starts_with("assets/minecraft/textures/block/") && !name.ends_with('/') {
            counts.block_textures += 1;
        } else if name.starts_with("assets/minecraft/models/block/") && !name.ends_with('/') {
            counts.block_models += 1;
        } else if name.starts_with("assets/minecraft/blockstates/") && !name.ends_with('/') {
            counts.blockstates += 1;
        }
    }

    Ok(counts)
}

fn sorted_object_entries(object: &serde_json::Map<String, Value>) -> BTreeMap<&String, &Value> {
    object.iter().collect()
}

fn ensure_unique_generated_identifiers(report: &PacketReport) -> Result<()> {
    for state in PacketState::ALL {
        for bound in PacketBound::ALL {
            let mut seen = BTreeSet::new();
            for entry in report.entries(state, bound) {
                if !seen.insert(entry.const_ident.as_str()) {
                    bail!(
                        "duplicate generated identifier {} in {:?}/{:?}",
                        entry.const_ident,
                        state,
                        bound
                    );
                }
            }
        }
    }
    Ok(())
}

fn resolve_output_path(
    workspace_root: &Path,
    out: Option<&Path>,
    default: &str,
) -> Result<PathBuf> {
    let requested = out.unwrap_or_else(|| Path::new(default));
    let relative = if requested.is_absolute() {
        requested.strip_prefix(workspace_root).with_context(|| {
            format!(
                "output path {} must be inside workspace {}",
                requested.display(),
                workspace_root.display()
            )
        })?
    } else {
        requested
    };

    validate_relative_child_path(relative)?;
    if !path_is_generated_packet_ids(relative) {
        bail!(
            "refusing to write outside crates/protocol/*/src/generated; requested {}",
            requested.display()
        );
    }

    Ok(workspace_root.join(relative))
}

/// Returns whether `relative` names `crates/protocol/<crate>/src/generated/<file>`.
///
/// This keeps generated packet id tables confined to a version crate's
/// `generated` directory regardless of which version crate is targeted.
fn path_is_generated_packet_ids(relative: &Path) -> bool {
    let components: Vec<&std::ffi::OsStr> = relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part),
            _ => None,
        })
        .collect();

    matches!(
        components.as_slice(),
        [crates, protocol, _crate_name, src, generated, _file]
            if *crates == "crates"
                && *protocol == "protocol"
                && *src == "src"
                && *generated == "generated"
    )
}

fn validate_relative_child_path(path: &Path) -> Result<()> {
    for component in path.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                bail!(
                    "output path must be a normal child path: {}",
                    path.display()
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use std::{collections::BTreeSet, ops::Deref, path::Path, process::Command};

    const REAL_REPORT: &str = ".cache/mc/26.2/generated/reports/packets.json";

    fn load_real_report() -> Result<Option<PacketReport>> {
        let path = Path::new(REAL_REPORT);
        if !path.exists() {
            eprintln!("skipping packet report tests: {REAL_REPORT} is absent");
            return Ok(None);
        }

        let json = std::fs::read_to_string(path)?;
        Ok(Some(parse_packet_report(&json, "26.2", 776)?))
    }

    #[test]
    fn cli_help_lists_supported_and_planned_commands() {
        let help = root_help();
        assert!(help.contains("gen-packet-ids"));
        assert!(help.contains("fetch-assets"));
        assert!(help.contains("fetch-version"));
        assert!(help.contains("gen-reports"));
        assert!(help.contains("gen-registries"));
        assert!(help.contains("codegen-ratio"));
        assert!(help.contains("new-version"));
        assert!(help.contains("conformance"));
    }

    #[test]
    fn cli_parses_codegen_ratio_command() -> Result<()> {
        assert_eq!(parse_cli_args(["codegen-ratio"])?, CliCommand::CodegenRatio);
        Ok(())
    }

    #[test]
    fn cli_parses_gen_packet_ids_command() -> Result<()> {
        let command = parse_cli_args([
            "gen-packet-ids",
            "--version",
            "26.2",
            "--protocol",
            "776",
            "--check",
            "--out",
            "crates/protocol/v770/src/generated/packet_ids.rs",
        ])?;

        assert_eq!(
            command,
            CliCommand::GenPacketIds {
                minecraft_version: "26.2".to_owned(),
                protocol_version: 776,
                check: true,
                out: Some(PathBuf::from(
                    "crates/protocol/v770/src/generated/packet_ids.rs"
                )),
                source: PacketSource::Mojang,
            }
        );
        Ok(())
    }

    #[test]
    fn cli_parses_fetch_assets_command() -> Result<()> {
        let command = parse_cli_args(["fetch-assets", "--version", "26.2", "--force"])?;

        assert_eq!(
            command,
            CliCommand::FetchAssets {
                minecraft_version: "26.2".to_owned(),
                force: true,
            }
        );
        Ok(())
    }

    #[test]
    fn cli_parses_fetch_version_command() -> Result<()> {
        let command = parse_cli_args(["fetch-version", "--version", "1.16.5", "--force"])?;

        assert_eq!(
            command,
            CliCommand::FetchVersion {
                minecraft_version: "1.16.5".to_owned(),
                force: true,
            }
        );
        Ok(())
    }

    #[test]
    fn cli_parses_gen_registries_command() -> Result<()> {
        let command = parse_cli_args([
            "gen-registries",
            "--version",
            "26.2",
            "--protocol",
            "776",
            "--out-dir",
            "crates/protocol/v770/src/generated",
            "--check",
            "--registries",
            "sound_event,particle_type,menu,item",
        ])?;

        assert_eq!(
            command,
            CliCommand::GenRegistries {
                options: GenRegistriesOptions {
                    minecraft_version: "26.2".to_owned(),
                    protocol_version: 776,
                    check: true,
                    out_dir: PathBuf::from("crates/protocol/v770/src/generated"),
                    registries: vec![
                        "minecraft:sound_event".to_owned(),
                        "minecraft:particle_type".to_owned(),
                        "minecraft:menu".to_owned(),
                        "minecraft:item".to_owned(),
                    ],
                }
            }
        );
        Ok(())
    }

    #[test]
    fn cli_parses_conformance_command() -> Result<()> {
        let command = parse_cli_args([
            "conformance",
            "--family",
            "v735",
            "--minecraft",
            "1.16.5",
            "--protocol",
            "754",
            "--source",
            "minecraft-data",
        ])?;

        assert_eq!(
            command,
            CliCommand::Conformance {
                options: ConformanceOptions {
                    family: "v735".to_owned(),
                    minecraft_version: "1.16.5".to_owned(),
                    protocol_version: 754,
                    source: PacketSource::MinecraftData,
                    skip_cargo: false,
                }
            }
        );
        Ok(())
    }

    #[test]
    fn cli_parses_check_connected_command() -> Result<()> {
        let command = parse_cli_args([
            "check-connected",
            "--allowlist",
            "xtask/custom-connected.toml",
        ])?;

        assert_eq!(
            command,
            CliCommand::CheckConnected {
                allowlist: PathBuf::from("xtask/custom-connected.toml"),
            }
        );
        Ok(())
    }

    #[test]
    fn conformance_skip_cargo_checks_packet_ids_and_skips_absent_registry_report() -> Result<()> {
        let workspace = isolation_fixture(
            "conformance",
            &[("crates/protocol/v999", "lodestone-v999", "")],
        )?;
        let packet_report_json = r#"{
            "configuration": {"clientbound": {}, "serverbound": {}},
            "handshake": {"serverbound": {"minecraft:intention": {"protocol_id": 0}}},
            "login": {"clientbound": {}, "serverbound": {}},
            "play": {"clientbound": {}, "serverbound": {}},
            "status": {"clientbound": {}, "serverbound": {}}
        }"#;
        let cache_dir = workspace.join(".cache/mc/test/generated/reports");
        std::fs::create_dir_all(&cache_dir)?;
        std::fs::write(cache_dir.join("packets.json"), packet_report_json)?;
        std::fs::create_dir_all(workspace.join("xtask"))?;
        std::fs::write(
            workspace.join(DEFAULT_CONNECTED_ALLOWLIST),
            r#"
[[allow]]
crate = "lodestone-v999"
owner = "xtask-test"
reason = "fixture has no shipped binary root"
"#,
        )?;

        let report = parse_packet_report(packet_report_json, "test", 999)?;
        let generated_dir = workspace.join("crates/protocol/v999/src/generated");
        std::fs::create_dir_all(&generated_dir)?;
        std::fs::write(
            generated_dir.join("packet_ids.rs"),
            generate_packet_ids_source(&report)?,
        )?;

        let conformance = run_conformance(
            &workspace,
            &ConformanceOptions {
                family: "v999".to_owned(),
                minecraft_version: "test".to_owned(),
                protocol_version: 999,
                source: PacketSource::Mojang,
                skip_cargo: true,
            },
        )?;

        assert_eq!(
            conformance.steps,
            vec![
                ConformanceStep {
                    name: "gen-packet-ids --check".to_owned(),
                    outcome: ConformanceOutcome::Passed,
                },
                ConformanceStep {
                    name: "gen-registries --check".to_owned(),
                    outcome: ConformanceOutcome::Skipped(format!(
                        "{} is absent; older server jars such as 1.16.5 do not emit Mojang registry reports",
                        workspace
                            .join(".cache/mc/test/generated/reports/registries.json")
                            .display()
                    ),),
                },
                ConformanceStep {
                    name: "check-isolation".to_owned(),
                    outcome: ConformanceOutcome::Passed,
                },
                ConformanceStep {
                    name: "check-deletable".to_owned(),
                    outcome: ConformanceOutcome::Passed,
                },
                ConformanceStep {
                    name: "shape-review".to_owned(),
                    outcome: ConformanceOutcome::Passed,
                },
                ConformanceStep {
                    name: "check-connected".to_owned(),
                    outcome: ConformanceOutcome::Passed,
                },
                ConformanceStep {
                    name: "cargo test/clippy".to_owned(),
                    outcome: ConformanceOutcome::Skipped("--skip-cargo was provided".to_owned()),
                },
            ]
        );
        Ok(())
    }

    #[test]
    fn planned_commands_return_not_implemented_errors() {
        for command in ["fetch-version", "gen-reports", "new-version"] {
            let error = run_cli_command(CliCommand::Planned { name: command }).unwrap_err();
            assert!(
                error.to_string().contains("not implemented yet"),
                "unexpected error for {command}: {error}"
            );
        }
    }

    #[test]
    fn parses_asset_manifest_and_version_json() -> Result<()> {
        let manifest = r#"{
                "versions": [
                    {"id": "1.21.11", "url": "https://example.invalid/old.json"},
                    {"id": "26.2", "url": "https://example.invalid/26.2.json"}
                ]
            }"#;
        let version_url = parse_version_manifest(manifest, "26.2")?;
        assert_eq!(version_url, "https://example.invalid/26.2.json");

        let version_json = r#"{
                "assetIndex": {
                    "id": "26",
                    "url": "https://example.invalid/assets/26.json",
                    "sha1": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                },
                "downloads": {
                    "client": {
                        "url": "https://example.invalid/client.jar",
                        "sha1": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                        "size": 123456
                    },
                    "server": {
                        "url": "https://example.invalid/server.jar",
                        "sha1": "cccccccccccccccccccccccccccccccccccccccc",
                        "size": 654321
                    }
                }
            }"#;
        let downloads = parse_asset_downloads(version_json)?;
        assert_eq!(downloads.client.url, "https://example.invalid/client.jar");
        assert_eq!(
            downloads.client.sha1,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert_eq!(downloads.client.size, 123456);
        assert_eq!(downloads.server.url, "https://example.invalid/server.jar");
        assert_eq!(
            downloads.server.sha1,
            "cccccccccccccccccccccccccccccccccccccccc"
        );
        assert_eq!(downloads.server.size, 654321);
        assert_eq!(downloads.asset_index.id, "26");
        assert_eq!(
            downloads.asset_index.url,
            "https://example.invalid/assets/26.json"
        );
        assert_eq!(
            downloads.asset_index.sha1,
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        );
        Ok(())
    }

    #[test]
    fn packet_shape_diff_reports_changed_added_and_removed_shapes() -> Result<()> {
        let source = r#"{
            "play": {
                "toClient": {
                    "types": {
                        "packet": ["container", [
                            {"name": "name", "type": ["mapper", {"mappings": {
                                "0x00": "keep_alive",
                                "0x01": "entity_destroy",
                                "0x02": "old_only"
                            }}]}
                        ]],
                        "packet_keep_alive": ["container", [{"name": "id", "type": "i64"}]],
                        "packet_entity_destroy": ["container", [{"name": "count", "type": "i8"}]],
                        "packet_old_only": ["container", [{"name": "id", "type": "varint"}]]
                    }
                }
            }
        }"#;
        let target = r#"{
            "play": {
                "toClient": {
                    "types": {
                        "packet": ["container", [
                            {"name": "name", "type": ["mapper", {"mappings": {
                                "0x00": "keep_alive",
                                "0x01": "entity_destroy",
                                "0x03": "new_only"
                            }}]}
                        ]],
                        "packet_keep_alive": ["container", [{"name": "id", "type": "i64"}]],
                        "packet_entity_destroy": ["container", [{"name": "ids", "type": ["array", {"type": "varint"}]}]],
                        "packet_new_only": ["container", [{"name": "flag", "type": "bool"}]]
                    }
                }
            }
        }"#;

        let changes = compare_minecraft_data_packet_shapes(source, target)?;
        assert_eq!(
            changes,
            vec![
                PacketShapeChange {
                    state: PacketState::Play,
                    bound: PacketBound::Clientbound,
                    packet_name: "minecraft:entity_destroy".to_owned(),
                    kind: PacketShapeChangeKind::Changed,
                },
                PacketShapeChange {
                    state: PacketState::Play,
                    bound: PacketBound::Clientbound,
                    packet_name: "minecraft:new_only".to_owned(),
                    kind: PacketShapeChangeKind::Added,
                },
                PacketShapeChange {
                    state: PacketState::Play,
                    bound: PacketBound::Clientbound,
                    packet_name: "minecraft:old_only".to_owned(),
                    kind: PacketShapeChangeKind::Removed,
                },
            ]
        );
        Ok(())
    }

    #[test]
    fn minecraft_data_protocol_json_falls_back_to_latest_same_major_shape() -> Result<()> {
        let workspace = fresh_test_workspace("minecraft-data-fallback")?;
        let pc = workspace.join("vendor/minecraft-data/data/pc");
        std::fs::create_dir_all(pc.join("1.16.2"))?;
        std::fs::create_dir_all(pc.join("1.16.5"))?;
        std::fs::write(
            pc.join("1.16.2/version.json"),
            r#"{"minecraftVersion":"1.16.2","version":751,"majorVersion":"1.16"}"#,
        )?;
        std::fs::write(pc.join("1.16.2/protocol.json"), r#"{"play":{}}"#)?;
        std::fs::write(
            pc.join("1.16.5/version.json"),
            r#"{"minecraftVersion":"1.16.5","version":754,"majorVersion":"1.16"}"#,
        )?;

        let protocol = load_minecraft_data_protocol_json(&workspace, "1.16.5", 754)?;
        assert_eq!(protocol.minecraft_version, "1.16.5");
        assert_eq!(protocol.protocol_data_version, "1.16.2");
        assert_eq!(protocol.json, r#"{"play":{}}"#);
        Ok(())
    }

    #[test]
    fn parses_registry_report_fixture_and_generates_table_source() -> Result<()> {
        let report = r#"{
            "minecraft:sound_event": {
                "entries": {
                    "minecraft:block.note_block.bell": {"protocol_id": 1, "fixed_range": 16.0},
                    "minecraft:entity.allay.ambient_with_item": {"protocol_id": 0}
                }
            },
            "minecraft:particle_type": {
                "entries": {
                    "minecraft:block": {"protocol_id": 1},
                    "minecraft:angry_villager": {"protocol_id": 0}
                }
            },
            "minecraft:menu": {
                "entries": {
                    "minecraft:generic_9x2": {"protocol_id": 1},
                    "minecraft:generic_9x1": {"protocol_id": 0}
                }
            },
            "minecraft:item": {
                "entries": {
                    "minecraft:air": {"protocol_id": 0},
                    "minecraft:stone": {"protocol_id": 1}
                }
            }
        }"#;

        let tables = parse_registry_report(report, &default_registry_specs())?;
        assert_eq!(tables.len(), 4);
        assert_eq!(
            tables[0].names,
            vec![
                "minecraft:entity.allay.ambient_with_item".to_owned(),
                "minecraft:block.note_block.bell".to_owned(),
            ]
        );
        assert_eq!(
            tables[0].fixed_ranges,
            Some(vec![None, Some("16.0".to_owned())])
        );

        let source = generate_registry_source(&tables[0], "26.2", 776)?;
        assert!(source.contains("pub const SOUND_EVENT_COUNT: u32 = 2;"));
        assert!(source.contains("pub static SOUND_EVENT_ENTRIES: [(&str, Option<f32>); 2]"));
        assert!(source.contains("pub static SOUND_EVENT_NAMES: [&str; 2]"));
        assert!(source.contains("(\"minecraft:block.note_block.bell\", Some(16.0))"));
        assert!(source.contains("\"minecraft:entity.allay.ambient_with_item\""));
        let item_source =
            generate_registry_source(registry_table(&tables, "minecraft:item")?, "26.2", 776)?;
        assert!(item_source.contains("pub const ITEM_COUNT: u32 = 2;"));
        assert!(item_source.contains("pub static ITEM_NAMES: [&str; 2]"));
        assert!(item_source.contains("\"minecraft:stone\""));
        Ok(())
    }

    #[test]
    fn parses_real_registry_report_counts_for_dispatch_blockers() -> Result<()> {
        let path = Path::new(".cache/mc/26.2/generated/reports/registries.json");
        if !path.exists() {
            eprintln!(
                "skipping registry report codegen test: {} is absent",
                path.display()
            );
            return Ok(());
        }

        let json = std::fs::read_to_string(path)?;
        let tables = parse_registry_report(&json, &default_registry_specs())?;
        assert_eq!(
            registry_table(&tables, "minecraft:sound_event")?
                .names
                .len(),
            1968
        );
        assert_eq!(
            registry_table(&tables, "minecraft:particle_type")?
                .names
                .len(),
            125
        );
        assert_eq!(registry_table(&tables, "minecraft:menu")?.names.len(), 25);
        assert_eq!(registry_table(&tables, "minecraft:item")?.names.len(), 1537);
        Ok(())
    }

    #[test]
    fn registry_codegen_is_deterministic_and_standalone_rust() -> Result<()> {
        let path = Path::new(".cache/mc/26.2/generated/reports/registries.json");
        if !path.exists() {
            eprintln!(
                "skipping registry report codegen test: {} is absent",
                path.display()
            );
            return Ok(());
        }

        let json = std::fs::read_to_string(path)?;
        let tables = parse_registry_report(&json, &default_registry_specs())?;
        let workspace = fresh_test_workspace("registry-codegen")?;

        for table in &tables {
            let first = generate_registry_source(table, "26.2", 776)?;
            let second = generate_registry_source(table, "26.2", 776)?;
            assert_eq!(first, second);

            let source_path = workspace.join(table.spec.file_name);
            let output_path = workspace.join(format!("{}.rmeta", table.spec.module_stem));
            std::fs::write(&source_path, first)?;
            let status = Command::new("rustc")
                .arg("--edition=2024")
                .arg("--crate-type=lib")
                .arg(&source_path)
                .arg("--emit=metadata")
                .arg("-o")
                .arg(&output_path)
                .status()?;
            assert!(
                status.success(),
                "generated registry source failed to compile: {}",
                source_path.display()
            );
        }
        Ok(())
    }

    #[test]
    fn gen_registries_check_detects_drift_without_writing() -> Result<()> {
        let workspace = fresh_test_workspace("gen-registries-check")?;
        let report_dir = workspace.join(".cache/mc/26.2/generated/reports");
        let out_dir = workspace.join("crates/protocol/v770/src/generated");
        std::fs::create_dir_all(&report_dir)?;
        let report = r#"{
            "minecraft:sound_event": {"entries": {"minecraft:a": {"protocol_id": 0}}},
            "minecraft:particle_type": {"entries": {"minecraft:p": {"protocol_id": 0}}},
            "minecraft:menu": {"entries": {"minecraft:m": {"protocol_id": 0}}},
            "minecraft:item": {"entries": {"minecraft:air": {"protocol_id": 0}}}
        }"#;
        std::fs::write(report_dir.join("registries.json"), report)?;
        let options = GenRegistriesOptions {
            minecraft_version: "26.2".to_owned(),
            protocol_version: 776,
            check: true,
            out_dir: PathBuf::from("crates/protocol/v770/src/generated"),
            registries: default_registry_specs()
                .iter()
                .map(|spec| spec.registry_key.to_owned())
                .collect(),
        };

        let written = generate_registries(&workspace, &options)?;
        assert_eq!(written.len(), 4);
        check_registries(&workspace, &options)?;

        let item_path = out_dir.join("items.rs");
        let pristine = std::fs::read_to_string(&item_path)?;
        std::fs::write(
            &item_path,
            pristine.replace("minecraft:air", "minecraft:dirt"),
        )?;
        let error = check_registries(&workspace, &options).unwrap_err();
        let error = error.to_string();
        assert!(error.contains("items.rs is out of date"), "{error}");
        assert!(error.contains("expected:"), "{error}");
        assert!(error.contains("actual:"), "{error}");
        assert!(std::fs::read_to_string(&item_path)?.contains("minecraft:dirt"));
        Ok(())
    }

    #[test]
    fn sha1_verification_accepts_match_and_rejects_mismatch() -> Result<()> {
        let workspace = fresh_test_workspace("sha1-verification")?;
        let path = workspace.join("hello.txt");
        std::fs::write(&path, b"hello")?;

        verify_sha1(&path, "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d")?;
        let error = verify_sha1(&path, "0000000000000000000000000000000000000000").unwrap_err();
        assert!(error.to_string().contains("SHA-1 mismatch"));
        assert!(error.to_string().contains("hello.txt"));
        Ok(())
    }

    #[test]
    fn valid_existing_asset_file_is_skipped_unless_forced() -> Result<()> {
        let workspace = fresh_test_workspace("asset-skip")?;
        let path = workspace.join("client.jar");
        std::fs::write(&path, b"hello")?;

        assert_eq!(
            download_decision(&path, "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d", false)?,
            DownloadDecision::SkipValid
        );
        assert_eq!(
            download_decision(&path, "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d", true)?,
            DownloadDecision::Download
        );
        assert_eq!(
            download_decision(&path, "0000000000000000000000000000000000000000", false)?,
            DownloadDecision::Download
        );
        Ok(())
    }

    #[test]
    fn packet_id_check_accepts_pristine_and_rejects_corrupted_file() -> Result<()> {
        let Some(report) = load_real_report()? else {
            return Ok(());
        };

        let workspace_root = fresh_test_workspace("packet-id-check")?;
        let report_dir = workspace_root.join(".cache/mc/26.2/generated/reports");
        std::fs::create_dir_all(&report_dir)?;
        std::fs::write(
            report_dir.join("packets.json"),
            std::fs::read_to_string(REAL_REPORT)?,
        )?;

        let generated_path = workspace_root.join(DEFAULT_PACKET_IDS_OUT);
        std::fs::create_dir_all(generated_path.parent().expect("generated path has parent"))?;
        std::fs::write(&generated_path, generate_packet_ids_source(&report)?)?;

        let pristine = check_packet_ids(
            &workspace_root,
            "26.2",
            776,
            Some(Path::new(DEFAULT_PACKET_IDS_OUT)),
            PacketSource::Mojang,
        )?;
        assert!(pristine.is_identical());

        let corrupted = std::fs::read_to_string(&generated_path)?.replace(
            "pub const PROTOCOL_VERSION: i32 = 776;",
            "pub const PROTOCOL_VERSION: i32 = 777;",
        );
        std::fs::write(&generated_path, corrupted)?;

        let drift = check_packet_ids(
            &workspace_root,
            "26.2",
            776,
            Some(Path::new(DEFAULT_PACKET_IDS_OUT)),
            PacketSource::Mojang,
        )?;
        assert!(!drift.is_identical());
        assert!(drift.summary.contains("packet_ids.rs is out of date"));
        assert!(drift.summary.contains("line 3"));
        assert!(drift.summary.contains("PROTOCOL_VERSION"));
        assert!(std::fs::read_to_string(&generated_path)?.contains("PROTOCOL_VERSION: i32 = 777"));
        Ok(())
    }

    #[test]
    fn version_crate_may_depend_on_any_shared_crate() -> Result<()> {
        // The lint is expressed in terms of deletability, not an allowlist of
        // "blessed" shared crates. A version crate may depend on ANY version-free
        // shared crate (core/model/macros, but also world, net, ...) because
        // deleting the version folder never breaks a shared crate.
        let workspace = isolation_fixture(
            "version-depends-on-shared",
            &[
                ("crates/lodestone-core", "lodestone-core", ""),
                ("crates/lodestone-model", "lodestone-model", ""),
                ("crates/lodestone-macros", "lodestone-macros", ""),
                ("crates/lodestone-world", "lodestone-world", ""),
                ("crates/lodestone-net", "lodestone-net", ""),
                (
                    "crates/protocol/v1",
                    "lodestone-v1",
                    r#"
[dependencies]
lodestone-core = { path = "../../lodestone-core" }
lodestone-model = { path = "../../lodestone-model" }
lodestone-macros = { path = "../../lodestone-macros" }
lodestone-world = { path = "../../lodestone-world" }
lodestone-net = { path = "../../lodestone-net" }
"#,
                ),
            ],
        )?;

        let report = check_workspace_isolation(&workspace)?;
        assert_eq!(report.findings, Vec::new());
        assert!(!report.has_violations());
        Ok(())
    }

    #[test]
    fn version_to_version_dependency_is_a_violation() -> Result<()> {
        let workspace = isolation_fixture(
            "version-crate-dependency",
            &[
                ("crates/protocol/v1", "lodestone-v1", ""),
                (
                    "crates/protocol/v2",
                    "lodestone-v2",
                    r#"
[dependencies]
lodestone-v1 = { path = "../v1" }
"#,
                ),
            ],
        )?;

        let report = check_workspace_isolation(&workspace)?;
        assert_eq!(
            report.findings,
            vec![IsolationFinding {
                crate_name: "lodestone-v2".to_owned(),
                dependency_name: "lodestone-v1".to_owned(),
                dependency_table: "dependencies",
                optional: false,
                rule: IsolationRule::VersionDependsOnVersion,
                severity: Severity::Violation,
                detail: None,
            }]
        );
        assert!(report.has_violations());
        Ok(())
    }

    #[test]
    fn required_shared_to_version_dependency_is_a_violation() -> Result<()> {
        // A shared crate with a *required* dependency on a version crate makes
        // that version undeletable, so it is fatal.
        let workspace = isolation_fixture(
            "required-shared-to-version",
            &[
                ("crates/protocol/v1", "lodestone-v1", ""),
                (
                    "crates/lodestone-client",
                    "lodestone-client",
                    r#"
[dependencies]
lodestone-v1 = { path = "../protocol/v1" }
"#,
                ),
            ],
        )?;

        let report = check_workspace_isolation(&workspace)?;
        assert_eq!(
            report.findings,
            vec![IsolationFinding {
                crate_name: "lodestone-client".to_owned(),
                dependency_name: "lodestone-v1".to_owned(),
                dependency_table: "dependencies",
                optional: false,
                rule: IsolationRule::SharedDependsOnVersion,
                severity: Severity::Violation,
                detail: None,
            }]
        );
        assert!(report.has_violations());
        Ok(())
    }

    #[test]
    fn optional_shared_to_version_dependency_is_a_surfaced_warning() -> Result<()> {
        // This models the real, deliberate wart: lodestone-client names a
        // concrete version crate through an OPTIONAL, feature-gated dependency
        // for its live-join test. The version is still deletable (drop the
        // folder plus the feature line), so this is surfaced, not fatal.
        let workspace = isolation_fixture(
            "optional-shared-to-version",
            &[
                ("crates/protocol/v1", "lodestone-v1", ""),
                (
                    "crates/lodestone-client",
                    "lodestone-client",
                    r#"
[dependencies]
lodestone-v1 = { path = "../protocol/v1", optional = true }

[features]
live-v1 = ["dep:lodestone-v1"]
"#,
                ),
            ],
        )?;

        let report = check_workspace_isolation(&workspace)?;
        assert_eq!(
            report.findings,
            vec![IsolationFinding {
                crate_name: "lodestone-client".to_owned(),
                dependency_name: "lodestone-v1".to_owned(),
                dependency_table: "dependencies",
                optional: true,
                rule: IsolationRule::SharedDependsOnVersion,
                severity: Severity::Warning,
                detail: None,
            }]
        );
        assert!(!report.has_violations());
        assert!(report.warning_summary().is_some());
        Ok(())
    }

    #[test]
    fn dev_shared_to_version_dependency_is_a_surfaced_warning() -> Result<()> {
        // A dev-only shared -> version edge only breaks the shared crate's
        // tests, not its build, so it is surfaced rather than fatal.
        let workspace = isolation_fixture(
            "dev-shared-to-version",
            &[
                ("crates/protocol/v1", "lodestone-v1", ""),
                (
                    "crates/lodestone-client",
                    "lodestone-client",
                    r#"
[dev-dependencies]
lodestone-v1 = { path = "../protocol/v1" }
"#,
                ),
            ],
        )?;

        let report = check_workspace_isolation(&workspace)?;
        assert_eq!(
            report.findings,
            vec![IsolationFinding {
                crate_name: "lodestone-client".to_owned(),
                dependency_name: "lodestone-v1".to_owned(),
                dependency_table: "dev-dependencies",
                optional: false,
                rule: IsolationRule::SharedDependsOnVersion,
                severity: Severity::Warning,
                detail: None,
            }]
        );
        assert!(!report.has_violations());
        Ok(())
    }

    #[test]
    fn check_isolation_allows_third_party_dependencies() -> Result<()> {
        let workspace = isolation_fixture(
            "third-party-dependencies",
            &[(
                "crates/protocol/v1",
                "lodestone-v1",
                r#"
[dependencies]
serde_json = "1"

[build-dependencies]
anyhow = "1"
"#,
            )],
        )?;

        let report = check_workspace_isolation(&workspace)?;
        assert_eq!(report.findings, Vec::new());
        Ok(())
    }

    #[test]
    fn new_version_parser_defaults_from_v770_and_mojang() -> Result<()> {
        let command =
            parse_cli_args(["new-version", "--protocol", "340", "--minecraft", "1.12.2"])?;
        let CliCommand::NewVersion { options } = command else {
            panic!("expected NewVersion command");
        };
        assert_eq!(options.protocol, 340);
        assert_eq!(options.minecraft_version, "1.12.2");
        assert_eq!(options.from, "v770");
        assert_eq!(options.source, PacketSource::Mojang);
        assert_eq!(options.name, "v340");
        assert!(!options.force);
        Ok(())
    }

    #[test]
    fn new_version_parser_infers_minecraft_data_from_v47() -> Result<()> {
        let command = parse_cli_args([
            "new-version",
            "--protocol",
            "107",
            "--minecraft",
            "1.9",
            "--from",
            "v47",
        ])?;
        let CliCommand::NewVersion { options } = command else {
            panic!("expected NewVersion command");
        };
        // Copying from the legacy family selects the minecraft-data oracle.
        assert_eq!(options.source, PacketSource::MinecraftData);
        assert_eq!(options.from, "v47");
        Ok(())
    }

    #[test]
    fn new_version_parser_honours_explicit_name_and_source() -> Result<()> {
        let command = parse_cli_args([
            "new-version",
            "--protocol",
            "47",
            "--minecraft",
            "1.8",
            "--from",
            "v47",
            "--name",
            "v18",
            "--source",
            "mojang",
            "--force",
        ])?;
        let CliCommand::NewVersion { options } = command else {
            panic!("expected NewVersion command");
        };
        assert_eq!(options.name, "v18");
        assert_eq!(options.source, PacketSource::Mojang);
        assert!(options.force);
        Ok(())
    }

    #[test]
    fn capitalize_family_uppercases_leading_v() {
        assert_eq!(capitalize_family("v47"), "V47");
        assert_eq!(capitalize_family(""), "");
    }

    #[test]
    fn set_protocol_constant_rewrites_only_the_constant_line() -> Result<()> {
        let root = fresh_test_workspace("set-protocol-constant")?;
        let path = root.join("adapter.rs");
        std::fs::write(
            &path,
            "pub const PROTOCOL: i32 = 776;\nfn supports(p: i32) -> bool { p == PROTOCOL }\n",
        )?;
        set_protocol_constant(&path, 340)?;
        let rewritten = std::fs::read_to_string(&path)?;
        assert!(rewritten.contains("pub const PROTOCOL: i32 = 340;"));
        // The reference to PROTOCOL elsewhere is untouched.
        assert!(rewritten.contains("p == PROTOCOL"));
        assert!(!rewritten.contains("776"));
        Ok(())
    }

    #[test]
    fn shape_review_toml_records_specific_unreviewed_packet_deltas() -> Result<()> {
        let toml = render_shape_review_toml(&ShapeReviewManifest {
            source_family: "v340".to_owned(),
            target_family: "v735".to_owned(),
            source_minecraft_version: "1.12.2".to_owned(),
            source_protocol_version: 340,
            target_minecraft_version: "1.16.5".to_owned(),
            target_protocol_version: 754,
            entries: vec![PacketShapeChange {
                state: PacketState::Play,
                bound: PacketBound::Clientbound,
                packet_name: "minecraft:map_chunk".to_owned(),
                kind: PacketShapeChangeKind::Changed,
            }],
        })?;

        assert!(toml.contains("source_family = \"v340\""));
        assert!(toml.contains("target_family = \"v735\""));
        assert!(toml.contains("[[packet]]"));
        assert!(toml.contains("state = \"play\""));
        assert!(toml.contains("bound = \"clientbound\""));
        assert!(toml.contains("name = \"minecraft:map_chunk\""));
        assert!(toml.contains("change = \"changed\""));
        assert!(toml.contains("reviewed = false"));
        Ok(())
    }

    #[test]
    fn shape_review_guard_rejects_unreviewed_entries() -> Result<()> {
        let workspace = fresh_test_workspace("shape-review-guard")?;
        let family = workspace.join("crates/protocol/v999");
        std::fs::create_dir_all(&family)?;
        std::fs::write(
            family.join("SHAPE_REVIEW.toml"),
            r#"source_family = "v1"
target_family = "v999"

[[packet]]
state = "play"
bound = "clientbound"
name = "minecraft:map_chunk"
change = "changed"
reviewed = false
"#,
        )?;

        let error = check_shape_reviews(&workspace).unwrap_err().to_string();
        assert!(error.contains("v999"), "{error}");
        assert!(error.contains("minecraft:map_chunk"), "{error}");
        assert!(error.contains("reviewed = true"), "{error}");
        Ok(())
    }

    #[test]
    fn copy_tree_refuses_to_clone_live_tests() -> Result<()> {
        let workspace = fresh_test_workspace("skip-live-tests")?;
        let source = workspace.join("from");
        let target = workspace.join("to");
        std::fs::create_dir_all(source.join("tests"))?;
        std::fs::create_dir_all(source.join("src"))?;
        std::fs::write(
            source.join("tests/live_chunk.rs"),
            "panic!(\"wrong server\");",
        )?;
        std::fs::write(source.join("tests/chunk.rs"), "#[test] fn hermetic() {}")?;
        std::fs::write(source.join("src/lib.rs"), "pub struct V1;")?;

        let mut created = Vec::new();
        copy_tree_with_substitutions(
            &source,
            &target,
            &[("V1".to_owned(), "V2".to_owned())],
            &workspace,
            &mut created,
        )?;

        assert!(!target.join("tests/live_chunk.rs").exists());
        assert!(target.join("tests/chunk.rs").exists());
        assert_eq!(
            std::fs::read_to_string(target.join("src/lib.rs"))?,
            "pub struct V2;"
        );
        Ok(())
    }

    #[test]
    fn new_version_writes_shape_review_and_skips_registry_until_reviewed() -> Result<()> {
        let workspace = new_version_fixture_workspace()?;

        let report = scaffold_new_version(
            &workspace,
            &NewVersionOptions {
                name: "v2".to_owned(),
                protocol: 2,
                minecraft_version: "target".to_owned(),
                source: PacketSource::MinecraftData,
                from: "v1".to_owned(),
                force: false,
            },
        )?;

        assert_eq!(report.shape_changes.len(), 1);
        let review =
            std::fs::read_to_string(workspace.join("crates/protocol/v2/SHAPE_REVIEW.toml"))?;
        assert!(review.contains("name = \"minecraft:map_chunk\""));
        assert!(review.contains("reviewed = false"));
        assert!(
            workspace
                .join("crates/protocol/v2/tests/shape_review.rs")
                .exists()
        );
        assert!(
            !workspace
                .join("crates/protocol/v2/tests/live_chunk.rs")
                .exists()
        );

        let registry_manifest =
            std::fs::read_to_string(workspace.join("crates/lodestone-registry/Cargo.toml"))?;
        let registry_lib =
            std::fs::read_to_string(workspace.join("crates/lodestone-registry/src/lib.rs"))?;
        assert!(!registry_manifest.contains("lodestone-v2"));
        assert!(!registry_manifest.contains("v2 = [\"dep:lodestone-v2\"]"));
        assert!(!registry_lib.contains("label: \"v2\""));
        assert!(
            report
                .residue
                .iter()
                .any(|item| item.contains("registry wiring skipped"))
        );
        Ok(())
    }

    #[test]
    fn new_version_skips_registry_when_shape_diff_is_unavailable() -> Result<()> {
        let workspace = new_version_fixture_workspace()?;
        std::fs::write(
            workspace.join("crates/protocol/v1/src/generated/packet_ids.rs"),
            "pub const PROTOCOL_VERSION: i32 = 1;\n",
        )?;

        let report = scaffold_new_version(
            &workspace,
            &NewVersionOptions {
                name: "v2".to_owned(),
                protocol: 2,
                minecraft_version: "target".to_owned(),
                source: PacketSource::MinecraftData,
                from: "v1".to_owned(),
                force: false,
            },
        )?;

        let registry_manifest =
            std::fs::read_to_string(workspace.join("crates/lodestone-registry/Cargo.toml"))?;
        assert!(!registry_manifest.contains("lodestone-v2"));
        assert!(
            report
                .residue
                .iter()
                .any(|item| item.contains("shape diff unavailable"))
        );
        assert!(
            report
                .residue
                .iter()
                .any(|item| item.contains("registry wiring skipped"))
        );
        Ok(())
    }

    #[test]
    fn codegen_ratio_counts_version_families_structurally() -> Result<()> {
        let workspace = fresh_test_workspace("codegen-ratio")?;
        let v1_src = workspace.join("crates/protocol/v1/src");
        let v2_src = workspace.join("crates/protocol/v2/src");
        std::fs::create_dir_all(v1_src.join("generated"))?;
        std::fs::create_dir_all(&v2_src)?;
        std::fs::write(
            v1_src.join("lib.rs"),
            "#[derive(Encode, Decode)]\nstruct GeneratedShape;\n\nimpl Encode for Manual {}\nimpl Decode for Manual {}\n",
        )?;
        std::fs::write(
            v1_src.join("generated/packet_ids.rs"),
            "pub const A: i32 = 1;\n",
        )?;
        std::fs::write(
            v2_src.join("lib.rs"),
            "#[derive(\n    Debug,\n    Decode,\n)]\nstruct MultiLine;\n",
        )?;

        let report = codegen_ratio_report(&workspace)?;
        assert_eq!(report.families.len(), 2);
        assert_eq!(
            report.families[0],
            CodegenRatioFamily {
                family: "v1".to_owned(),
                derive_blocks: 1,
                manual_impls: 2,
                generated_lines: 1,
                hand_written_lines: 5,
            }
        );
        assert_eq!(report.families[1].family, "v2");
        assert_eq!(report.families[1].derive_blocks, 1);
        assert_eq!(report.families[1].manual_impls, 0);
        assert_eq!(report.families[1].generated_lines, 0);
        assert_eq!(report.families[1].hand_written_lines, 5);

        let rendered = report.render();
        assert!(rendered.contains("per-struct ratio is optimistic"));
        assert!(rendered.contains("v1"));
        assert!(rendered.contains("hand-written"));
        Ok(())
    }

    #[test]
    fn registry_optional_version_dependency_is_by_design_aggregation() -> Result<()> {
        // The version registry opts in via metadata and names versions only
        // through optional, feature-gated edges. That is the intended
        // aggregation point, so it is reported as informational, never a warning
        // or a violation.
        let workspace = isolation_fixture(
            "registry-optional-version",
            &[
                ("crates/protocol/v1", "lodestone-v1", ""),
                (
                    "crates/lodestone-registry",
                    "lodestone-registry",
                    r#"
[package.metadata.lodestone-isolation]
role = "version-registry"

[dependencies]
lodestone-v1 = { path = "../protocol/v1", optional = true }

[features]
v1 = ["dep:lodestone-v1"]
"#,
                ),
            ],
        )?;

        let report = check_workspace_isolation(&workspace)?;
        assert_eq!(
            report.findings,
            vec![IsolationFinding {
                crate_name: "lodestone-registry".to_owned(),
                dependency_name: "lodestone-v1".to_owned(),
                dependency_table: "dependencies",
                optional: true,
                rule: IsolationRule::RegistryAggregatesVersion,
                severity: Severity::Info,
                detail: None,
            }]
        );
        assert!(!report.has_violations());
        assert!(report.warning_summary().is_none());
        assert!(report.info_summary().is_some());
        Ok(())
    }

    #[test]
    fn registry_cannot_aggregate_unreviewed_shape_family() -> Result<()> {
        let workspace = isolation_fixture(
            "registry-unreviewed-shapes",
            &[
                ("crates/protocol/v2", "lodestone-v2", ""),
                (
                    "crates/lodestone-registry",
                    "lodestone-registry",
                    r#"
[package.metadata.lodestone-isolation]
role = "version-registry"

[dependencies]
lodestone-v2 = { path = "../protocol/v2", optional = true }
"#,
                ),
            ],
        )?;
        std::fs::write(
            workspace.join("crates/protocol/v2/SHAPE_REVIEW.toml"),
            r#"source_family = "v1"
target_family = "v2"

[[packet]]
state = "play"
bound = "clientbound"
name = "minecraft:map_chunk"
change = "changed"
reviewed = false
"#,
        )?;

        let report = check_workspace_isolation(&workspace)?;
        assert_eq!(report.findings.len(), 1);
        assert_eq!(
            report.findings[0].rule,
            IsolationRule::RegistryAggregatesUnreviewedVersion
        );
        assert_eq!(report.findings[0].severity, Severity::Violation);
        assert!(report.violation_summary().contains("minecraft:map_chunk"));
        Ok(())
    }

    #[test]
    fn check_connected_reports_orphan_chain_and_ignores_dev_dependencies() -> Result<()> {
        let workspace = connected_fixture(
            "connected-orphans",
            &[(
                "apps/lodestone",
                "lodestone-shell",
                true,
                r#"
[dev-dependencies]
lodestone-entity = { path = "../../crates/lodestone-entity" }
"#,
            )],
            &[
                (
                    "crates/lodestone-server",
                    "lodestone-server",
                    false,
                    r#"
[dependencies]
lodestone-worldgen = { path = "../lodestone-worldgen" }
"#,
                ),
                ("crates/lodestone-worldgen", "lodestone-worldgen", false, ""),
                ("crates/lodestone-entity", "lodestone-entity", false, ""),
            ],
            "",
        )?;

        let report = check_workspace_connected(&workspace)?;
        assert!(report.has_violations());
        let rendered = report.violation_summary();
        assert!(
            rendered.contains("lodestone-server is unreachable"),
            "{rendered}"
        );
        assert!(
            rendered.contains("lodestone-worldgen is unreachable; its non-dev workspace dependent lodestone-server is also unreachable"),
            "{rendered}"
        );
        assert!(
            rendered
                .contains("lodestone-entity is unreachable; it is only used by dev-dependencies"),
            "{rendered}"
        );
        Ok(())
    }

    #[test]
    fn check_connected_counts_optional_non_dev_dependencies_as_reachable() -> Result<()> {
        let workspace = connected_fixture(
            "connected-optional",
            &[(
                "apps/lodestone",
                "lodestone-shell",
                true,
                r#"
[dependencies]
lodestone-registry = { path = "../../crates/lodestone-registry" }
"#,
            )],
            &[
                (
                    "crates/lodestone-registry",
                    "lodestone-registry",
                    false,
                    r#"
[dependencies]
lodestone-v770 = { path = "../protocol/v770", optional = true }
"#,
                ),
                ("crates/protocol/v770", "lodestone-v770", false, ""),
            ],
            "",
        )?;

        let report = check_workspace_connected(&workspace)?;
        assert!(
            !report
                .violations()
                .any(|finding| finding.crate_name == "lodestone-v770"),
            "{}",
            report.violation_summary()
        );
        Ok(())
    }

    #[test]
    fn check_connected_requires_allowlist_reason_and_owner() -> Result<()> {
        let workspace = connected_fixture(
            "connected-allowlist",
            &[("apps/lodestone", "lodestone-shell", true, "")],
            &[("xtask", "xtask", true, "")],
            r#"
[[allow]]
crate = "xtask"
owner = "impl-xtask"
reason = "build tool, not shipped runtime artifact"
"#,
        )?;

        let report = check_workspace_connected(&workspace)?;
        assert!(!report.has_violations(), "{}", report.violation_summary());

        std::fs::write(
            workspace.join(DEFAULT_CONNECTED_ALLOWLIST),
            r#"
[[allow]]
crate = "xtask"
reason = ""
"#,
        )?;
        let error = check_workspace_connected(&workspace)
            .unwrap_err()
            .to_string();
        assert!(error.contains("owner"), "{error}");
        assert!(error.contains("reason"), "{error}");
        Ok(())
    }

    #[test]
    fn registry_required_version_dependency_is_still_a_violation() -> Result<()> {
        // Safety valve: the registry role only downgrades OPTIONAL edges. A
        // *required* version dependency — even on the designated registry — would
        // make that version undeletable, so it stays fatal. This is what stops
        // the metadata marker from being abused to hide a real violation.
        let workspace = isolation_fixture(
            "registry-required-version",
            &[
                ("crates/protocol/v1", "lodestone-v1", ""),
                (
                    "crates/lodestone-registry",
                    "lodestone-registry",
                    r#"
[package.metadata.lodestone-isolation]
role = "version-registry"

[dependencies]
lodestone-v1 = { path = "../protocol/v1" }
"#,
                ),
            ],
        )?;

        let report = check_workspace_isolation(&workspace)?;
        assert_eq!(
            report.findings,
            vec![IsolationFinding {
                crate_name: "lodestone-registry".to_owned(),
                dependency_name: "lodestone-v1".to_owned(),
                dependency_table: "dependencies",
                optional: false,
                rule: IsolationRule::SharedDependsOnVersion,
                severity: Severity::Violation,
                detail: None,
            }]
        );
        assert!(report.has_violations());
        Ok(())
    }

    #[test]
    fn registry_role_does_not_exempt_other_crates() -> Result<()> {
        // The exemption is scoped to the crate that carries the metadata role. A
        // different shared crate with the same optional edge still gets a
        // surfaced warning, so stamping one crate as the registry cannot quiet
        // another crate's coupling.
        let workspace = isolation_fixture(
            "registry-scoped-exemption",
            &[
                ("crates/protocol/v1", "lodestone-v1", ""),
                (
                    "crates/lodestone-registry",
                    "lodestone-registry",
                    r#"
[package.metadata.lodestone-isolation]
role = "version-registry"

[dependencies]
lodestone-v1 = { path = "../protocol/v1", optional = true }

[features]
v1 = ["dep:lodestone-v1"]
"#,
                ),
                (
                    "crates/lodestone-client",
                    "lodestone-client",
                    r#"
[dependencies]
lodestone-v1 = { path = "../protocol/v1", optional = true }

[features]
live-v1 = ["dep:lodestone-v1"]
"#,
                ),
            ],
        )?;

        let report = check_workspace_isolation(&workspace)?;
        // Registry edge is info; client edge is still a warning.
        assert!(
            report
                .infos()
                .any(|finding| finding.crate_name == "lodestone-registry")
        );
        let client_findings: Vec<_> = report
            .findings
            .iter()
            .filter(|finding| finding.crate_name == "lodestone-client")
            .collect();
        assert_eq!(client_findings.len(), 1);
        assert_eq!(client_findings[0].severity, Severity::Warning);
        assert_eq!(
            client_findings[0].rule,
            IsolationRule::SharedDependsOnVersion
        );
        assert!(!report.has_violations());
        assert!(report.warning_summary().is_some());
        Ok(())
    }

    #[test]
    fn check_deletable_treats_optional_dependent_as_clean() -> Result<()> {
        let workspace = isolation_fixture(
            "deletable-optional-dependent",
            &[
                ("crates/protocol/v1", "lodestone-v1", ""),
                (
                    "crates/lodestone-client",
                    "lodestone-client",
                    r#"
[dependencies]
lodestone-v1 = { path = "../protocol/v1", optional = true }

[features]
live-v1 = ["dep:lodestone-v1"]
"#,
                ),
            ],
        )?;

        let report = check_workspace_deletable(&workspace, "v1")?;
        assert_eq!(report.target_crate, "lodestone-v1");
        assert_eq!(report.target_dir, "crates/protocol/v1");
        assert!(report.is_cleanly_deletable());
        assert!(report.blockers.is_empty());
        assert_eq!(report.manual_edits.len(), 1);
        assert_eq!(report.manual_edits[0].crate_name, "lodestone-client");
        assert!(report.manual_edits[0].optional);
        // The workspace root manifest + the client manifest reference it, but the
        // v1 crate's own manifest is excluded.
        assert!(!report.manifest_lines.is_empty());
        assert!(
            report
                .manifest_lines
                .iter()
                .all(|line| !line.path.starts_with("crates/protocol/v1"))
        );
        Ok(())
    }

    #[test]
    fn check_deletable_flags_required_dependent_as_blocker() -> Result<()> {
        let workspace = isolation_fixture(
            "deletable-required-dependent",
            &[
                ("crates/protocol/v1", "lodestone-v1", ""),
                (
                    "crates/lodestone-client",
                    "lodestone-client",
                    r#"
[dependencies]
lodestone-v1 = { path = "../protocol/v1" }
"#,
                ),
            ],
        )?;

        let report = check_workspace_deletable(&workspace, "lodestone-v1")?;
        assert!(!report.is_cleanly_deletable());
        assert_eq!(report.blockers.len(), 1);
        assert_eq!(report.blockers[0].crate_name, "lodestone-client");
        assert!(!report.blockers[0].optional);
        Ok(())
    }

    #[test]
    fn check_deletable_flags_version_to_version_dependent_as_blocker() -> Result<()> {
        let workspace = isolation_fixture(
            "deletable-version-dependent",
            &[
                ("crates/protocol/v1", "lodestone-v1", ""),
                (
                    "crates/protocol/v2",
                    "lodestone-v2",
                    r#"
[dependencies]
lodestone-v1 = { path = "../v1", optional = true }

[features]
compat = ["dep:lodestone-v1"]
"#,
                ),
            ],
        )?;

        // Even though the edge is optional, a version->version dependency is a
        // hard break: v2 could not be built against a deleted v1 in its compat
        // configuration, and it is an isolation violation regardless.
        let report = check_workspace_deletable(&workspace, "v1")?;
        assert!(!report.is_cleanly_deletable());
        assert_eq!(report.blockers.len(), 1);
        assert!(report.blockers[0].dependent_is_version_crate);
        Ok(())
    }

    #[test]
    fn check_deletable_rejects_unknown_version() -> Result<()> {
        let workspace = isolation_fixture(
            "deletable-unknown",
            &[("crates/protocol/v1", "lodestone-v1", "")],
        )?;

        let error = check_workspace_deletable(&workspace, "v999").unwrap_err();
        assert!(error.to_string().contains("no version crate matched"));
        Ok(())
    }

    #[test]
    fn check_deletable_flags_feature_forward_reference() -> Result<()> {
        // This mirrors the real client wart: the client depends on the *registry*
        // (not on the version), and only forwards a Cargo feature to it via
        // `live-v1 = ["lodestone-registry/v1"]`. There is no dependency-graph edge
        // from the client to lodestone-v1, so a naive graph-only check misses it —
        // but Cargo validates the feature string at resolve time, so a dangling
        // forward breaks even the default build. The manifest scan must catch it.
        let workspace = isolation_fixture(
            "deletable-feature-forward",
            &[
                ("crates/protocol/v1", "lodestone-v1", ""),
                (
                    "crates/lodestone-registry",
                    "lodestone-registry",
                    r#"
[dependencies]
lodestone-v1 = { path = "../protocol/v1", optional = true }

[features]
v1 = ["dep:lodestone-v1"]
"#,
                ),
                (
                    "crates/lodestone-client",
                    "lodestone-client",
                    r#"
[dependencies]
lodestone-registry = { path = "../lodestone-registry" }

[features]
live-v1 = ["lodestone-registry/v1"]
"#,
                ),
            ],
        )?;

        let report = check_workspace_deletable(&workspace, "v1")?;
        // No graph edge from the client, but its feature-forward line must be
        // surfaced as a required manifest edit.
        let client_line = report
            .manifest_lines
            .iter()
            .find(|line| line.path == "crates/lodestone-client/Cargo.toml");
        let client_line = client_line.expect("client feature-forward line should be surfaced");
        assert!(
            client_line.text.contains("lodestone-registry/v1"),
            "expected the forwarded feature line, got {:?}",
            client_line.text
        );
        Ok(())
    }

    #[test]
    fn feature_forward_detection_respects_token_boundaries() {
        assert!(line_forwards_to_family_feature(
            r#"live-v1 = ["lodestone-registry/v1"]"#,
            "v1"
        ));
        assert!(line_forwards_to_family_feature(
            r#"x = ["lodestone-registry/v47", "other"]"#,
            "v47"
        ));
        // ...but not a longer token that merely starts with it.
        assert!(!line_forwards_to_family_feature(
            r#"live-v470 = ["lodestone-registry/v470"]"#,
            "v47"
        ));
        // A feature name that merely embeds the token (no `/token` path segment)
        // is not a forward and must not match.
        assert!(!line_forwards_to_family_feature(
            r#"compat = ["v47-shim"]"#,
            "v47"
        ));
    }

    #[test]
    fn check_deletable_surfaces_registry_source_cfg_entries() -> Result<()> {
        // The registry's FAMILIES table gates each family behind
        // `#[cfg(feature = "v1")]`. Those lines never break the build (a dead cfg
        // just compiles out), but they emit `unexpected_cfgs` warnings once the
        // feature is gone, and the workspace standard is zero warnings — so the
        // drill must surface them as required source edits.
        let workspace = isolation_fixture(
            "deletable-registry-source",
            &[
                ("crates/protocol/v1", "lodestone-v1", ""),
                (
                    "crates/lodestone-registry",
                    "lodestone-registry",
                    "[package.metadata.lodestone-isolation]\nrole = \"version-registry\"\n\n[dependencies]\nlodestone-v1 = { path = \"../protocol/v1\", optional = true }\n\n[features]\nv1 = [\"dep:lodestone-v1\"]\n",
                ),
            ],
        )?;
        std::fs::write(
            workspace.join("crates/lodestone-registry/src/lib.rs"),
            "pub const FAMILIES: &[Family] = &[\n    #[cfg(feature = \"v1\")]\n    Family { make: || Box::new(lodestone_v1::adapter()) },\n];\n",
        )?;

        let report = check_workspace_deletable(&workspace, "v1")?;
        assert_eq!(
            report.registry_source_lines.len(),
            2,
            "expected the cfg gate and the crate-path line, got {:?}",
            report.registry_source_lines
        );
        assert!(
            report
                .registry_source_lines
                .iter()
                .any(|line| line.text.contains("cfg(feature = \"v1\")"))
        );
        assert!(
            report
                .registry_source_lines
                .iter()
                .any(|line| line.text.contains("lodestone_v1::adapter"))
        );
        Ok(())
    }

    #[test]
    fn parses_real_packet_report_counts() -> Result<()> {
        let Some(report) = load_real_report()? else {
            return Ok(());
        };

        assert_eq!(
            report.count(PacketState::Play, PacketBound::Clientbound),
            141
        );
        assert_eq!(
            report.count(PacketState::Play, PacketBound::Serverbound),
            69
        );
        assert_eq!(
            report.count(PacketState::Configuration, PacketBound::Clientbound),
            20
        );
        assert_eq!(
            report.count(PacketState::Configuration, PacketBound::Serverbound),
            10
        );
        assert_eq!(
            report.count(PacketState::Login, PacketBound::Clientbound),
            6
        );
        assert_eq!(
            report.count(PacketState::Login, PacketBound::Serverbound),
            5
        );
        assert_eq!(
            report.count(PacketState::Status, PacketBound::Clientbound),
            2
        );
        assert_eq!(
            report.count(PacketState::Status, PacketBound::Serverbound),
            2
        );
        assert_eq!(
            report.count(PacketState::Handshaking, PacketBound::Clientbound),
            0
        );
        assert_eq!(
            report.count(PacketState::Handshaking, PacketBound::Serverbound),
            1
        );
        Ok(())
    }

    #[test]
    fn parses_real_minecraft_data_report_for_protocol_47() -> Result<()> {
        let path = Path::new("vendor/minecraft-data/data/pc/1.8/protocol.json");
        if !path.exists() {
            eprintln!("skipping minecraft-data test: {} is absent", path.display());
            return Ok(());
        }

        let json = std::fs::read_to_string(path)?;
        let report = parse_minecraft_data_report(&json, "1.8.8", 47)?;

        assert_eq!(report.protocol_version, 47);
        assert_eq!(report.minecraft_version, "1.8.8");

        // Spot-check a handful of well-known 1.8 ids across states/bounds.
        assert_eq!(
            report.id_for(
                PacketState::Handshaking,
                PacketBound::Serverbound,
                "minecraft:set_protocol"
            ),
            Some(0x00)
        );
        assert_eq!(
            report.id_for(
                PacketState::Login,
                PacketBound::Serverbound,
                "minecraft:login_start"
            ),
            Some(0x00)
        );
        assert_eq!(
            report.id_for(
                PacketState::Login,
                PacketBound::Clientbound,
                "minecraft:success"
            ),
            Some(0x02)
        );
        assert_eq!(
            report.id_for(
                PacketState::Login,
                PacketBound::Clientbound,
                "minecraft:compress"
            ),
            Some(0x03)
        );
        assert_eq!(
            report.id_for(
                PacketState::Play,
                PacketBound::Clientbound,
                "minecraft:keep_alive"
            ),
            Some(0x00)
        );
        assert_eq!(
            report.id_for(
                PacketState::Play,
                PacketBound::Clientbound,
                "minecraft:login"
            ),
            Some(0x01)
        );
        assert_eq!(
            report.name_for(PacketState::Play, PacketBound::Clientbound, 0x00),
            Some("minecraft:keep_alive")
        );

        // 1.8 has no configuration state.
        assert_eq!(
            report.count(PacketState::Configuration, PacketBound::Clientbound),
            0
        );
        Ok(())
    }

    #[test]
    fn parse_hex_packet_id_handles_prefixes() -> Result<()> {
        assert_eq!(parse_hex_packet_id("0x00")?, 0);
        assert_eq!(parse_hex_packet_id("0x1a")?, 26);
        assert_eq!(parse_hex_packet_id("0xfe")?, 254);
        assert!(parse_hex_packet_id("0xzz").is_err());
        Ok(())
    }

    #[test]
    fn generated_identifiers_are_unique_per_state_and_bound() -> Result<()> {
        let Some(report) = load_real_report()? else {
            return Ok(());
        };

        for state in PacketState::ALL {
            for bound in PacketBound::ALL {
                let mut identifiers = BTreeSet::new();
                for entry in report.entries(state, bound) {
                    assert!(
                        identifiers.insert(entry.const_ident.as_str()),
                        "duplicate identifier {} in {:?}/{:?}",
                        entry.const_ident,
                        state,
                        bound
                    );
                }
            }
        }
        Ok(())
    }

    #[test]
    fn generated_lookup_helpers_round_trip() -> Result<()> {
        let Some(report) = load_real_report()? else {
            return Ok(());
        };

        let source = generate_packet_ids_source(&report)?;
        let test_dir = Path::new("xtask/target/generated-packet-id-tests");
        std::fs::create_dir_all(test_dir)?;
        let test_source = test_dir.join("packet_ids_roundtrip.rs");
        let test_binary = test_dir.join("packet_ids_roundtrip");

        let mut source_with_tests = source;
        source_with_tests.push_str("\n#[cfg(test)]\nmod generated_round_trip_tests {\n    use super::*;\n\n    #[test]\n    fn all_packet_entries_round_trip() {\n");
        for entry in report.all_entries() {
            source_with_tests.push_str(&format!(
                "        assert_eq!(id_for({}, {}, {:?}), Some({}));\n        assert_eq!(name_for({}, {}, {}), Some({:?}));\n",
                entry.state.code_const(),
                entry.bound.code_const(),
                entry.name,
                entry.protocol_id,
                entry.state.code_const(),
                entry.bound.code_const(),
                entry.protocol_id,
                entry.name
            ));
        }
        source_with_tests.push_str("    }\n}\n");
        std::fs::write(&test_source, source_with_tests)?;

        let status = Command::new("rustc")
            .arg("--edition=2024")
            .arg("--test")
            .arg(&test_source)
            .arg("-o")
            .arg(&test_binary)
            .status()?;
        assert!(
            status.success(),
            "generated source test harness failed to compile"
        );

        let status = Command::new(&test_binary).status()?;
        assert!(
            status.success(),
            "generated id_for/name_for round-trip tests failed"
        );
        Ok(())
    }

    #[test]
    fn codegen_is_deterministic() -> Result<()> {
        let Some(report) = load_real_report()? else {
            return Ok(());
        };

        let first = generate_packet_ids_source(&report)?;
        let second = generate_packet_ids_source(&report)?;
        assert_eq!(first.as_bytes(), second.as_bytes());
        Ok(())
    }

    /// A throwaway workspace rooted in a unique temp directory.
    ///
    /// Fixtures used to live at a *fixed* reused path
    /// (`xtask/target/test-workspaces/<name>`) that each test deleted and
    /// recreated. Under the concurrent filesystem load of `cargo test
    /// --workspace`, that delete-then-recreate is not atomic: `remove_dir_all`
    /// intermittently failed with `ENOTEMPTY` and the follow-up `write` with
    /// `ENOENT` (confirmed by backtrace to originate in the fixture helper, not
    /// in any spawned `cargo`/`rustc` child). A unique `mkdtemp` directory per
    /// run removes the reuse window entirely, and cleanup happens on drop with
    /// errors ignored — so teardown can never fail a test. The guard is held by
    /// the test's binding for the duration of the test; `Deref<Target = Path>`
    /// lets call sites keep using `&workspace` and `workspace.join(..)`.
    struct TestWorkspace {
        dir: tempfile::TempDir,
    }

    impl Deref for TestWorkspace {
        type Target = Path;

        fn deref(&self) -> &Path {
            self.dir.path()
        }
    }

    fn fresh_test_workspace(name: &str) -> Result<TestWorkspace> {
        // Keep fixtures on the same filesystem as before (under the crate's
        // `target/`) rather than `$TMPDIR`, but give each run a unique mkdtemp
        // leaf instead of a fixed reused name. Only the shared *parent* is
        // created here (idempotent, race-free); the unique leaf is created
        // atomically by `tempdir_in`, so there is no delete-then-recreate window.
        let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-workspaces");
        std::fs::create_dir_all(&parent)
            .with_context(|| format!("create fixture parent {}", parent.display()))?;
        let dir = tempfile::Builder::new()
            .prefix(&format!("{name}-"))
            .tempdir_in(&parent)
            .with_context(|| format!("create temp workspace for {name}"))?;
        Ok(TestWorkspace { dir })
    }

    fn isolation_fixture(name: &str, crates: &[(&str, &str, &str)]) -> Result<TestWorkspace> {
        let workspace = fresh_test_workspace(name)?;
        let root = workspace.deref();
        let members = crates
            .iter()
            .map(|(path, _, _)| format!("\"{path}\""))
            .collect::<Vec<_>>()
            .join(", ");
        std::fs::write(
            root.join("Cargo.toml"),
            format!("[workspace]\nresolver = \"3\"\nmembers = [{members}]\n"),
        )?;

        for (path, crate_name, extra_manifest) in crates {
            let manifest_dir = root.join(path);
            std::fs::create_dir_all(&manifest_dir)?;
            std::fs::create_dir_all(manifest_dir.join("src"))?;
            std::fs::write(manifest_dir.join("src/lib.rs"), "")?;
            std::fs::write(
                manifest_dir.join("Cargo.toml"),
                format!(
                    "[package]\nname = \"{crate_name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n{extra_manifest}\n"
                ),
            )?;
        }

        Ok(workspace)
    }

    fn connected_fixture(
        name: &str,
        roots: &[(&str, &str, bool, &str)],
        crates: &[(&str, &str, bool, &str)],
        allowlist: &str,
    ) -> Result<TestWorkspace> {
        let workspace = fresh_test_workspace(name)?;
        let all = roots.iter().chain(crates.iter()).collect::<Vec<_>>();
        let members = all
            .iter()
            .map(|(path, _, _, _)| format!("\"{path}\""))
            .collect::<Vec<_>>()
            .join(", ");
        std::fs::write(
            workspace.join("Cargo.toml"),
            format!("[workspace]\nresolver = \"3\"\nmembers = [{members}]\n"),
        )?;

        for (path, crate_name, has_bin, extra_manifest) in all {
            let manifest_dir = workspace.join(path);
            std::fs::create_dir_all(manifest_dir.join("src"))?;
            let target_section = if *has_bin {
                std::fs::write(manifest_dir.join("src/main.rs"), "fn main() {}\n")?;
                ""
            } else {
                std::fs::write(manifest_dir.join("src/lib.rs"), "")?;
                ""
            };
            std::fs::write(
                manifest_dir.join("Cargo.toml"),
                format!(
                    "[package]\nname = \"{crate_name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n{target_section}{extra_manifest}\n"
                ),
            )?;
        }

        if !allowlist.trim().is_empty() {
            let allowlist_path = workspace.join(DEFAULT_CONNECTED_ALLOWLIST);
            std::fs::create_dir_all(allowlist_path.parent().expect("allowlist path has parent"))?;
            std::fs::write(allowlist_path, allowlist)?;
        }
        Ok(workspace)
    }

    fn new_version_fixture_workspace() -> Result<TestWorkspace> {
        let workspace = fresh_test_workspace("new-version-shape-review")?;
        let root = workspace.deref();
        std::fs::write(
            root.join("Cargo.toml"),
            r#"[workspace]
resolver = "3"
members = ["crates/protocol/*", "crates/lodestone-registry"]

[workspace.package]
version = "0.1.0"
edition = "2024"
license = "MIT OR Apache-2.0"

[workspace.dependencies]
lodestone-v1 = { path = "crates/protocol/v1" }
"#,
        )?;

        let v1 = root.join("crates/protocol/v1");
        std::fs::create_dir_all(v1.join("src/generated"))?;
        std::fs::create_dir_all(v1.join("tests"))?;
        std::fs::write(
            v1.join("Cargo.toml"),
            r#"[package]
name = "lodestone-v1"
version.workspace = true
edition.workspace = true
license.workspace = true
"#,
        )?;
        std::fs::write(
            v1.join("src/generated/packet_ids.rs"),
            "pub const PROTOCOL_VERSION: i32 = 1;\npub const MINECRAFT_VERSION: &str = \"source\";\n",
        )?;
        std::fs::write(v1.join("src/adapter.rs"), "pub const PROTOCOL: i32 = 1;\n")?;
        std::fs::write(v1.join("src/lib.rs"), "pub mod adapter;\n")?;
        std::fs::write(
            v1.join("tests/live_chunk.rs"),
            "#[test] fn cloned_live_gate() {}\n",
        )?;

        let registry = root.join("crates/lodestone-registry");
        std::fs::create_dir_all(registry.join("src"))?;
        std::fs::write(
            registry.join("Cargo.toml"),
            r#"[package]
name = "lodestone-registry"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
lodestone-v1 = { workspace = true, optional = true }

[features]
v1 = ["dep:lodestone-v1"]
"#,
        )?;
        std::fs::write(
            registry.join("src/lib.rs"),
            "const FAMILIES: &[&str] = &[\n    #[cfg(feature = \"v1\")]\n    \"v1\",\n];\n",
        )?;

        let pc = root.join("vendor/minecraft-data/data/pc");
        std::fs::create_dir_all(pc.join("source"))?;
        std::fs::create_dir_all(pc.join("target"))?;
        std::fs::write(
            pc.join("source/version.json"),
            r#"{"minecraftVersion":"source","version":1}"#,
        )?;
        std::fs::write(
            pc.join("target/version.json"),
            r#"{"minecraftVersion":"target","version":2}"#,
        )?;
        std::fs::write(
            pc.join("source/protocol.json"),
            minecraft_data_protocol_fixture("old_field"),
        )?;
        std::fs::write(
            pc.join("target/protocol.json"),
            minecraft_data_protocol_fixture("new_field"),
        )?;

        Ok(workspace)
    }

    fn minecraft_data_protocol_fixture(field_name: &str) -> String {
        format!(
            r#"{{
  "play": {{
    "toClient": {{
      "types": {{
        "packet": ["container", [
          {{"name": "name", "type": ["mapper", {{"mappings": {{"0x00": "map_chunk"}}}}]}}
        ]],
        "packet_map_chunk": ["container", [
          {{"name": "{field_name}", "type": "varint"}}
        ]]
      }}
    }}
  }}
}}"#
        )
    }
}
