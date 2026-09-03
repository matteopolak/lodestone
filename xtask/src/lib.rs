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

pub mod comment_voice;
pub mod islands;
pub mod no_winit_headless;
pub mod protocol_dup;
pub mod ptr_const;
pub mod world_coverage;

pub const DEFAULT_PACKET_IDS_OUT: &str = "crates/versions/26.2/src/generated/packet_ids.rs";
/// Default output for the minecraft-data-sourced protocol 47 (Minecraft 1.8.x).
pub const DEFAULT_PACKET_IDS_OUT_V47: &str = "crates/versions/1.8/src/generated/packet_ids.rs";
pub const DEFAULT_CONNECTED_ALLOWLIST: &str = "xtask/check-connected.toml";
/// Where `gen-registries` reads/writes the `sound_event`/`particle_type`/`menu`/
/// `item`/`data_component_type` registry tables by default, and where
/// `conformance`'s registry step drift-checks them regardless of `--family`.
/// These describe **the game**, not **the protocol** (see
/// `docs/lodestone-data-crate.md`), so unlike `packet_ids.rs` they are not
/// duplicated per protocol family — there is exactly one committed copy, for
/// the one canonical internal version (26.2 / v770).
pub const DEFAULT_REGISTRY_OUT_DIR: &str = "crates/lodestone-data/src/generated";

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
/// fabricate (see `docs/plans/multi-version-protocol-dedup.md`'s namespace
/// problem: v735 and v770 agree on only 7 of 88 `ENTRIES` names as plain
/// strings, so nothing here can be guessed from spelling). Each verified
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
fn resolve_canonical_alias<'a>(table: &[(&str, &'a str)], name: &str) -> Option<&'a str> {
    table
        .iter()
        .find(|(from, _)| *from == name)
        .map(|(_, to)| *to)
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

// `PartialEq` only (not `Eq`): `BenchCompare`'s `tolerance: f64` has no `Eq`
// impl, and assert_eq! in this module's tests only needs `PartialEq + Debug`.
#[derive(Clone, Debug, PartialEq)]
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
    FetchSounds {
        minecraft_version: String,
        /// Include background music and jukebox discs (+293 MB).
        all: bool,
        force: bool,
        /// Concurrent downloads; `None` means [`SOUND_FETCH_JOBS`].
        jobs: Option<usize>,
    },
    FetchVersion {
        minecraft_version: String,
        force: bool,
    },
    VersionTable {
        check: bool,
        fetch_missing: bool,
    },
    GenRegistries {
        options: GenRegistriesOptions,
    },
    CheckIsolation,
    CheckConnected {
        allowlist: PathBuf,
    },
    Connectedness,
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
    DocsIndex {
        check: bool,
    },
    BenchCompare {
        path: PathBuf,
        metric: String,
        scene: String,
        baseline_sha: Option<String>,
        candidate_sha: Option<String>,
        tolerance: f64,
    },
    /// wasm32 compile + confinement-guard tripwire.
    WasmCheck,
    Islands {
        only_crate: Option<String>,
    },
    /// Census of registry subjects that reach no draw path.
    WorldCoverage,
    /// Pointer-identity comparison / const-vs-static guard (`ptr_const`).
    CheckPtrConst,
    /// `winit`-absence guard for a `--no-default-features` `lodestone-shell`
    /// build (`no_winit_headless`).
    CheckNoWinitHeadless,
    /// Comment-voice / issue-reference guard (`comment_voice`).
    CheckCommentVoice {
        allowlist: PathBuf,
    },
    /// The four `docs/plans/multi-version-protocol-dedup.md` duplication
    /// measurements (file, struct, dispatch-arm, function) plus the
    /// minecraft-data adjacency table (`protocol_dup`).
    ProtocolDup,

    Planned {
        name: &'static str,
    },
}

#[must_use]
pub const fn root_help() -> &'static str {
    "xtask\n\nUsage:\n    cargo run -p xtask -- <command> [options]\n\nCommands:\n    gen-packet-ids   Generate Rust packet ID tables from a Mojang report or minecraft-data\n    fetch-assets     Download and verify vanilla client.jar, the asset index, and the asset-store objects client.jar stubs, into .cache/mc/<version>/\n    fetch-sounds     Download and verify the vanilla .ogg sound corpus (~80 MB) into .cache/mc/<version>/objects/\n    fetch-version    Download and verify vanilla server.jar into .cache/mc/<version>/\n    version-table    Generate/check the epic-343 16-version protocol/data-version table\n    gen-registries   Generate selected registry id->ResourceKey tables from registries.json\n    check-isolation  Enforce protocol version crate dependency isolation\n    check-connected  Enforce workspace crates are reachable from shipped binary/cdylib roots\n    connectedness    Report 26.2 play packet reachability\n    check-deletable  Simulate deleting a version family's folder and report the fallout\n    codegen-ratio    Report generated-vs-hand-written codec metrics per protocol family\n    new-version      Scaffold a protocol family; registry support is withheld until SHAPE_REVIEW.toml is discharged\n    gen-reports      Not implemented yet\n    conformance      Run packet-id, registry, isolation, deletability, test, and clippy checks for a family\n    docs-index       Generate docs/README.md from every doc's own H1 + `## What it is` summary\n    bench-compare    Ratio + verdict between two recorded bench-results/*.jsonl runs (issue #82)\n    wasm-check       wasm32 compile + confinement-guard tripwire (tested port of scripts/wasm-check.sh)\n    islands          syn-based scan for dead functions/methods, zero-read fields, and default-only fields\n    world-coverage   Census of registry subjects (entity/block-entity/particle types) that reach no draw path\n    check-ptr-const  syn-based guard: fail any std::ptr::eq/addr_eq or raw-pointer == that targets a const\n    check-no-winit-headless  Fail if winit is reachable from a --no-default-features lodestone-shell build\n    check-comment-voice  Fail on issue references and change-voice phrases in .rs/.md/.wgsl comments\n    protocol-dup     Report file/struct/dispatch-arm/function duplication across crates/versions/ plus the minecraft-data adjacency table\n\nOptions for gen-packet-ids:\n    --version <version>   Minecraft version, e.g. 26.2 (Mojang) or 1.8 (minecraft-data dir)\n    --protocol <id>       Protocol version, e.g. 776 or 47\n    --source <source>     Report source: mojang (default) or minecraft-data\n    --out <path>          Output path under crates/versions/*/src/generated/\n    --check               Compare generated output against disk and fail on drift without writing\n\nOptions for gen-registries:\n    --version <version>       Minecraft version, e.g. 26.2\n    --protocol <id>           Protocol version, e.g. 776\n    --out-dir <path>          Output directory (default crates/lodestone-data/src/generated;\n                              crates/versions/*/src/generated also accepted, for a table\n                              that is genuinely per-family translation data)\n    --registries <csv>        Registry keys to generate (default: sound_event,particle_type,menu,item)\n    --check                   Compare generated registry tables against disk without writing\n\nOptions for check-connected:\n    --allowlist <path>    TOML file of explicit exceptions (default: xtask/check-connected.toml)\n\nOptions for connectedness:\n    Parses 26.2's generated packet_ids.rs for play denominators, then classifies adapter dispatch outlets (ClientEvent, Directive, world/sink writes) with explicit UNCLASSIFIED output.\n\nOptions for check-deletable:\n    <version>             Version family to simulate deleting: package name (lodestone-v1-8), folder (1.8), or path\n\nOptions for codegen-ratio:\n    Reports both the optimistic per-struct derive/manual ratio and the more decision-useful absolute hand-written source lines.\n\nOptions for new-version:\n    --protocol <id>       Protocol number for the new family (required)\n    --minecraft <ver>    Minecraft version key for the packet-id oracle (required)\n    --from <family>       Existing family to copy from, e.g. v770 (default) or v47 (legacy tokens; still resolve to their new crates/versions/ folder)\n    --source <source>     Oracle: mojang or minecraft-data (default inferred from --from)\n    --name <vNNN>         Family folder/label (default v<protocol>)\n    --force               Overwrite the target folder if it already exists\n    SHAPE_REVIEW.toml     Generated when packet shapes differ; every entry must be reviewed before registry support may be added\n\nOptions for conformance:\n    --family <vNNN>       Version family package/feature suffix to check, e.g. v1-14\n    --minecraft <ver>     Minecraft version key for packet-id/registry checks\n    --protocol <id>       Protocol number for the family\n    --source <source>     Packet-id oracle: mojang or minecraft-data (default mojang)\n    --skip-cargo          Only run xtask structural checks; skip cargo test/clippy\n\nOptions for fetch-version:\n    --version <version>   Minecraft version, e.g. 1.16.5\n    --force               Re-download even when cached server.jar already matches its SHA-1\n\nOptions for fetch-assets:\n    --version <version>   Minecraft version, e.g. 26.2\n    --force               Re-download even when cached files already match their SHA-1\n    -h, --help            Print help\n  Also fetches asset-store objects, ~3.2 MB in total:\n    - the 8 whose name is in client.jar at a DIFFERENT size, i.e. the stubs the jar ships to be\n      overridden (the 6 panorama faces, panorama_overlay, unifont.json). Nothing at runtime can\n      tell a stub from the real asset, which is why these must be eager.\n    - minecraft/sounds.json (626 KB), which ShellAudio reads eagerly and cannot start without.\n  The 4871 .ogg samples (375 MB) are NOT fetched: a missing sample is one silent sound, resolved\n  lazily per event. Run `fetch-sounds` for the corpus.\n\nOptions for fetch-sounds:\n    --version <version>   Minecraft version, e.g. 26.2 (fetch-assets must have run first)\n    --all                 Also fetch background music and jukebox discs (+293 MB, 92 objects)\n    --jobs <n>            Concurrent downloads (default 12)\n    --force               Re-download every object even when it already matches its SHA-1\n  Derives the corpus from sounds.json, not a file list: every sample any non-music event can\n  select. Measured on 26.2 -- 4751 objects, 80.14 MB, including all six biome ambience loops.\n  Excluded by default: 70 music tracks + 22 jukebox records = 92 objects, 293.23 MB. The 28 index\n  .ogg objects no event references are fetched in neither mode. Every object's SHA-1 is verified\n  against the index, and a re-run of a complete fetch downloads nothing.\n\nOptions for version-table:\n    --check               Compare the generated table against crates/lodestone-registry/src/generated/version_table.rs and fail on drift without writing\n    --fetch-missing       Also run fetch-version for any of the 16 target versions with no cached .cache/mc/<version>/server.jar (network + disk heavy; off by default)\n\nOptions for docs-index:\n    --check               Compare the generated index against docs/README.md and fail on drift without writing\n  Do not hand-edit docs/README.md: add/edit a doc under docs/ (with an H1 and a `## What\n  it is`/`## What this is` summary paragraph) and re-run this command. `cargo test -p xtask`\n  already fails if the committed file drifts from the generator.\n\nOptions for bench-compare:\n    <path>                 A bench-results/<bench>.jsonl file (gitignored local measurement log)\n    --metric <name>        Metric name to compare, e.g. neighbourhood_factor_vs_single\n    --scene <name>          Scene string to compare (must match exactly, including punctuation)\n    --candidate <sha>       Git-sha prefix of the \"after\" run (default: most recent recorded run)\n    --baseline <sha>        Git-sha prefix of the \"before\" run (default: the run immediately\n                            preceding the candidate on the same machine/profile)\n    --tolerance <pct>       Tolerance band as a percentage (default 25, i.e. +/-25%)\n  Never wired into CI by this command -- a manual/local/scheduled check, per\n  docs/roadmap/benchmarks.md's policy. Exits non-zero when the ratio falls outside the\n  tolerance band (useful for a future opt-in script; this alone does not make anything\n  CI-blocking).\n\nOptions for world-coverage:\n    Enumerates all three registries through lodestone-data, resolves each subject against the real\n    rig corpora and dispatch tables (syn), and buckets it as drawn / stranded / absent. \"Stranded\"\n    is the finding class: code that names the subject but emits no geometry for it. Fails hard\n    rather than skipping when a declared draw-surface path or renderer anchor has moved.\n\nOptions for islands:\n    --crate <name>         Only report the named workspace crate (default: every crate)\n  Resolution is name-based (no type checker), so it has few false positives and real false\n  negatives on common names -- see docs/island-detection.md before trusting a finding. Exits\n  non-zero if cargo metadata fails, if any workspace member yields zero .rs files, or if more\n  than 5% of files fail to parse.\n\nOptions for check-ptr-const:\n  syn-parses crates/, xtask/ and web/, indexes every const/static item name, then fails on\n  any std::ptr::eq/addr_eq call or raw-pointer == comparison whose operand directly names a\n  const: a const has no stable address (inlined per use site), so a pointer-identity\n  comparison against one can silently stop matching under a different codegen backend --\n  see CLAUDE.md's const/static rule. Resolution is name-based, like islands; a comparison\n  that goes through a local variable or a function call is out of scope, not asserted safe.\n  Prints the full census (every comparison found, tagged const/static/unresolved) on every\n  run, pass or fail. Exits non-zero if fewer than 500 .rs files are found (a broken walk)\n  or if more than 5% of scanned files fail to parse.\n\nOptions for check-no-winit-headless:\n    No flags. Runs `cargo tree -p lodestone-shell --no-default-features -i winit` and fails\n    if winit is reachable -- a headless build must not link the windowing stack. See\n    docs/runtime-presentation.md's winit-free headless build section.\n\nOptions for check-comment-voice:\n    --allowlist <path>    TOML file of explicit exceptions (default: xtask/check-comment-voice.toml)\n  Scans every .rs/.md/.wgsl comment/doc-comment (.rs) or prose (.md, fenced code excluded) or\n  comment (.wgsl) for #123-shaped issue references and word-bounded, case-insensitive \"this\n  change\"/\"this commit\"/\"this patch\"/\"before this change\"/\"this PR\" phrases. Excludes\n  #[attributes], hex colour literals, and URL fragments from the issue-reference pattern, and\n  requires a trailing word boundary so \"this PR\" never matches \"this process\"/\"this property\".\n  Prints the full census (every hit found, tagged ALLOWED/VIOLATION) on every run, pass or fail.\n  Exits non-zero if fewer than 1500 .rs/.md/.wgsl files are found (a broken walk) or if any hit\n  is not covered by the allowlist.\n\nOptions for protocol-dup:\n  No flags. Re-derives docs/plans/multi-version-protocol-dedup.md's \"Duplication, four ways\"\n  tables from the working tree: whole-file line similarity (src/ + tests/, adjacent family\n  pairs), packet struct/enum body identity under src/packets/, handle_play dispatch-arm\n  token similarity (1.8/1.9/1.14 only -- 26.2 is a directory module, not an if-chain),\n  free-function body identity under src/ (excl. generated/, excl. #[cfg(test)]), and the\n  minecraft-data packet-shape adjacency table across the 15 covered target versions. Every\n  number is a fresh measurement, not a citation -- re-run before quoting, and a material\n  disagreement with the plan document is a finding to report, not a mismatch to paper over.\n"
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
        "fetch-sounds" => parse_fetch_sounds_args(&args[1..]),
        "gen-registries" => parse_gen_registries_args(&args[1..]),
        "check-isolation" => Ok(CliCommand::CheckIsolation),
        "check-connected" => parse_check_connected_args(&args[1..]),
        "connectedness" => Ok(CliCommand::Connectedness),
        "check-deletable" => parse_check_deletable_args(&args[1..]),
        "codegen-ratio" => Ok(CliCommand::CodegenRatio),
        "new-version" => parse_new_version_args(&args[1..]),
        "fetch-version" => parse_fetch_version_args(&args[1..]),
        "version-table" => parse_version_table_args(&args[1..]),
        "conformance" => parse_conformance_args(&args[1..]),
        "docs-index" => parse_docs_index_args(&args[1..]),
        "bench-compare" => parse_bench_compare_args(&args[1..]),
        "wasm-check" => Ok(CliCommand::WasmCheck),
        "islands" => parse_islands_args(&args[1..]),
        "world-coverage" => Ok(CliCommand::WorldCoverage),
        "check-ptr-const" => Ok(CliCommand::CheckPtrConst),
        "check-no-winit-headless" => Ok(CliCommand::CheckNoWinitHeadless),
        "check-comment-voice" => parse_check_comment_voice_args(&args[1..]),
        "protocol-dup" => Ok(CliCommand::ProtocolDup),
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
        CliCommand::FetchSounds {
            minecraft_version,
            all,
            force,
            jobs,
        } => {
            let workspace_root =
                std::env::current_dir().context("determine current workspace directory")?;
            let summary = fetch_sounds(
                &workspace_root,
                &minecraft_version,
                all,
                force,
                jobs.unwrap_or(SOUND_FETCH_JOBS),
            )?;
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
        CliCommand::VersionTable {
            check,
            fetch_missing,
        } => {
            let workspace_root =
                std::env::current_dir().context("determine current workspace directory")?;
            if check {
                let check = check_version_table(&workspace_root, fetch_missing)?;
                if !check.is_identical() {
                    bail!("{}", check.summary);
                }
                println!("{} is up to date", check.out_path.display());
            } else {
                let path = generate_version_table(&workspace_root, fetch_missing)?;
                println!("generated {}", path.display());
            }
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
        CliCommand::Connectedness => {
            let workspace_root =
                std::env::current_dir().context("determine current workspace directory")?;
            let report = connectedness_report(&workspace_root)?;
            println!("{}", report.render());
            if report.has_unclassified() {
                bail!(
                    "connectedness classification has {} unclassified clientbound arm(s)",
                    report.unclassified_count()
                );
            }
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
        CliCommand::DocsIndex { check } => {
            let workspace_root =
                std::env::current_dir().context("determine current workspace directory")?;
            if check {
                let check = check_docs_index(&workspace_root)?;
                if !check.is_identical() {
                    bail!("{}", check.summary);
                }
                println!("{} is up to date", check.out_path.display());
            } else {
                let path = write_docs_index(&workspace_root)?;
                println!("generated {}", path.display());
            }
            Ok(())
        }
        CliCommand::BenchCompare {
            path,
            metric,
            scene,
            baseline_sha,
            candidate_sha,
            tolerance,
        } => {
            let records = read_bench_records(&path)?;
            let report = compare_bench_records(
                &records,
                &BenchCompareOptions {
                    metric,
                    scene,
                    baseline_sha,
                    candidate_sha,
                    tolerance,
                },
            )?;
            print!("{}", report.render());
            if !report.within_tolerance() {
                std::process::exit(1);
            }
            Ok(())
        }
        CliCommand::WasmCheck => {
            let workspace_root =
                std::env::current_dir().context("determine current workspace directory")?;
            run_wasm_check(&workspace_root)
        }
        CliCommand::Islands { only_crate } => {
            let workspace_root =
                std::env::current_dir().context("determine current workspace directory")?;
            let report = islands::islands_report(&workspace_root)?;
            print!(
                "{}",
                islands::format_islands_report(&report, only_crate.as_deref())
            );
            Ok(())
        }
        CliCommand::WorldCoverage => {
            let workspace_root =
                std::env::current_dir().context("determine current workspace directory")?;
            let report = world_coverage::world_coverage_report(&workspace_root)?;
            print!("{}", world_coverage::format_world_coverage_report(&report));
            Ok(())
        }
        CliCommand::CheckPtrConst => {
            let workspace_root =
                std::env::current_dir().context("determine current workspace directory")?;
            ptr_const::run_check_ptr_const(&workspace_root)
        }
        CliCommand::CheckNoWinitHeadless => {
            let workspace_root =
                std::env::current_dir().context("determine current workspace directory")?;
            no_winit_headless::run_check_no_winit_headless(&workspace_root)
        }
        CliCommand::CheckCommentVoice { allowlist } => {
            let workspace_root =
                std::env::current_dir().context("determine current workspace directory")?;
            comment_voice::run_check_comment_voice(&workspace_root, &allowlist)
        }
        CliCommand::ProtocolDup => {
            let workspace_root =
                std::env::current_dir().context("determine current workspace directory")?;
            let report = protocol_dup::protocol_dup_report(&workspace_root)?;
            print!("{}", report.render());
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

fn parse_fetch_sounds_args(args: &[String]) -> Result<CliCommand> {
    let mut minecraft_version = None;
    let mut all = false;
    let mut force = false;
    let mut jobs = None;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => return Ok(CliCommand::Help),
            "--all" => all = true,
            "--force" => force = true,
            "--version" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow!("--version requires a value"))?;
                minecraft_version = Some(value.clone());
            }
            "--jobs" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow!("--jobs requires a value"))?;
                let parsed: usize = value
                    .parse()
                    .with_context(|| format!("--jobs expects a positive integer, got {value:?}"))?;
                if parsed == 0 {
                    bail!("--jobs must be at least 1");
                }
                jobs = Some(parsed);
            }
            unknown => bail!("unknown fetch-sounds option {unknown:?}"),
        }
        index += 1;
    }

    Ok(CliCommand::FetchSounds {
        minecraft_version: minecraft_version.ok_or_else(|| anyhow!("--version is required"))?,
        all,
        force,
        jobs,
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

fn parse_version_table_args(args: &[String]) -> Result<CliCommand> {
    let mut check = false;
    let mut fetch_missing = false;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => return Ok(CliCommand::Help),
            "--check" => check = true,
            "--fetch-missing" => fetch_missing = true,
            unknown => bail!("unknown version-table option {unknown:?}"),
        }
        index += 1;
    }

    Ok(CliCommand::VersionTable {
        check,
        fetch_missing,
    })
}

fn parse_docs_index_args(args: &[String]) -> Result<CliCommand> {
    let mut check = false;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => return Ok(CliCommand::Help),
            "--check" => check = true,
            unknown => bail!("unknown docs-index option {unknown:?}"),
        }
        index += 1;
    }

    Ok(CliCommand::DocsIndex { check })
}

fn parse_islands_args(args: &[String]) -> Result<CliCommand> {
    let mut only_crate = None;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => return Ok(CliCommand::Help),
            "--crate" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow!("--crate requires a value"))?;
                only_crate = Some(value.clone());
            }
            unknown => bail!("unknown islands option {unknown:?}"),
        }
        index += 1;
    }

    Ok(CliCommand::Islands { only_crate })
}

fn parse_bench_compare_args(args: &[String]) -> Result<CliCommand> {
    let mut path: Option<PathBuf> = None;
    let mut metric: Option<String> = None;
    let mut scene: Option<String> = None;
    let mut baseline_sha: Option<String> = None;
    let mut candidate_sha: Option<String> = None;
    let mut tolerance = 0.25_f64;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => return Ok(CliCommand::Help),
            "--metric" => {
                index += 1;
                metric = Some(
                    args.get(index)
                        .ok_or_else(|| anyhow!("--metric requires a value"))?
                        .clone(),
                );
            }
            "--scene" => {
                index += 1;
                scene = Some(
                    args.get(index)
                        .ok_or_else(|| anyhow!("--scene requires a value"))?
                        .clone(),
                );
            }
            "--baseline" => {
                index += 1;
                baseline_sha = Some(
                    args.get(index)
                        .ok_or_else(|| anyhow!("--baseline requires a git-sha prefix"))?
                        .clone(),
                );
            }
            "--candidate" => {
                index += 1;
                candidate_sha = Some(
                    args.get(index)
                        .ok_or_else(|| anyhow!("--candidate requires a git-sha prefix"))?
                        .clone(),
                );
            }
            "--tolerance" => {
                index += 1;
                let raw = args
                    .get(index)
                    .ok_or_else(|| anyhow!("--tolerance requires a percentage, e.g. 25"))?;
                let pct: f64 = raw
                    .parse()
                    .with_context(|| format!("--tolerance {raw:?} is not a number"))?;
                tolerance = pct / 100.0;
            }
            unknown if unknown.starts_with("--") => bail!("unknown bench-compare option {unknown:?}"),
            positional => {
                if path.is_some() {
                    bail!("bench-compare takes exactly one positional <path>, got a second: {positional:?}");
                }
                path = Some(PathBuf::from(positional));
            }
        }
        index += 1;
    }

    Ok(CliCommand::BenchCompare {
        path: path.ok_or_else(|| anyhow!("bench-compare requires a <path> to a bench-results/*.jsonl file"))?,
        metric: metric.ok_or_else(|| anyhow!("bench-compare requires --metric <name>"))?,
        scene: scene.ok_or_else(|| anyhow!("bench-compare requires --scene <name>"))?,
        baseline_sha,
        candidate_sha,
        tolerance,
    })
}

fn parse_gen_registries_args(args: &[String]) -> Result<CliCommand> {
    let mut minecraft_version = None;
    let mut protocol_version = None;
    let mut check = false;
    let mut out_dir = PathBuf::from(DEFAULT_REGISTRY_OUT_DIR);
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

fn parse_check_comment_voice_args(args: &[String]) -> Result<CliCommand> {
    let mut allowlist = PathBuf::from(comment_voice::DEFAULT_ALLOWLIST);
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
            unknown => bail!("unknown check-comment-voice option {unknown:?}"),
        }
        index += 1;
    }
    Ok(CliCommand::CheckCommentVoice { allowlist })
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
/// for a version must mean deleting a single `crates/versions/<version>` folder
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
struct PlayPacketIdSummary {
    clientbound: Vec<PlayPacketEntry>,
    serverbound: Vec<PlayPacketEntry>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PlayPacketEntry {
    const_name: String,
    resource_name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ClientboundArm {
    packet: String,
    /// Adapter source file this arm's dispatch site lives in, relative to the
    /// workspace root. A protocol family's adapter can be one flat
    /// `src/adapter.rs` or a `src/adapter/` directory module (v770, since its
    /// split) — see [`read_adapter_sources`] — so this is per-arm rather than
    /// one path for the whole family.
    file: String,
    line: usize,
    verdict: ClientboundVerdict,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ClientboundVerdict {
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
enum ConsumerOutlet {
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

fn parse_play_packet_id_summary(source: &str) -> Result<PlayPacketIdSummary> {
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
fn classify_clientbound_dispatch(
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
struct FunctionBody<'a> {
    body: &'a str,
}

fn extract_functions(source: &str) -> Result<BTreeMap<String, FunctionBody<'_>>> {
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

fn delegate_function_calls(
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
struct ServerboundDecodeArm {
    packet: String,
    line: usize,
    verdict: ServerboundDecodeVerdict,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ServerboundDecodeVerdict {
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
fn match_arm_body(source: &str, arrow_end: usize) -> Option<(usize, usize)> {
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
fn classify_serverbound_decode(
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
fn find_outside_comments(source: &str, from: usize, needle: &str) -> Option<usize> {
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
/// non-empty-bodied) match arm anywhere in `dispatch_source`
/// (`crates/lodestone-server/src/server.rs`) — the cross-crate second hop.
///
/// A variant can legitimately appear more than once (the Play-state
/// dispatcher has real arms; an earlier handshake/login-state dispatcher
/// no-ops every Play variant, and vice versa for its own variants) — so this
/// takes the **best** verdict across all occurrences, not the first.
fn serverbound_variant_is_connected(dispatch_source: &str, variant: &str) -> Result<bool> {
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
    let protocol_root = workspace_root.join("crates/versions");
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

fn natural_family_key(family: &str) -> (u8, u32, u32, &str) {
    if let Some(digits) = family.strip_prefix('v')
        && let Ok(value) = digits.parse::<u32>()
    {
        return (0, value, 0, family);
    }
    // Era-start Minecraft-version directory (`1.8`, `1.9`, `1.14`, `26.2`):
    // compare the major/minor components numerically rather than as a
    // legacy protocol number, so `1.14` does not sort before `1.8`.
    let mut parts = family.split('.').filter_map(|part| part.parse::<u32>().ok());
    if let Some(major) = parts.next() {
        return (1, major, parts.next().unwrap_or(0), family);
    }
    (2, 0, 0, family)
}

/// Options for scaffolding a new protocol version family (`xtask new-version`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewVersionOptions {
    /// Family label / folder name under `crates/versions/`, e.g. `v340`.
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

/// `(directory name, legacy token)` pairs for the four families whose folder
/// was renamed from a protocol-number token to an era-start Minecraft version
/// while their embedded `V<token>`-style identifiers — and therefore the
/// literal text `--from`/`new-version` still substitutes — did not change.
/// Every other, future family stays symmetric (folder name == token), which
/// is what keeps every existing synthetic fixture in this module's own tests
/// working with no lookup at all.
const RENAMED_FAMILY_DIRS: &[(&str, &str)] =
    &[("1.8", "v47"), ("1.9", "v340"), ("1.14", "v735"), ("26.2", "v770")];

/// Resolves a `--from`/`check-deletable` token to the directory it actually
/// lives in under `crates/versions/`. Falls back to the token itself, which
/// is correct for any family whose folder was never decoupled from its token.
fn family_dir_name(token: &str) -> &str {
    RENAMED_FAMILY_DIRS
        .iter()
        .find(|(_, legacy)| *legacy == token)
        .map_or(token, |(dir, _)| *dir)
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

    let protocol_dir = workspace_root.join("crates/versions");
    let from_dir = protocol_dir.join(family_dir_name(from_token));
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
    // token covers `lodestone-v26-2`, `lodestone_v26_2`, `mod v770`, doc refs; the
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
    let relative_out = format!("crates/versions/{to_token}/src/generated/packet_ids.rs");
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
            "registry wiring skipped for {to_token}: {} packet shape review entr{} must be marked reviewed = true in crates/versions/{to_token}/SHAPE_REVIEW.toml first",
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
        "review packet structs under crates/versions/{to_token}/src/packets/ — they are {from_token}'s wire shapes; change the ones that differ for protocol {}",
        options.protocol
    ));
    residue.push(format!(
        "update `minecraft_versions()` and crate docs in crates/versions/{to_token} to name {}",
        options.minecraft_version
    ));
    residue.push(format!(
        "update the login/play choreography in crates/versions/{to_token}/src/adapter.rs if it differs from {from_token}"
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
    let new_line = format!("lodestone-{name} = {{ path = \"crates/versions/{name}\" }}");
    if contents.contains(&new_line) {
        return Ok(());
    }
    let lines: Vec<&str> = contents.lines().collect();
    let insert_at = lines
        .iter()
        .rposition(|line| {
            line.trim_start().starts_with("lodestone-v")
                && line.contains("path = \"crates/versions/")
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
    let generated_dir = PathBuf::from(format!(
        "crates/versions/{}/src/generated",
        family_dir_name(&options.family)
    ));
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
        // Registry tables (sound events, particle types, menus, items, data
        // component types) describe the game, not the wire format for this
        // one family: they live in crates/lodestone-data/src/generated,
        // shared by every family, not under this family's own generated/.
        // Pointing this at `generated_dir` (crates/versions/<family>/src/generated)
        // was the stale-location bug — that path has not held these tables
        // since the lodestone-data extraction, so the check silently read
        // nothing for the one family (v770) that reaches this branch at all.
        let registry_options = GenRegistriesOptions {
            minecraft_version: options.minecraft_version.clone(),
            protocol_version: options.protocol_version,
            check: true,
            out_dir: PathBuf::from(DEFAULT_REGISTRY_OUT_DIR),
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

    // The BFS itself is unavoidably workspace-wide (reachability can only be
    // judged by walking the whole graph from every shipped root), but the
    // verdict handed to a --family run is scoped to that family's own
    // crates. Without this, `conformance --family v340` fails or passes
    // depending on unrelated crates elsewhere in the workspace -- a
    // per-family tool held hostage to state outside its own subject.
    let connected = check_workspace_connected_for_family(workspace_root, &options.family)?;
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
                "--no-deps",
                "--",
                "-D",
                "warnings",
            ],
        )?;
        steps.push(ConformanceStep {
            name: format!("cargo clippy -p {package} --all-targets --no-deps -- -D warnings"),
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
    Ok(relative.starts_with("crates/versions"))
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

/// Registry specs recognised by `--registries` beyond [`default_registry_specs`].
///
/// The default set is what conformance regenerates and drift-checks for every
/// family; `data_component_type` is opt-in because only 1.20.5+ reports it, so
/// it is generated explicitly for the families that carry item component
/// patches rather than folded into the family-agnostic default sweep.
#[must_use]
pub fn known_registry_specs() -> Vec<RegistryCodegenSpec> {
    let mut specs = default_registry_specs();
    specs.push(RegistryCodegenSpec {
        registry_key: "minecraft:data_component_type",
        file_name: "data_component_types.rs",
        module_stem: "data_component_types",
        count_const: "DATA_COMPONENT_TYPE_COUNT",
        names_const: "DATA_COMPONENT_TYPE_NAMES",
        noun: "data component type",
        packet_context: "an item stack's DataComponentPatch identifies each added or removed component by a data-component-type registry id",
    });
    specs
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
    let known = known_registry_specs();
    registry_keys
        .iter()
        .map(|key| {
            let normalized = normalize_registry_key(key);
            known
                .iter()
                .copied()
                .find(|spec| spec.registry_key == normalized)
                .ok_or_else(|| {
                    let supported = known
                        .iter()
                        .map(|spec| spec.registry_key.trim_start_matches("minecraft:"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    anyhow!("unsupported registry {normalized:?}; supported registries are {supported}")
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
            "refusing to write outside crates/lodestone-data/src/generated or crates/versions/*/src/generated; requested {}",
            requested.display()
        );
    }

    Ok(workspace_root.join(relative))
}

/// `crates/versions/*/src/generated` remains legal (a family-scoped override
/// is still a valid `--out-dir`, e.g. for a table this repo later decides is
/// genuinely per-protocol-family translation data rather than shared game
/// data), but `crates/lodestone-data/src/generated` is now the one the
/// default and `conformance` point at: `sound_events`/`particle_types`/
/// `menus`/`items`/`data_component_types` are game data, not protocol data,
/// and live there (see `docs/lodestone-data-crate.md`).
fn path_is_generated_dir(relative: &Path) -> bool {
    let components: Vec<&std::ffi::OsStr> = relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part),
            _ => None,
        })
        .collect();

    if let [crates, protocol, _crate_name, src, generated] = components.as_slice() {
        if *crates == "crates" && *protocol == "versions" && *src == "src" && *generated == "generated"
        {
            return true;
        }
    }

    matches!(
        components.as_slice(),
        [crates, data, src, generated]
            if *crates == "crates"
                && *data == "lodestone-data"
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

/// One asset-store object `fetch-assets` ensures is present: either one
/// `client.jar` shadows with a differently-sized stub, or one named in
/// [`REQUIRED_OBJECT_NAMES`]. See [`fetch_shadowed_objects`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FetchedObject {
    /// The asset-index name (no `assets/` prefix).
    pub name: String,
    /// Lowercase hex SHA-1, which is also the object's path.
    pub hash: String,
    /// The index's declared size — the real asset.
    pub size: u64,
    /// The size of the `client.jar` entry of the same name — the stub. `0` when
    /// the jar has no copy at all, which is the case for every
    /// [`REQUIRED_OBJECT_NAMES`] entry.
    pub jar_size: u64,
    /// Whether this run downloaded it (false = already cached and verified).
    pub downloaded: bool,
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
    /// The asset-store objects this command ensures are present — the ones the jar
    /// shadows with a differently-sized stub, plus `REQUIRED_OBJECT_NAMES`. All are
    /// on disk and SHA-1 verified by the time this is returned.
    pub fetched_objects: Vec<FetchedObject>,
}

impl FetchAssetsSummary {
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = format!(
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
        );
        let fetched = self
            .fetched_objects
            .iter()
            .filter(|o| o.downloaded)
            .count();
        out.push_str(&format!(
            "asset objects: {} ({} downloaded, {} cached)\n",
            self.fetched_objects.len(),
            fetched,
            self.fetched_objects.len() - fetched
        ));
        for object in &self.fetched_objects {
            out.push_str(&format!(
                "  {} {} real={} {}\n",
                object.name,
                if object.jar_size == 0 {
                    "not-in-jar".to_string()
                } else {
                    format!("jar-stub={}", object.jar_size)
                },
                object.size,
                if object.downloaded {
                    "downloaded"
                } else {
                    "cached"
                }
            ));
        }
        out
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

    let fetched_objects =
        fetch_shadowed_objects(&version_cache, &asset_index_path, &client_path, force)
            .context("download and verify the required asset-store objects")?;

    Ok(FetchAssetsSummary {
        client_path,
        client_size,
        client_downloaded,
        asset_index_path,
        asset_index_size,
        asset_index_downloaded,
        jar_counts,
        fetched_objects,
    })
}

/// Base URL of the launcher's content-addressed asset store.
const RESOURCES_BASE_URL: &str = "https://resources.download.minecraft.net";

/// Logical asset names to fetch **regardless** of whether `client.jar` shadows
/// them, because something refuses to start without them.
///
/// `minecraft/sounds.json` is the whole list today: `ShellAudio::load_from_root`
/// reads it eagerly and returns an error if it is absent, so without it audio does
/// not come up at all. It is one 626 KB file describing 1968 sound events, and it
/// is *not* jar-shadowed — the jar has no copy — so the size-disagreement rule
/// below would never select it.
///
/// `minecraft/font/unifont.zip` is the second, and it is the *data* half of a
/// jar-shadowed pair rather than a shadowed file itself. `font/include/unifont.json`
/// **is** shadowed (29 B stub in the jar, 3993 B real file in the store), so the
/// size-disagreement rule below already selects it — but that file's only content
/// is a `unihex` provider pointing at `font/unifont.zip`, which the jar does not
/// contain at all and so the rule cannot see. Fetching the declaration without the
/// data gives a font that resolves a unihex provider, finds no `hex_file`, and
/// draws the missing-glyph box for all 112,018 codepoints outside the three bitmap
/// sheets — the same symptom as not fetching either. 1.5 MB of GNU Unifont HEX
/// text; the `unifont_jp` and `unifont_pua` variants are behind the `jp` font
/// option and a private-use pack respectively, and are not needed to render.
///
/// The 4871 `.ogg` samples `sounds.json` references are deliberately **not** here.
/// They are 375 MB, and unlike a stub they fail *honestly*: a missing sample is one
/// silent sound, resolved lazily per event, not a wrong asset masquerading as the
/// right one. Someone wanting real audio should fetch them with [`ensure_object`]
/// rather than by growing this list.
const REQUIRED_OBJECT_NAMES: &[&str] =
    &["minecraft/sounds.json", "minecraft/font/unifont.zip"];

/// Ensure one asset-store object is on disk, verifying its SHA-1 against the
/// index.
///
/// This is the general primitive — *given a logical asset name, make the object
/// present and prove it is the right bytes* — that everything else here is built
/// from. `name` is an asset-index name with no `assets/` prefix. Returns whether
/// this call downloaded it (`false` = already cached and verified).
///
/// The hash is both the object's address and its integrity check, which is the
/// only one available: there is no signature and no manifest beyond the index.
/// [`download_verified_file`] does the verify-then-rename, so a failed digest
/// leaves nothing behind.
///
/// # Errors
///
/// Returns an error when `name` is not in the index, its hash is implausible, the
/// download fails, the SHA-1 does not match, or the resulting file's length
/// disagrees with the index.
pub fn ensure_object(
    version_cache: &Path,
    index: &serde_json::Map<String, Value>,
    name: &str,
    force: bool,
) -> Result<bool> {
    let meta = index
        .get(name)
        .ok_or_else(|| anyhow!("asset index has no object named {name:?}"))?;
    let hash = meta
        .get("hash")
        .and_then(|h| h.as_str())
        .ok_or_else(|| anyhow!("asset index entry {name:?} has no hash"))?;
    if hash.len() < 2 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("asset index entry {name:?} has an implausible hash {hash:?}");
    }
    let size = meta
        .get("size")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("asset index entry {name:?} has no size"))?;

    let destination = version_cache
        .join("objects")
        .join(&hash[0..2])
        .join(hash);
    let url = format!("{RESOURCES_BASE_URL}/{}/{hash}", &hash[0..2]);
    let downloaded = download_verified_file(&url, &destination, hash, force)
        .with_context(|| format!("download asset object {name} ({url})"))?;

    let on_disk = std::fs::metadata(&destination)
        .with_context(|| format!("stat {}", destination.display()))?
        .len();
    if on_disk != size {
        bail!(
            "asset object {name} size mismatch: index says {size} bytes, {} has {on_disk}",
            destination.display()
        );
    }
    Ok(downloaded)
}

/// Download every asset-store object whose name is **also** a `client.jar` entry
/// of a *different* size — i.e. every object the jar shadows with a stub — plus
/// [`REQUIRED_OBJECT_NAMES`].
///
/// # Why this exists, and why the boundary is where it is
///
/// `client.jar` ships deliberate stubs for a handful of files the object store
/// overrides, and reading the jar copy silently gives you the stub. Measured on
/// 26.2: of 5057 index objects exactly **8** share a name with a jar entry, and
/// all 8 differ in size —
///
/// | name | jar | real |
/// |---|---|---|
/// | `textures/gui/title/background/panorama_0.png` | 69 | 547,239 |
/// | `panorama_1.png` | 69 | 294,940 |
/// | `panorama_2.png` | 69 | 425,769 |
/// | `panorama_3.png` | 69 | 461,522 |
/// | `panorama_4.png` | 69 | 738,917 |
/// | `panorama_5.png` | 69 | 118,484 |
/// | `font/include/unifont.json` | 29 | 3,993 |
/// | `panorama_overlay.png` | 68 | 86 |
///
/// — about 2.6 MB in total. The title-screen panorama was ported against those
/// stubs and shipped a flat grey sky that looked like working code; this command
/// is what stops that recurring.
///
/// **The shadowed set is derived from the data, not hardcoded.** A name list would
/// rot at the next version bump; "present in both, sizes disagree" cannot. The one
/// hardcoded part is [`REQUIRED_OBJECT_NAMES`], for objects the jar does not
/// shadow but something refuses to start without.
///
/// It also keeps the boundary honest: this deliberately does **not** fetch the
/// remaining index-only objects, 4871 of which are `.ogg` samples totalling
/// 375 MB. A missing sample fails *honestly* — one silent sound, resolved lazily
/// per event — whereas nothing at runtime can tell a stub from the real asset.
/// [`ensure_object`] is the primitive to reach for if you do want the corpus.
///
/// SHA-1 is verified against the index hash after download by
/// [`download_verified_file`] — that is what the index hash is for, and the only
/// integrity check available here.
///
/// # Errors
///
/// Propagates a read/parse failure of the index or the jar, and any download or
/// SHA-1 verification failure.
pub fn fetch_shadowed_objects(
    version_cache: &Path,
    asset_index_path: &Path,
    client_path: &Path,
    force: bool,
) -> Result<Vec<FetchedObject>> {
    let index_bytes = std::fs::read(asset_index_path)
        .with_context(|| format!("read asset index {}", asset_index_path.display()))?;
    let index: serde_json::Value = serde_json::from_slice(&index_bytes)
        .with_context(|| format!("parse asset index {}", asset_index_path.display()))?;
    let objects = index
        .get("objects")
        .and_then(|o| o.as_object())
        .ok_or_else(|| anyhow!("asset index has no \"objects\" map"))?;

    // Jar entry sizes, keyed the way the index names things (no `assets/`).
    let file = File::open(client_path)
        .with_context(|| format!("open client jar {}", client_path.display()))?;
    let mut archive = zip::ZipArchive::new(file)
        .with_context(|| format!("read zip {}", client_path.display()))?;
    let mut jar_sizes: BTreeMap<String, u64> = BTreeMap::new();
    for i in 0..archive.len() {
        let entry = archive
            .by_index(i)
            .with_context(|| format!("read zip entry {i} of {}", client_path.display()))?;
        let name = entry.name();
        if let Some(key) = name.strip_prefix("assets/") {
            jar_sizes.insert(key.to_string(), entry.size());
        }
    }

    let mut wanted: Vec<FetchedObject> = Vec::new();
    for (name, meta) in objects {
        let Some(size) = meta.get("size").and_then(Value::as_u64) else {
            continue;
        };
        let jar_size = jar_sizes.get(name.as_str()).copied();
        let shadowed_stub = jar_size.is_some_and(|jar| jar != size);
        let required = REQUIRED_OBJECT_NAMES.contains(&name.as_str());
        if !shadowed_stub && !required {
            // Either byte-for-byte the same asset in both places (the jar copy is
            // fine), or index-only and nothing refuses to start without it.
            continue;
        }
        let hash = meta
            .get("hash")
            .and_then(|h| h.as_str())
            .ok_or_else(|| anyhow!("asset index entry {name:?} has no hash"))?;
        wanted.push(FetchedObject {
            name: name.clone(),
            hash: hash.to_string(),
            // `jar_size` is 0 for a required-but-unshadowed object, which is what
            // the summary prints and is honest: there is no jar copy.
            jar_size: jar_size.unwrap_or(0),
            size,
            downloaded: false,
        });
    }
    wanted.sort_by(|a, b| a.name.cmp(&b.name));

    // Every name in `REQUIRED_OBJECT_NAMES` must have been found. A typo there
    // would otherwise silently fetch nothing and leave audio dead exactly as
    // before, which is the failure this whole change exists to stop.
    for required in REQUIRED_OBJECT_NAMES {
        if !wanted.iter().any(|o| o.name == *required) {
            bail!(
                "required asset object {required:?} is not in {} — the name is \
                 wrong, or this version's index does not carry it",
                asset_index_path.display()
            );
        }
    }

    for object in &mut wanted {
        let downloaded = ensure_object(version_cache, objects, &object.name, force)?;
        object.downloaded = downloaded;
    }

    Ok(wanted)
}

/// The `.ogg` corpus a `fetch-sounds` run should ensure is present, split out of
/// `sounds.json` by [`plan_sound_corpus`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SoundCorpus {
    /// Index names to fetch, sorted, with their declared sizes.
    pub wanted: Vec<(String, u64)>,
    /// Index names deliberately left out (music and records under the default
    /// policy), sorted, with their declared sizes. Reported, never fetched.
    pub excluded: Vec<(String, u64)>,
    /// `.ogg` objects in the index that **no** `sounds.json` event references.
    /// Neither mode fetches these: nothing can select them.
    pub unreferenced: Vec<(String, u64)>,
    /// Distinct events walked (`sounds.json` top-level keys).
    pub events: usize,
}

impl SoundCorpus {
    /// Total declared bytes of [`Self::wanted`].
    #[must_use]
    pub fn wanted_bytes(&self) -> u64 {
        self.wanted.iter().map(|(_, size)| size).sum()
    }

    /// Total declared bytes of [`Self::excluded`].
    #[must_use]
    pub fn excluded_bytes(&self) -> u64 {
        self.excluded.iter().map(|(_, size)| size).sum()
    }
}

/// True for a `sounds.json` event key that is background music or a music disc.
///
/// This is the **whole** exclusion policy, and it is a two-token prefix over the
/// event namespace rather than a list of files, because a file list rots at the
/// next version bump and a namespace does not. 26.2's music events are `music.*`
/// (biome/menu/credits tracks) and `music_disc.*` (jukebox records); the bare key
/// `music` is accepted defensively in case a version ever ships one.
pub fn is_music_event(event: &str) -> bool {
    event == "music" || event.starts_with("music.") || event.starts_with("music_disc.")
}

/// The asset-index name of a `sounds.json` sound name: `entity.zombie.hurt`'s
/// `"mob/zombie/hurt1"` becomes `minecraft/sounds/mob/zombie/hurt1.ogg`.
///
/// A name may carry its own namespace (`somepack:foo/bar`), in which case that
/// namespace replaces `minecraft`. Measured on 26.2: all 4843 distinct names
/// resolve to a real index entry through this rule, with zero misses — which is
/// what makes the derivation trustworthy rather than approximate.
pub fn sound_object_name(sound_name: &str) -> String {
    let (namespace, path) = match sound_name.split_once(':') {
        Some((namespace, path)) => (namespace, path),
        None => ("minecraft", sound_name),
    };
    format!("{namespace}/sounds/{path}.ogg")
}

/// Decide which `.ogg` objects to fetch, **derived from `sounds.json`** rather
/// than from a hand-written list.
///
/// # Why this is derived and what the derivation is
///
/// The index declares 4871 `.ogg` objects totalling 375 MB, so an unconditional
/// fetch is not acceptable and a curated file list would be stale within one
/// version. Instead this walks every event in `sounds.json` (1968 of them on
/// 26.2), collects each entry's sound name, and resolves it to an index name.
/// `"type": "event"` entries are indirections to another event and contribute no
/// sample of their own, so they are skipped — the event they name is walked in its
/// own right.
///
/// A sample is **excluded** only when *every* event that references it is a music
/// event ([`is_music_event`]). "Every", not "any": a sample shared between a music
/// event and a world event still has to be fetched, and phrasing the rule the
/// other way round would silently drop it.
///
/// # What the default covers, measured on 26.2
///
/// | set | objects | bytes |
/// |---|---|---|
/// | fetched (default) | 4751 | 80.14 MB |
/// | excluded: 70 music tracks + 22 records | 92 | 293.23 MB |
/// | referenced by no event at all | 28 | — |
///
/// So the default is **every sample any non-music event can select**: mobs,
/// blocks, items, entities, steps, digs, liquid, UI, notes, enchanting,
/// fireworks, minecarts, portals — *and* all six biome ambience loops, which is
/// the reason the rule is "music events" and not vanilla's `"stream": true` flag.
/// `stream: true` is the other candidate derivation and it is cheaper to state,
/// but it selects 98 samples including those six nether/underwater loops, so it
/// would silence cave and nether ambience to save 2.9 MB. Measured, not assumed.
///
/// `include_music` (the `--all` flag) folds the excluded set back in: 4843
/// objects, 373.37 MB. The 28 unreferenced objects are never fetched in either
/// mode — no event can select them, so they would be 28 downloads no code path
/// can reach.
///
/// # Errors
///
/// Returns an error when `sounds_json` is not a JSON object, when it is empty, or
/// when a resolved name is not in the asset index — the last of which means the
/// resolution rule is wrong for this version and must be fixed rather than
/// worked around.
pub fn plan_sound_corpus(
    index: &serde_json::Map<String, Value>,
    sounds_json: &[u8],
    include_music: bool,
) -> Result<SoundCorpus> {
    let parsed: Value =
        serde_json::from_slice(sounds_json).context("parse minecraft/sounds.json")?;
    let events = parsed
        .as_object()
        .ok_or_else(|| anyhow!("sounds.json is not a JSON object of event -> definition"))?;
    if events.is_empty() {
        bail!("sounds.json declares no events");
    }

    // name -> whether every referencing event so far is a music event. Starting
    // from `true` and `&&`-ing keeps the "only music references it" semantics.
    let mut only_music: BTreeMap<String, bool> = BTreeMap::new();
    for (event, definition) in events {
        let Some(entries) = definition.get("sounds").and_then(Value::as_array) else {
            continue;
        };
        let music = is_music_event(event);
        for entry in entries {
            let name = match entry {
                // The shorthand form: a bare string is a sound name.
                Value::String(name) => name.as_str(),
                Value::Object(map) => {
                    // `"type": "event"` names another event, not a file.
                    if map.get("type").and_then(Value::as_str).unwrap_or("sound") != "sound" {
                        continue;
                    }
                    match map.get("name").and_then(Value::as_str) {
                        Some(name) => name,
                        None => continue,
                    }
                }
                _ => continue,
            };
            let object = sound_object_name(name);
            only_music
                .entry(object)
                .and_modify(|flag| *flag = *flag && music)
                .or_insert(music);
        }
    }

    let mut corpus = SoundCorpus {
        events: events.len(),
        ..SoundCorpus::default()
    };
    for (name, music_only) in &only_music {
        let size = index
            .get(name)
            .and_then(|meta| meta.get("size"))
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                anyhow!(
                    "sounds.json references {name:?}, which the asset index does not \
                     declare — the sound-name resolution rule is wrong for this version"
                )
            })?;
        if *music_only && !include_music {
            corpus.excluded.push((name.clone(), size));
        } else {
            corpus.wanted.push((name.clone(), size));
        }
    }

    // Index-only `.ogg` objects no event mentions. Reported so the count is
    // visible rather than looking like a shortfall in the plan.
    for (name, meta) in index {
        if !name.ends_with(".ogg") || only_music.contains_key(name) {
            continue;
        }
        let size = meta.get("size").and_then(Value::as_u64).unwrap_or(0);
        corpus.unreferenced.push((name.clone(), size));
    }

    // `wanted` and `excluded` came out of a `BTreeMap`, so they are already sorted;
    // `unreferenced` came out of the index (a `serde_json::Map`, insertion-ordered
    // unless the `preserve_order` feature is off), so sort it explicitly. Stable
    // output matters: the summary is read by a human comparing two runs.
    corpus.unreferenced.sort();
    Ok(corpus)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FetchSoundsSummary {
    pub asset_index_path: PathBuf,
    /// The plan this run executed.
    pub corpus: SoundCorpus,
    /// How many objects this run actually downloaded.
    pub downloaded: usize,
    /// How many were already present and SHA-1 verified.
    pub cached: usize,
    /// Whether music and records were included (`--all`).
    pub included_music: bool,
}

impl FetchSoundsSummary {
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = format!(
            "asset index: {}\nsounds.json: {} events\nsamples: {} ({} downloaded, {} cached), {:.2} MB\n",
            self.asset_index_path.display(),
            self.corpus.events,
            self.corpus.wanted.len(),
            self.downloaded,
            self.cached,
            self.corpus.wanted_bytes() as f64 / 1e6,
        );
        if self.included_music {
            out.push_str("music and records: included (--all)\n");
        } else {
            out.push_str(&format!(
                "music and records: {} objects, {:.2} MB NOT fetched — re-run with --all for \
                 background music and jukebox discs\n",
                self.corpus.excluded.len(),
                self.corpus.excluded_bytes() as f64 / 1e6,
            ));
        }
        if !self.corpus.unreferenced.is_empty() {
            out.push_str(&format!(
                "unreferenced: {} .ogg objects no sounds.json event names (never fetched)\n",
                self.corpus.unreferenced.len()
            ));
        }
        out
    }
}

/// Default worker count for [`fetch_sounds`].
///
/// Each object is one `curl` process, and the corpus averages ~17 KB per file, so
/// wall time is dominated by connection setup rather than bandwidth: serial would
/// be ~4751 round trips. Twelve is well within what `resources.download.minecraft.net`
/// serves without throttling and keeps a cold fetch in the low minutes.
pub const SOUND_FETCH_JOBS: usize = 12;

/// Ensure the `.ogg` corpus [`plan_sound_corpus`] selected is on disk, SHA-1
/// verified against the asset index.
///
/// This is deliberately **not** part of `fetch-assets`: 80 MB (or 373 with
/// `--all`) must be an explicit act, and a missing sample degrades honestly — one
/// silent sound — where a `client.jar` stub lies. See
/// [`fetch_shadowed_objects`] for the other half of that boundary.
///
/// Every object goes through [`ensure_object`], which verifies the SHA-1 the
/// index declares; there is no second fetcher here. A re-run of a complete fetch
/// downloads nothing: it re-hashes what is on disk (80 MB of SHA-1, well under a
/// second) and reports every object as cached.
///
/// # Errors
///
/// Returns an error when the version cache has no single `asset-index-*.json`,
/// when `minecraft/sounds.json` is not in the store (run `fetch-assets` first),
/// or on the first download or SHA-1 failure — reported with the object that
/// failed, not as a bare count.
pub fn fetch_sounds(
    workspace_root: &Path,
    minecraft_version: &str,
    include_music: bool,
    force: bool,
    jobs: usize,
) -> Result<FetchSoundsSummary> {
    let version_cache = workspace_root
        .join(".cache")
        .join("mc")
        .join(minecraft_version);
    let asset_index_path = find_cached_asset_index(&version_cache)?;
    let index_bytes = std::fs::read(&asset_index_path)
        .with_context(|| format!("read asset index {}", asset_index_path.display()))?;
    let index_json: Value = serde_json::from_slice(&index_bytes)
        .with_context(|| format!("parse asset index {}", asset_index_path.display()))?;
    let index = index_json
        .get("objects")
        .and_then(|objects| objects.as_object())
        .ok_or_else(|| anyhow!("asset index has no \"objects\" map"))?;

    // sounds.json is the plan's input, so it has to be present first. It is in
    // `REQUIRED_OBJECT_NAMES`, meaning `fetch-assets` already put it there;
    // ensuring it again here makes this command usable on its own.
    ensure_object(&version_cache, index, "minecraft/sounds.json", false)
        .context("ensure minecraft/sounds.json (the corpus is derived from it)")?;
    let sounds_hash = index
        .get("minecraft/sounds.json")
        .and_then(|meta| meta.get("hash"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("asset index has no hash for minecraft/sounds.json"))?;
    let sounds_path = version_cache
        .join("objects")
        .join(&sounds_hash[0..2])
        .join(sounds_hash);
    let sounds_json = std::fs::read(&sounds_path)
        .with_context(|| format!("read {}", sounds_path.display()))?;

    let corpus = plan_sound_corpus(index, &sounds_json, include_music)?;
    let total = corpus.wanted.len();
    println!(
        "fetch-sounds: {total} samples, {:.2} MB declared ({} events in sounds.json)",
        corpus.wanted_bytes() as f64 / 1e6,
        corpus.events
    );
    if !include_music {
        println!(
            "  skipping {} music/record objects ({:.2} MB); --all includes them",
            corpus.excluded.len(),
            corpus.excluded_bytes() as f64 / 1e6
        );
    }

    let jobs = jobs.max(1);
    let cursor = std::sync::atomic::AtomicUsize::new(0);
    let done = std::sync::atomic::AtomicUsize::new(0);
    let downloaded = std::sync::atomic::AtomicUsize::new(0);
    let failure: std::sync::Mutex<Option<anyhow::Error>> = std::sync::Mutex::new(None);

    std::thread::scope(|scope| {
        for _ in 0..jobs {
            scope.spawn(|| {
                loop {
                    // A failure anywhere stops every worker: the first error is
                    // the useful one, and 4750 more retries after a dead CDN are
                    // not.
                    if failure.lock().is_ok_and(|slot| slot.is_some()) {
                        return;
                    }
                    let next = cursor.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let Some((name, _size)) = corpus.wanted.get(next) else {
                        return;
                    };
                    match ensure_object(&version_cache, index, name, force) {
                        Ok(true) => {
                            downloaded.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        }
                        Ok(false) => {}
                        Err(error) => {
                            if let Ok(mut slot) = failure.lock() {
                                slot.get_or_insert(error);
                            }
                            return;
                        }
                    }
                    let finished = done.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    // One line per 250 objects: enough to see it moving, few
                    // enough not to bury the summary.
                    if finished % 250 == 0 || finished == total {
                        println!(
                            "  {finished}/{total} ({} downloaded)",
                            downloaded.load(std::sync::atomic::Ordering::SeqCst)
                        );
                    }
                }
            });
        }
    });

    if let Some(error) = failure.into_inner().ok().flatten() {
        return Err(error);
    }

    let downloaded = downloaded.into_inner();
    Ok(FetchSoundsSummary {
        asset_index_path,
        downloaded,
        cached: total - downloaded,
        corpus,
        included_music: include_music,
    })
}

/// The single `asset-index-*.json` already in a version cache.
///
/// `fetch-assets` learns the index id from Mojang's manifest; this command works
/// off what is on disk instead, so it needs no network before it can plan. Zero
/// or several candidates is an error rather than a guess — several client versions
/// coexist under `.cache/mc` and "first match wins over a shared directory" is a
/// known landmine here.
///
/// # Errors
///
/// Returns an error when the directory is unreadable, holds no
/// `asset-index-*.json`, or holds more than one.
fn find_cached_asset_index(version_cache: &Path) -> Result<PathBuf> {
    let entries = std::fs::read_dir(version_cache).with_context(|| {
        format!(
            "read {} — run: cargo run -p xtask -- fetch-assets --version <version>",
            version_cache.display()
        )
    })?;
    let mut matches: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("asset-index-") && name.ends_with(".json") {
            matches.push(entry.path());
        }
    }
    matches.sort();
    match matches.len() {
        0 => bail!(
            "no asset-index-*.json in {} — run: cargo run -p xtask -- fetch-assets --version <version>",
            version_cache.display()
        ),
        1 => Ok(matches.remove(0)),
        n => bail!(
            "{n} asset-index-*.json files in {}; refusing to guess",
            version_cache.display()
        ),
    }
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

/// The sixteen versions this workspace tracks: the latest patch of every
/// major Minecraft release from 1.7.10 through the current 26.2.
///
/// This is the one place that list is spelled out; [`version_table_report`]
/// walks it in order, so adding or removing a target version is a one-line
/// change here plus a `cargo run -p xtask -- version-table` regen.
pub const EPIC_343_VERSIONS: &[&str] = &[
    "1.7.10", "1.8.9", "1.9.4", "1.10.2", "1.11.2", "1.12.2", "1.13.2", "1.14.4", "1.15.2",
    "1.16.5", "1.17.1", "1.18.2", "1.19.4", "1.20.6", "1.21.11", "26.2",
];

/// Relative path (from the workspace root) to `vendor/minecraft-data`'s
/// cross-version protocol/data-version index. Covers 1.8 (as `1.7`/`1.7.10`
/// entries in this specific file) through 1.21.11, plus — measured directly,
/// contrary to this repo's usual "no 26.x data" caveat about
/// `vendor/minecraft-data` — a 26.2 entry too. See `docs/version-table.md`
/// for what that means and does not mean.
const MINECRAFT_DATA_PROTOCOL_VERSIONS: &str =
    "vendor/minecraft-data/data/pc/common/protocolVersions.json";

/// Where one field of a [`VersionTableEntry`] was sourced from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VersionSource {
    /// `version.json` at the root of the vanilla server jar. Present since
    /// 18w47b; the oldest version in `EPIC_343_VERSIONS` that ships it is
    /// 1.14.4. Authoritative per `CLAUDE.md`'s "Data sources, in order".
    JarVersionJson,
    /// `vendor/minecraft-data`'s `protocolVersions.json`. Used only for
    /// versions whose jar predates `version.json`; cross-check-grade, never
    /// authoritative, per `CLAUDE.md`.
    MinecraftData,
}

impl VersionSource {
    #[must_use]
    pub const fn as_rust_expr(self) -> &'static str {
        match self {
            Self::JarVersionJson => "Source::JarVersionJson",
            Self::MinecraftData => "Source::MinecraftData",
        }
    }
}

/// One resolved row of the version table.
#[derive(Clone, Debug)]
pub struct VersionTableEntry {
    pub minecraft_version: String,
    pub protocol_version: i32,
    pub data_version: i32,
    /// ISO-8601 `releaseTime` from Mojang's version manifest.
    pub release_date: String,
    pub protocol_source: VersionSource,
    pub data_version_source: VersionSource,
    /// True when both a jar `version.json` and a `minecraft-data` entry were
    /// available and cross-checked in agreement. False when only one source
    /// was available (nothing to cross-check) or the two disagreed — a
    /// disagreement is a hard error in [`resolve_version_table_entry`], not a
    /// row you can see with this flag set, so `false` here just means
    /// single-sourced.
    pub cross_checked: bool,
}

/// Minimal parsed shape of Mojang's per-version JSON, just the pieces
/// `version-table` needs beyond what [`parse_asset_downloads`] already
/// extracts.
fn manifest_release_time(manifest_json: &str, minecraft_version: &str) -> Result<String> {
    let root: Value =
        serde_json::from_str(manifest_json).context("parse Mojang version manifest")?;
    let versions = root
        .get("versions")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("version manifest is missing versions array"))?;

    for version in versions {
        if version.get("id").and_then(Value::as_str) == Some(minecraft_version) {
            return version
                .get("releaseTime")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .ok_or_else(|| anyhow!("version {minecraft_version} is missing releaseTime"));
        }
    }

    bail!("Minecraft version {minecraft_version} was not found in Mojang version manifest")
}

/// A `(protocol_version, data_version)` pair sourced from the vanilla server
/// jar's embedded `version.json`, when present.
struct JarVersionInfo {
    protocol_version: i32,
    data_version: i32,
}

/// Reads `version.json` from the root of a vanilla server jar, if present.
///
/// Returns `Ok(None)` for jars that predate the file (18w47b / 1.14) rather
/// than treating absence as an error — the caller falls back to
/// `minecraft-data` in that case.
fn read_jar_version_json(jar_path: &Path) -> Result<Option<JarVersionInfo>> {
    let file =
        File::open(jar_path).with_context(|| format!("open jar {}", jar_path.display()))?;
    let mut archive = zip::ZipArchive::new(file)
        .with_context(|| format!("read zip {}", jar_path.display()))?;

    let mut entry = match archive.by_name("version.json") {
        Ok(entry) => entry,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| {
                format!("read version.json from {}", jar_path.display())
            });
        }
    };

    let mut contents = String::new();
    entry
        .read_to_string(&mut contents)
        .with_context(|| format!("read version.json contents from {}", jar_path.display()))?;
    drop(entry);

    let root: Value = serde_json::from_str(&contents)
        .with_context(|| format!("parse version.json from {}", jar_path.display()))?;
    let protocol_version = root
        .get("protocol_version")
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("{}: version.json missing protocol_version", jar_path.display()))?;
    let data_version = root
        .get("world_version")
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("{}: version.json missing world_version", jar_path.display()))?;

    Ok(Some(JarVersionInfo {
        protocol_version: i32::try_from(protocol_version)
            .with_context(|| format!("protocol_version {protocol_version} out of i32 range"))?,
        data_version: i32::try_from(data_version)
            .with_context(|| format!("world_version {data_version} out of i32 range"))?,
    }))
}

/// One row of `vendor/minecraft-data`'s `protocolVersions.json`, filtered to
/// the fields `version-table` needs.
struct MinecraftDataProtocolEntry {
    protocol_version: i32,
    data_version: i32,
}

/// Looks up `minecraft_version` in `vendor/minecraft-data`'s
/// `protocolVersions.json` by exact `minecraftVersion` string match.
///
/// Returns `Ok(None)` when the file has no entry for that exact version
/// string (this repo's cross-check source, never its authority — see
/// `CLAUDE.md`'s "Data sources, in order").
fn minecraft_data_protocol_entry(
    workspace_root: &Path,
    minecraft_version: &str,
) -> Result<Option<MinecraftDataProtocolEntry>> {
    let path = workspace_root.join(MINECRAFT_DATA_PROTOCOL_VERSIONS);
    let json = std::fs::read_to_string(&path)
        .with_context(|| format!("read {}", path.display()))?;
    let entries: Vec<Value> =
        serde_json::from_str(&json).with_context(|| format!("parse {}", path.display()))?;

    for entry in &entries {
        if entry.get("minecraftVersion").and_then(Value::as_str) != Some(minecraft_version) {
            continue;
        }
        let protocol_version = entry
            .get("version")
            .and_then(Value::as_i64)
            .ok_or_else(|| anyhow!("{}: entry for {minecraft_version} missing version", path.display()))?;
        let data_version = entry
            .get("dataVersion")
            .and_then(Value::as_i64)
            .ok_or_else(|| {
                anyhow!("{}: entry for {minecraft_version} missing dataVersion", path.display())
            })?;
        return Ok(Some(MinecraftDataProtocolEntry {
            protocol_version: i32::try_from(protocol_version)
                .with_context(|| format!("protocol_version {protocol_version} out of i32 range"))?,
            data_version: i32::try_from(data_version)
                .with_context(|| format!("dataVersion {data_version} out of i32 range"))?,
        }));
    }

    Ok(None)
}

/// Resolves one [`VersionTableEntry`] for `minecraft_version`.
///
/// Prefers the jar's `version.json` (authoritative) when a server jar is
/// already cached at `.cache/mc/<minecraft_version>/server.jar` — or,
/// with `fetch_missing`, downloads it first via [`fetch_version`]. Falls
/// back to `minecraft-data` when the jar has no `version.json` (every
/// version in `EPIC_343_VERSIONS` at or before 1.13.2, confirmed empirically:
/// see `docs/version-table.md`). When both sources are available, disagreement
/// is a hard error — this table has no silent-drift path.
fn resolve_version_table_entry(
    workspace_root: &Path,
    manifest_json: &str,
    minecraft_version: &str,
    fetch_missing: bool,
) -> Result<VersionTableEntry> {
    let release_date = manifest_release_time(manifest_json, minecraft_version)?;

    let jar_path = workspace_root
        .join(".cache")
        .join("mc")
        .join(minecraft_version)
        .join("server.jar");

    if fetch_missing && !jar_path.exists() {
        fetch_version(workspace_root, minecraft_version, false)
            .with_context(|| format!("fetch server.jar for {minecraft_version}"))?;
    }

    let jar_info = if jar_path.exists() {
        read_jar_version_json(&jar_path)?
    } else {
        None
    };
    let data_entry = minecraft_data_protocol_entry(workspace_root, minecraft_version)?;

    let (protocol_version, protocol_source) = match (&jar_info, &data_entry) {
        (Some(jar), Some(data)) if jar.protocol_version != data.protocol_version => bail!(
            "{minecraft_version}: jar version.json protocol_version {} disagrees with minecraft-data {}",
            jar.protocol_version,
            data.protocol_version
        ),
        (Some(jar), _) => (jar.protocol_version, VersionSource::JarVersionJson),
        (None, Some(data)) => (data.protocol_version, VersionSource::MinecraftData),
        (None, None) => bail!(
            "{minecraft_version}: no protocol_version source available (no cached jar with \
             version.json, and no minecraft-data entry) — re-run with --fetch-missing or add a \
             minecraft-data entry rather than guessing"
        ),
    };

    let (data_version, data_version_source) = match (&jar_info, &data_entry) {
        (Some(jar), Some(data)) if jar.data_version != data.data_version => bail!(
            "{minecraft_version}: jar version.json world_version {} disagrees with minecraft-data \
             dataVersion {}",
            jar.data_version,
            data.data_version
        ),
        (Some(jar), _) => (jar.data_version, VersionSource::JarVersionJson),
        (None, Some(data)) => (data.data_version, VersionSource::MinecraftData),
        (None, None) => bail!(
            "{minecraft_version}: no data_version source available (no cached jar with \
             version.json, and no minecraft-data entry) — re-run with --fetch-missing or add a \
             minecraft-data entry rather than guessing"
        ),
    };

    Ok(VersionTableEntry {
        minecraft_version: minecraft_version.to_owned(),
        protocol_version,
        data_version,
        release_date,
        protocol_source,
        data_version_source,
        cross_checked: jar_info.is_some() && data_entry.is_some(),
    })
}

/// Resolves every row of the epic-343 version table, in `EPIC_343_VERSIONS`
/// order.
///
/// Fetches Mojang's version manifest once. With `fetch_missing`, also
/// downloads (and SHA-1-verifies, via [`fetch_version`]) any target
/// version's server jar not already cached under `.cache/mc/<version>/` —
/// this is the only network-heavy path and is off by default specifically so
/// routine `--check` runs do not silently pull a dozen jars.
pub fn version_table_report(
    workspace_root: &Path,
    fetch_missing: bool,
) -> Result<Vec<VersionTableEntry>> {
    let manifest_json = curl_to_string(VERSION_MANIFEST_URL)?;
    EPIC_343_VERSIONS
        .iter()
        .map(|minecraft_version| {
            resolve_version_table_entry(
                workspace_root,
                &manifest_json,
                minecraft_version,
                fetch_missing,
            )
        })
        .collect()
}

const VERSION_TABLE_OUT: &str = "crates/lodestone-registry/src/generated/version_table.rs";

fn version_table_out_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join(VERSION_TABLE_OUT)
}

/// Renders [`VersionTableEntry`] rows as the checked-in generated Rust
/// source, matching the `generated_*.rs` convention used throughout
/// `crates/versions/*/src/generated/`.
pub fn render_version_table_source(entries: &[VersionTableEntry]) -> Result<String> {
    let mut source = String::new();
    writeln!(
        source,
        "// @generated by `cargo run -p xtask -- version-table`. DO NOT EDIT BY HAND.\n\
         // Regenerate with `cargo run -p xtask -- version-table [--fetch-missing]` (see\n\
         // `crates/lodestone-registry/src/version_table.rs` module docs and\n\
         // `docs/version-table.md` for provenance and how to refresh).\n\
         //! Generated version table: the latest patch of every major Minecraft\n\
         //! release this workspace tracks, 1.7.10 through 26.2.\n\
         //! See [`crate::version_table`] for the public API and full provenance notes.\n"
    )?;
    writeln!(
        source,
        "/// Where one field of an [`Entry`] was sourced from."
    )?;
    writeln!(
        source,
        "#[derive(Clone, Copy, Debug, Eq, PartialEq)]\npub enum Source {{"
    )?;
    writeln!(
        source,
        "    /// `version.json` embedded in the vanilla server jar (present since 18w47b / 1.14)."
    )?;
    writeln!(source, "    JarVersionJson,")?;
    writeln!(
        source,
        "    /// `vendor/minecraft-data`'s `protocolVersions.json`; used only where the jar\n    /// predates `version.json`."
    )?;
    writeln!(source, "    MinecraftData,")?;
    writeln!(source, "}}\n")?;

    writeln!(source, "/// One row of the version table.")?;
    writeln!(source, "#[derive(Clone, Copy, Debug)]\npub struct Entry {{")?;
    writeln!(source, "    pub minecraft_version: &'static str,")?;
    writeln!(source, "    pub protocol_version: i32,")?;
    writeln!(source, "    pub data_version: i32,")?;
    writeln!(
        source,
        "    /// ISO-8601 `releaseTime` from Mojang's version_manifest_v2.json.\n    pub release_date: &'static str,"
    )?;
    writeln!(source, "    pub protocol_source: Source,")?;
    writeln!(source, "    pub data_version_source: Source,")?;
    writeln!(
        source,
        "    /// Whether jar and minecraft-data agreed (both present and equal). False means\n    /// only one source was available; the two never disagree in a committed row.\n    pub cross_checked: bool,"
    )?;
    writeln!(source, "}}\n")?;

    writeln!(
        source,
        "pub static VERSIONS: [Entry; {}] = [",
        entries.len()
    )?;
    for entry in entries {
        writeln!(
            source,
            "    Entry {{ minecraft_version: {:?}, protocol_version: {}, data_version: {}, release_date: {:?}, protocol_source: {}, data_version_source: {}, cross_checked: {} }},",
            entry.minecraft_version,
            entry.protocol_version,
            entry.data_version,
            entry.release_date,
            entry.protocol_source.as_rust_expr(),
            entry.data_version_source.as_rust_expr(),
            entry.cross_checked,
        )?;
    }
    writeln!(source, "];")?;

    format_rust_source(&source)
}

/// Regenerates `crates/lodestone-registry/src/generated/version_table.rs`
/// from the network + any cached jars, and writes it.
pub fn generate_version_table(workspace_root: &Path, fetch_missing: bool) -> Result<PathBuf> {
    let entries = version_table_report(workspace_root, fetch_missing)?;
    let source = render_version_table_source(&entries)?;
    let out_path = version_table_out_path(workspace_root);
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create output directory {}", parent.display()))?;
    }
    std::fs::write(&out_path, source)
        .with_context(|| format!("write generated version table to {}", out_path.display()))?;
    Ok(out_path)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VersionTableCheck {
    pub out_path: PathBuf,
    pub summary: String,
    identical: bool,
}

impl VersionTableCheck {
    #[must_use]
    pub const fn is_identical(&self) -> bool {
        self.identical
    }
}

/// Recomputes the version table and compares it against the checked-in file
/// without writing, for use as a drift-guard CI check.
pub fn check_version_table(workspace_root: &Path, fetch_missing: bool) -> Result<VersionTableCheck> {
    let entries = version_table_report(workspace_root, fetch_missing)?;
    let expected = render_version_table_source(&entries)?;
    let out_path = version_table_out_path(workspace_root);
    let actual = std::fs::read_to_string(&out_path)
        .with_context(|| format!("read generated version table at {}", out_path.display()))?;

    if actual == expected {
        return Ok(VersionTableCheck {
            out_path,
            summary: "version_table.rs is up to date".to_owned(),
            identical: true,
        });
    }

    Ok(VersionTableCheck {
        summary: packet_id_diff_summary(&out_path, &expected, &actual),
        out_path,
        identical: false,
    })
}

// ---------------------------------------------------------------------------
// docs-index: `docs/README.md` generated from every doc's own H1 + summary
// ---------------------------------------------------------------------------
//
// `docs/README.md` was the single most contended file in this repo -- every
// agent that lands a doc needs one index line in it, so it was hand-edited
// under a shared index and hand-edited files under a shared index are exactly
// what CLAUDE.md's repo-hazards section warns about (a stale staged blob of
// this file has, historically, deleted another agent's index bullet). The fix
// is to stop hand-editing it: `CLAUDE.md`'s own docs convention already
// requires every doc to open with a "what it is" summary, so the index is a
// pure function of the doc tree, not a thing anyone should ever type by hand
// again.
//
// A doc's summary is the first paragraph under a `## What it is` or
// `## What this is` heading (the two spellings actually used across this
// repo's 123 existing docs, checked before picking them -- 113 use one of the
// two, and no doc uses a spelling other than these two). Docs written before
// that heading convention existed fall back to the first paragraph directly
// under the H1. A doc with neither -- so `extract_doc_summary` cannot find
// any prose to quote -- fails loudly naming the file, per the acceptance
// criterion: no doc gets a blank index line.

/// `docs/*.md` files that are not part of the generated index at all --
/// companion docs `CLAUDE.md` and `docs/README.md`'s own prose already link
/// to directly, structurally different from a per-subsystem doc (an ordered
/// work queue, not the record of a landed feature). Kept as an explicit,
/// documented exception rather than an inferred one.
const DOCS_INDEX_SKIP: &[&str] = &["backlog.md"];

/// Top-level `docs/*.md` files that belong in the "Plans and research" group
/// (phased plans and read-only diagnoses) rather than the main per-subsystem
/// list, mirroring the hand-curated split the pre-generator `docs/README.md`
/// used. Everything under `docs/research/` joins them automatically.
const DOCS_INDEX_PLANS_AND_RESEARCH: &[&str] = &["worldgen-plan.md", "worldgen-parity.md"];

struct DocIndexEntry {
    /// Repo-relative link target, e.g. `./accounts.md` or `./roadmap/protocol.md`.
    link: String,
    title: String,
    summary: String,
}

/// True for a real ATX heading line (`#` through `######`, followed by a
/// space or end of line, per CommonMark) -- deliberately **not** just
/// `starts_with('#')`. A prose line beginning with an issue reference like
/// `#12/#72/#98/#121)` also starts with `#`, and treating that as a heading
/// silently truncated `docs/research/combat-scope.md`'s summary mid-sentence
/// the first time this ran -- caught by eye in the generated
/// `docs/README.md`, not by any test, which is why this got its own name
/// instead of staying an inline check.
fn is_atx_heading(trimmed_line: &str) -> bool {
    let hashes = trimmed_line.chars().take_while(|&c| c == '#').count();
    (1..=6).contains(&hashes) && matches!(trimmed_line.as_bytes().get(hashes), None | Some(b' '))
}

/// Extracts a doc's H1 title and a one-paragraph summary. See the module note
/// above for the extraction rule and why these two heading spellings.
fn extract_doc_summary(text: &str, rel_path: &str) -> Result<(String, String)> {
    let lines: Vec<&str> = text.lines().collect();

    let h1_idx = lines
        .iter()
        .position(|l| l.starts_with("# "))
        .ok_or_else(|| anyhow!("{rel_path}: no H1 (`# Title`) heading found"))?;
    let title = lines[h1_idx][2..].trim().to_string();
    if title.is_empty() {
        bail!("{rel_path}: H1 heading has no title text");
    }

    let is_summary_heading = |l: &str| {
        let t = l.trim();
        t.eq_ignore_ascii_case("## what it is") || t.eq_ignore_ascii_case("## what this is")
    };

    // Search the whole doc for the heading (not just immediately after the
    // H1): several docs carry a long preamble -- issue status, corrections --
    // before reaching it.
    let body_start = lines[h1_idx + 1..]
        .iter()
        .position(|l| is_summary_heading(l))
        .map(|i| h1_idx + 1 + i + 1);
    let scan_from = body_start.unwrap_or(h1_idx + 1);

    let mut para: Vec<&str> = Vec::new();
    for &line in &lines[scan_from..] {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if para.is_empty() {
                continue; // skip leading blank lines
            }
            break; // paragraph ended
        }
        if is_atx_heading(trimmed) {
            break; // hit a heading before (or right after) any prose
        }
        para.push(trimmed);
    }

    if para.is_empty() {
        bail!(
            "{rel_path}: no usable summary paragraph found (no prose under a \
             `## What it is`/`## What this is` heading, and none directly under the H1) -- \
             add one instead of leaving this doc out of the index"
        );
    }

    Ok((title, para.join(" ")))
}

/// Word-wraps one index bullet at a fixed width, matching the hand-authored
/// file's rough line length so the generated output stays readable as plain
/// text, not just as rendered markdown.
fn write_docs_index_entry(out: &mut String, entry: &DocIndexEntry) {
    const WIDTH: usize = 86;
    let mut line = format!("- [{}]({}) —", entry.title, entry.link);
    for word in entry.summary.split_whitespace() {
        if line.len() + 1 + word.len() > WIDTH {
            out.push_str(&line);
            out.push('\n');
            line = format!("  {word}");
        } else {
            line.push(' ');
            line.push_str(word);
        }
    }
    out.push_str(&line);
    out.push('\n');
}

/// [`read_md_dir_sorted`] for a directory that is allowed not to exist.
///
/// Only `NotFound` is tolerated; every other error still propagates. The
/// distinction matters: a directory that is absent has nothing to omit, whereas
/// a directory that fails to read for any other reason would be silently
/// skipped, which is the failure mode the `docs/plans/` comment below records —
/// the drift gate proves the index is *consistent* with the docs, never that it
/// *covers* them.
fn read_md_dir_sorted_optional(dir: &Path) -> Result<Vec<PathBuf>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    read_md_dir_sorted(dir)
}

fn read_md_dir_sorted(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("md"))
        .collect();
    files.sort();
    Ok(files)
}

/// Builds `docs/README.md`'s full contents from the doc tree. Deterministic:
/// a pure function of what is on disk under `docs/`, so two runs against the
/// same tree always produce byte-identical output -- the property the
/// drift-guard test below depends on.
pub fn generate_docs_index(workspace_root: &Path) -> Result<String> {
    let docs_dir = workspace_root.join("docs");

    let mut main: Vec<DocIndexEntry> = Vec::new();
    let mut plans: Vec<DocIndexEntry> = Vec::new();

    for path in read_md_dir_sorted(&docs_dir)? {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        if name == "README.md" || DOCS_INDEX_SKIP.contains(&name.as_str()) {
            continue;
        }
        let rel = format!("./{name}");
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let (title, summary) = extract_doc_summary(&text, &rel)?;
        let entry = DocIndexEntry { link: rel, title, summary };
        if DOCS_INDEX_PLANS_AND_RESEARCH.contains(&name.as_str()) {
            plans.push(entry);
        } else {
            main.push(entry);
        }
    }

    let mut roadmap: Vec<DocIndexEntry> = Vec::new();
    for path in read_md_dir_sorted(&docs_dir.join("roadmap"))? {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        let rel = format!("./roadmap/{name}");
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let (title, summary) = extract_doc_summary(&text, &rel)?;
        roadmap.push(DocIndexEntry { link: rel, title, summary });
    }

    // `docs/research/` was deleted wholesale during the documentation
    // reduction, so this scan is optional rather than required. It stays
    // because the group still exists and a research doc added later must be
    // indexed rather than silently dropped.
    for path in read_md_dir_sorted_optional(&docs_dir.join("research"))? {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        let rel = format!("./research/{name}");
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let (title, summary) = extract_doc_summary(&text, &rel)?;
        plans.push(DocIndexEntry { link: rel, title, summary });
    }

    // `docs/plans/` joins the same group, for the same reason `docs/research/`
    // does. It was NOT scanned until 2026-08-04, and the omission was silent in
    // the worst way: six plan documents, including the server-ECS migration
    // plan, landed invisible to the index, each one having
    // been written to satisfy the H1 + `## What it is` contract that only
    // matters *because* the generator reads it. Nothing failed -- the drift test
    // compares the generator against `docs/README.md`, and both agreed the
    // directory did not exist. A generated index cannot drift from the docs, but
    // it can silently omit a whole directory, which is a distinct failure mode
    // worth remembering: the gate proves consistency, not coverage.
    for path in read_md_dir_sorted(&docs_dir.join("plans"))? {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        let rel = format!("./plans/{name}");
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let (title, summary) = extract_doc_summary(&text, &rel)?;
        plans.push(DocIndexEntry { link: rel, title, summary });
    }

    let mut out = String::new();
    out.push_str("# Lodestone docs\n\n");
    out.push_str(
        "<!-- Generated by `cargo xtask docs-index` from every doc's own H1 and its \
`## What it is`/`## What this is` summary paragraph. Do not hand-edit: edit the doc\n\
     itself and regenerate (`cargo xtask docs-index`), or run `LODESTONE_REGEN=1 cargo\n\
     test -p xtask docs_index_matches_committed`. `cargo test -p xtask` fails loudly if\n\
     this file drifts from the generator's output. -->\n\n",
    );
    out.push_str(
        "Subsystem documentation. See also [`architecture.md`](./architecture.md)\n\
(the crate graph and the cross-cutting constraints) and\n\
[`meta/handoff.md`](./meta/handoff.md) (for an agent orchestrating this repo).\n\n",
    );

    for entry in &main {
        write_docs_index_entry(&mut out, entry);
    }

    out.push_str("\n---\n\n## Roadmap\n\n");
    out.push_str(
        "Per-track roadmap documents for the plan to 1:1 parity (epic decompositions, one per\n\
track) -- see the first entry below for how the whole set is organised and what invariants\n\
every issue under it inherits.\n\n",
    );
    for entry in &roadmap {
        write_docs_index_entry(&mut out, entry);
    }

    out.push_str("\n---\n\n## Plans and research\n\n");
    out.push_str(
        "Longer-form artifacts that are not per-subsystem docs: phased plans (everything under\n\
`docs/plans/`, written to be dispatchable before the work starts), and read-only\n\
diagnoses produced before the corresponding fix was written. They live here because a\n\
diagnosis is worth keeping *after* the fix lands -- CLAUDE.md's standing claim is that the\n\
record of confidently-held false beliefs is the most valuable thing in this repo, and several\n\
of these caught the *brief* being wrong rather than the code.\n\n",
    );
    for entry in &plans {
        write_docs_index_entry(&mut out, entry);
    }

    Ok(out)
}

fn docs_index_out_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join("docs/README.md")
}

/// Writes the generated index to `docs/README.md`.
pub fn write_docs_index(workspace_root: &Path) -> Result<PathBuf> {
    let generated = generate_docs_index(workspace_root)?;
    let out_path = docs_index_out_path(workspace_root);
    std::fs::write(&out_path, generated)
        .with_context(|| format!("write generated docs index to {}", out_path.display()))?;
    Ok(out_path)
}

/// The result of a docs-index drift check, shaped like
/// [`VersionTableCheck`] (same idea, different generated file) but kept as
/// its own type rather than reused -- `VersionTableCheck` naming a
/// `docs/README.md` result would be its own small staleness trap.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocsIndexCheck {
    pub out_path: PathBuf,
    pub summary: String,
    identical: bool,
}

impl DocsIndexCheck {
    #[must_use]
    pub const fn is_identical(&self) -> bool {
        self.identical
    }
}

/// Recomputes the docs index and compares it against the checked-in file
/// without writing, for use as a drift-guard check (mirrors
/// [`check_version_table`]'s shape).
pub fn check_docs_index(workspace_root: &Path) -> Result<DocsIndexCheck> {
    let expected = generate_docs_index(workspace_root)?;
    let out_path = docs_index_out_path(workspace_root);
    let actual = std::fs::read_to_string(&out_path)
        .with_context(|| format!("read {}", out_path.display()))?;

    if actual == expected {
        return Ok(DocsIndexCheck {
            out_path,
            summary: "docs/README.md is up to date".to_owned(),
            identical: true,
        });
    }

    Ok(DocsIndexCheck {
        summary: packet_id_diff_summary(&out_path, &expected, &actual),
        out_path,
        identical: false,
    })
}

// ---------------------------------------------------------------------------
// bench-compare: ratio-against-a-stored-baseline tool
// ---------------------------------------------------------------------------
//
// `docs/roadmap/benchmarks.md` already states the policy this tool
// implements (ratio against a same-machine baseline, ±25% tolerance band,
// never a CI-blocking gate); this command is the one piece that policy left
// still missing: "a small comparison script/tool ... that reads two
// bench-results/*.jsonl records and reports a ratio + verdict against a
// stated tolerance." `benches/support.rs`'s `record()` already does this
// automatically for "this run vs the immediately preceding run" every time a
// bench executes; this command is the standalone form that needs no bench
// re-run at all -- point it at an existing `bench-results/<bench>.jsonl` and
// ask it to compare two *specific* recorded commits (e.g. "before my change"
// vs "after my change") without regenerating anything.
//
// Per CLAUDE.md's evidence standard, this never asserts a verdict like
// "regression" -- a metric can be either direction-is-better depending on
// its unit (lower is better for a `_ms` timing, higher is better for a
// `_throughput` count), and this schema does not carry that annotation. It
// reports the ratio and whether it falls inside the tolerance band, and lets
// the caller -- who knows what the metric means -- read the direction.

/// One recorded line from a `bench-results/<bench>.jsonl` file, matching
/// `benches/support.rs`'s `record()` schema exactly (kept as its own
/// deserialization target, independent of that file, since `xtask` cannot
/// depend on any one crate's `benches/support.rs` — it is intentionally
/// duplicated per crate, not a shared module).
#[derive(Clone, Debug)]
pub struct BenchRecord {
    pub timestamp: u64,
    pub git_sha: String,
    pub machine: String,
    pub profile: String,
    pub scene: String,
    pub metric: String,
    pub value: f64,
    pub unit: String,
}

/// Parses every line of a `bench-results/*.jsonl` file into [`BenchRecord`]s,
/// in file order (which is chronological -- the format is append-only).
/// Malformed lines are skipped with a `None` filtered out rather than
/// failing the whole read, matching `support.rs`'s own tolerant parsing (a
/// hand-edited or partially-written line should not make every other
/// recorded run unreadable).
pub fn read_bench_records(path: &Path) -> Result<Vec<BenchRecord>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let records = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|v| {
            Some(BenchRecord {
                timestamp: v.get("timestamp")?.as_u64()?,
                git_sha: v.get("git_sha")?.as_str()?.to_string(),
                machine: v.get("machine")?.as_str()?.to_string(),
                profile: v.get("profile")?.as_str()?.to_string(),
                scene: v.get("scene")?.as_str()?.to_string(),
                metric: v.get("metric")?.as_str()?.to_string(),
                value: v.get("value")?.as_f64()?,
                unit: v.get("unit")?.as_str()?.to_string(),
            })
        })
        .collect();
    Ok(records)
}

/// Inputs to [`compare_bench_records`].
#[derive(Clone, Debug)]
pub struct BenchCompareOptions {
    pub metric: String,
    pub scene: String,
    /// Git-sha prefix to select the candidate ("after") run. `None` means
    /// "the most recent recorded run matching `metric`/`scene`".
    pub candidate_sha: Option<String>,
    /// Git-sha prefix to select the baseline ("before") run. `None` means
    /// "the run immediately preceding the candidate, on the same machine and
    /// build profile" -- the same pairing `support.rs::record` compares
    /// against automatically.
    pub baseline_sha: Option<String>,
    /// Tolerance band as a fraction (e.g. `0.25` for ±25%), matching
    /// `docs/roadmap/benchmarks.md`'s stated policy and `support.rs`'s own
    /// literal.
    pub tolerance: f64,
}

/// The result of comparing two [`BenchRecord`]s.
#[derive(Clone, Debug)]
pub struct BenchCompareReport {
    pub baseline: BenchRecord,
    pub candidate: BenchRecord,
    pub ratio: f64,
    pub tolerance: f64,
}

impl BenchCompareReport {
    #[must_use]
    pub fn within_tolerance(&self) -> bool {
        (1.0 - self.tolerance..=1.0 + self.tolerance).contains(&self.ratio)
    }

    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "metric={} scene={:?}",
            self.candidate.metric, self.candidate.scene
        );
        let _ = writeln!(
            out,
            "  baseline  {:>12.4}{} @ {} ({}, {})",
            self.baseline.value, self.baseline.unit, self.baseline.git_sha, self.baseline.machine, self.baseline.profile
        );
        let _ = writeln!(
            out,
            "  candidate {:>12.4}{} @ {} ({}, {})",
            self.candidate.value, self.candidate.unit, self.candidate.git_sha, self.candidate.machine, self.candidate.profile
        );
        let band_pct = self.tolerance * 100.0;
        if self.within_tolerance() {
            let _ = writeln!(
                out,
                "  ratio {:.3} -- within +/-{band_pct:.2}% band -> OK",
                self.ratio
            );
        } else {
            let _ = writeln!(
                out,
                "  ratio {:.3} -- OUTSIDE +/-{band_pct:.2}% band -> FLAGGED (direction depends on whether \
                 {:?} is lower- or higher-is-better; this tool does not know)",
                self.ratio, self.candidate.metric
            );
        }
        out
    }
}

/// Finds the baseline/candidate pair `opts` describes among `records`
/// (already filtered to one `metric`/`scene`... no -- takes the *unfiltered*
/// list and does the metric/scene filtering itself, so callers just hand it
/// [`read_bench_records`]'s output) and reports their ratio.
pub fn compare_bench_records(records: &[BenchRecord], opts: &BenchCompareOptions) -> Result<BenchCompareReport> {
    let filtered: Vec<&BenchRecord> = records
        .iter()
        .filter(|r| r.metric == opts.metric && r.scene == opts.scene)
        .collect();
    if filtered.is_empty() {
        bail!(
            "no records match metric {:?} scene {:?}",
            opts.metric,
            opts.scene
        );
    }

    let candidate_index = match &opts.candidate_sha {
        Some(prefix) => filtered
            .iter()
            .rposition(|r| r.git_sha.starts_with(prefix.as_str()))
            .ok_or_else(|| anyhow!("no record matching metric/scene has git_sha prefix {prefix:?} (candidate)"))?,
        None => filtered.len() - 1,
    };
    let candidate = filtered[candidate_index];

    let baseline_index = match &opts.baseline_sha {
        Some(prefix) => filtered[..candidate_index]
            .iter()
            .rposition(|r| r.git_sha.starts_with(prefix.as_str()))
            .ok_or_else(|| {
                anyhow!("no record before the candidate has git_sha prefix {prefix:?} (baseline)")
            })?,
        None => filtered[..candidate_index]
            .iter()
            .rposition(|r| r.machine == candidate.machine && r.profile == candidate.profile)
            .ok_or_else(|| {
                anyhow!(
                    "no prior record on machine {:?} profile {:?} to use as an implicit baseline -- \
                     pass --baseline <sha>",
                    candidate.machine,
                    candidate.profile
                )
            })?,
    };
    let baseline = filtered[baseline_index];

    if baseline.machine != candidate.machine || baseline.profile != candidate.profile {
        bail!(
            "baseline ({}, {}) and candidate ({}, {}) are not the same machine/profile -- \
             not a valid comparison per the evidence standard (a number is not comparable across machines)",
            baseline.machine,
            baseline.profile,
            candidate.machine,
            candidate.profile
        );
    }

    Ok(BenchCompareReport {
        baseline: baseline.clone(),
        candidate: candidate.clone(),
        ratio: candidate.value / baseline.value,
        tolerance: opts.tolerance,
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
            "refusing to write outside crates/versions/*/src/generated; requested {}",
            requested.display()
        );
    }

    Ok(workspace_root.join(relative))
}

/// Returns whether `relative` names `crates/versions/<crate>/src/generated/<file>`.
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
                && *protocol == "versions"
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

// ---------------------------------------------------------------------------
// wasm-check — wasm32 compile + confinement-guard tripwire.
//
// Tested port of scripts/wasm-check.sh. The script is a shell pipeline with no
// gate (it relies on manual review of its output); this command runs the same
// three phases — compile the wasm crate subset, grep-based CONFINEMENT guards,
// trunk build of web/ — but reports every failure through Result so a leak is a
// non-zero exit rather than something a `| grep | tail` can swallow. The
// scanners are unit-tested below; the shell original has none.
//
// Read scripts/wasm-check.sh's header for the WHY this exists: "compiles to
// wasm" and "works on wasm" are different, and std::fs / Instant::now /
// std::thread::spawn / tokio::time all COMPILE for wasm32 and only die at
// runtime. The compile pass is structurally blind to them; the confinement
// guards are the tripwire that actually catches a leaked hazard.
// ---------------------------------------------------------------------------

/// Target triple the wasm crate subset is compiled for.
pub const WASM_TARGET: &str = "wasm32-unknown-unknown";

/// One crate in the wasm compile subset, plus any extra `cargo build` args the
/// browser configuration requires (the script's `"pkg|extra"` rows).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WasmCrate {
    pub name: &'static str,
    pub extra_args: &'static [&'static str],
}

/// The wasm compile subset, in build order — parity with scripts/wasm-check.sh.
/// The two non-obvious rows' whys are kept from the script: lodestone-net needs
/// the `ws-web` feature for browser websockets; every other crate builds
/// default.
pub fn wasm_crates() -> Vec<WasmCrate> {
    vec![
        // The portable clock seam: nearly every crate below depends on this
        // one, so a regression here would otherwise surface only
        // transitively, attributed to whichever dependent happened to fail
        // first. Listed first for the same reason lodestone-data is listed
        // separately from v770.
        WasmCrate {
            name: "lodestone-time",
            extra_args: &[],
        },
        WasmCrate {
            name: "lodestone-core",
            extra_args: &[],
        },
        WasmCrate {
            name: "lodestone-model",
            extra_args: &[],
        },
        WasmCrate {
            name: "lodestone-world",
            extra_args: &[],
        },
        WasmCrate {
            name: "lodestone-physics",
            extra_args: &[],
        },
        WasmCrate {
            name: "lodestone-assets",
            extra_args: &[],
        },
        WasmCrate {
            name: "lodestone-registry",
            extra_args: &[],
        },
        WasmCrate {
            name: "lodestone-render",
            extra_args: &[],
        },
        WasmCrate {
            name: "lodestone-audio",
            extra_args: &[],
        },
        // Event→sound bridge. Its default build is device-free and version-free
        // (lodestone-audio, lodestone-assets, lodestone-model, glam, thiserror
        // — all wasm-safe); the live gate's client/tokio/registry deps are
        // gated behind the off-by-default `live-v770` feature.
        WasmCrate {
            name: "lodestone-sound",
            extra_args: &[],
        },
        // Canonical 26.2 game-data censuses; depends on nothing
        // but lodestone-model, listed separately so a regression is
        // unambiguous rather than only surfacing via v770.
        WasmCrate {
            name: "lodestone-data",
            extra_args: &[],
        },
        WasmCrate {
            name: "lodestone-v26-2",
            extra_args: &[],
        },
        WasmCrate {
            name: "lodestone-v1-8",
            extra_args: &[],
        },
        WasmCrate {
            name: "lodestone-net",
            extra_args: &["--features", "ws-web"],
        },
        // bevy_ecs must be wasm32-clean or the bevy migration stops here.
        WasmCrate {
            name: "lodestone-ecs",
            extra_args: &[],
        },
        WasmCrate {
            name: "lodestone-client",
            extra_args: &[],
        },
        WasmCrate {
            name: "lodestone-controller",
            extra_args: &[],
        },
        // Integrated server runs in the browser under the `spawn_local` seam;
        // browser singleplayer depends on it.
        WasmCrate {
            name: "lodestone-server",
            extra_args: &[],
        },
        WasmCrate {
            name: "lodestone-worldgen",
            extra_args: &[],
        },
        // The playable game shell — the menu, `Sim`, the renderer, all of it. The
        // browser consumes this crate's LIB target, and it is the crate most likely
        // to regress because almost nobody working in it builds for wasm. Placed
        // last because it sits on top of everything above, so a failure here is
        // unambiguous rather than transitive.
        //
        // It was missing from this table while the reference script listed it, so
        // `cargo xtask wasm-check` — the implementation CI runs — never named it and
        // only reached it transitively through the trunk build of `web/`.
        WasmCrate {
            name: "lodestone-shell",
            extra_args: &[],
        },
    ]
}

/// One confinement guard rule — parity with scripts/wasm-check.sh's
/// CONFINEMENT_RULES table, asserted by a test that PARSES that table rather than
/// restating it.
///
/// `banned` is matched as a literal substring, and that is a requirement rather
/// than an observation: the same string is handed to grep by the script, so a
/// regex metacharacter would mean two different things on the two sides.
/// `confinement_rules_match_the_reference_script_table` enforces it. A `|` is the
/// worst case — it is the script's own field separator, and a `\|` alternation is
/// what made five rules print PASS with their grep never executing. Split a
/// two-hazard rule into one rule per hazard instead.
#[derive(Debug, Clone)]
pub struct ConfinementRule {
    /// Report label, e.g. "lodestone-assets fs-confinement".
    pub label: &'static str,
    /// Directory under the workspace root to scan, e.g. "crates/lodestone-assets/src".
    pub src_dir: &'static str,
    /// Banned symbol, matched as a literal substring.
    pub banned: &'static str,
    /// File basenames allowed to contain the banned symbol — the
    /// cfg(not(target_arch = "wasm32"))-gated files that confine the hazard.
    pub allowlist: &'static [&'static str],
}

/// The confinement rules in effect — parity with scripts/wasm-check.sh. Add a
/// row only after the crate actually confines the hazard to an allowlisted,
/// cfg(not(wasm32))-gated file; a rule for ungated code goes red for everyone.
pub fn confinement_rules() -> Vec<ConfinementRule> {
    vec![
        // lodestone-audio has NO time source at all (sample-driven clock), so
        // Instant::now() is banned across the whole crate with an empty
        // allowlist — "audio never touches wall-clock time" is a checked
        // invariant, not a promise.
        ConfinementRule {
            label: "lodestone-assets fs-confinement",
            src_dir: "crates/lodestone-assets/src",
            banned: "std::fs::",
            allowlist: &["source_native.rs"],
        },
        ConfinementRule {
            label: "lodestone-audio device-confinement",
            src_dir: "crates/lodestone-audio/src",
            banned: "cpal::",
            allowlist: &["sink.rs"],
        },
        ConfinementRule {
            label: "lodestone-audio time-confinement",
            src_dir: "crates/lodestone-audio/src",
            banned: "Instant::now(",
            allowlist: &[],
        },
        ConfinementRule {
            label: "lodestone-sound time-confinement",
            src_dir: "crates/lodestone-sound/src",
            banned: "Instant::now(",
            allowlist: &[],
        },
        // lodestone-client confines tokio::time to native_time.rs and bans the
        // whole Instant/std::fs/std::thread family across the crate (the driver
        // is event-driven and never reads a wall clock); tokio::spawn is
        // confined to the spawn.rs seam, whose wasm arm uses
        // wasm_bindgen_futures::spawn_local.
        ConfinementRule {
            label: "lodestone-client time-confinement",
            src_dir: "crates/lodestone-client/src",
            banned: "tokio::time::",
            allowlist: &["native_time.rs"],
        },
        ConfinementRule {
            label: "lodestone-client instant-ban",
            src_dir: "crates/lodestone-client/src",
            banned: "Instant::now(",
            allowlist: &[],
        },
        ConfinementRule {
            label: "lodestone-client fs-ban",
            src_dir: "crates/lodestone-client/src",
            banned: "std::fs::",
            allowlist: &[],
        },
        ConfinementRule {
            label: "lodestone-client thread-ban",
            src_dir: "crates/lodestone-client/src",
            banned: "std::thread",
            allowlist: &[],
        },
        ConfinementRule {
            label: "lodestone-client spawn-confinement",
            src_dir: "crates/lodestone-client/src",
            banned: "tokio::spawn",
            allowlist: &["spawn.rs"],
        },
        // --- lodestone-shell ---
        // The shell confines both trapping clocks to `crate::platform`, which
        // re-exports `web_time` (std's own types on native, `performance.now()` /
        // `Date.now()` in a browser). These rules ban the `std::time::` PATHS rather
        // than the bare `Instant::now(` spelling, deliberately: the shell's call
        // sites read `crate::platform::Instant::now()`, so an `Instant::now(`
        // pattern would match all of them and the rule could never go green. The
        // path is what distinguishes a trapping call from a portable one.
        //
        // `platform.rs` alone is allowlisted — the strongest form. Both rules found
        // LIVE TRAPS when first added, on a tree whose wasm32 build was already
        // exit 0, which is the entire argument for having them.
        ConfinementRule {
            label: "lodestone-shell instant-confinement",
            src_dir: "crates/lodestone-shell/src",
            banned: "std::time::Instant",
            allowlist: &["platform.rs"],
        },
        ConfinementRule {
            label: "lodestone-shell systemtime-confinement",
            src_dir: "crates/lodestone-shell/src",
            banned: "std::time::SystemTime::now",
            allowlist: &["platform.rs"],
        },
        // `thread::spawn` TRAPS on wasm32; the three thread entry points do NOT
        // behave alike, which is why this names one of them and not the family:
        //
        //     std::thread::spawn                 TRAPS
        //     std::thread::sleep                 TRAPS
        //     std::thread::Builder::new().spawn  Err(Unsupported) — degrades
        //     std::thread::available_parallelism Err              — degrades
        //
        // Allowlist = the files that confine it behind
        // cfg(not(target_arch = "wasm32")) with a browser arm beside it. SCOPE
        // LIMIT: this does NOT cover `thread::sleep`, whose remaining sites are
        // inside `#[cfg(test)] mod tests` in files whose production halves must stay
        // covered — a scanner cannot tell a test module from a production one, so
        // allowlisting those files would buy one hazard and blind two files to it.
        //
        // `app/runners.rs` joined this list for `run_headless_session`:
        // its stdin control thread is
        // `cfg(all(not(target_arch = "wasm32"), feature = "runtime-presentation"))`
        // — the whole function is native-only, like `run_connect`/`run_headless`
        // right beside it in the same file, and has no browser arm because
        // `Mode::HeadlessSession` itself is refused on wasm32 (`app.rs`'s `run`).
        ConfinementRule {
            label: "lodestone-shell thread-spawn-confinement",
            src_dir: "crates/lodestone-shell/src",
            banned: "thread::spawn",
            allowlist: &["mesher.rs", "accounts.rs", "status.rs", "runners.rs"],
        },
        // --- the clock, in every other crate the browser reaches ---
        //
        // These exist because lodestone-shell's three rules were not enough. The
        // browser build reached exit 0 with all three PASSing and still died twice:
        // once in lodestone-particle (`from_entropy` → `SystemTime::now()`, three
        // crates below the shell) and once in lodestone-server/lodestone-worldgen on
        // the way into a world. A confinement guard only covers the crate it names,
        // and the browser reaches about fifteen.
        //
        // lodestone-server is the sharpest case: `collect_nearby_items` already
        // carried a comment stating the rule — "this crate must not call
        // std::time::Instant::now() anywhere in lodestone-server, because the crate
        // links into a wasm32 bundle where that compiles and then panics at runtime"
        // — and four sites violated it anyway. The rule was right and it was prose.
        //
        // ONE RULE PER HAZARD, not one `(Instant|SystemTime)` rule per crate. In the
        // reference script the alternation form spelled `\|`, which IS that table's
        // field separator, so all five of those rules had their pattern truncated,
        // grep exited 2, and a swallowed error printed PASS. Keeping every pattern a
        // literal substring is what lets both implementations share one table.
        //
        // Empty allowlists: these crates have no business reading a wall clock
        // through `std`. Each uses `web_time`, whose non-wasm arm is
        // `pub use std::time::*`, so native is byte-identical.
        ConfinementRule {
            label: "lodestone-server instant-ban",
            src_dir: "crates/lodestone-server/src",
            banned: "std::time::Instant",
            allowlist: &[],
        },
        ConfinementRule {
            label: "lodestone-server systemtime-ban",
            src_dir: "crates/lodestone-server/src",
            banned: "std::time::SystemTime",
            allowlist: &[],
        },
        // `tokio::time::Instant::now()` is a different literal than
        // `std::time::Instant`, so the rule above cannot see it, and it traps
        // identically — `server.rs`'s own `JoinStopwatch` doc says so ("it
        // bottoms out in std::time::Instant::now() ... and panics identically"),
        // which did not stop `serve_play`'s keep-alive/time-sync/vitals/
        // container-sync interval setup from shipping six unguarded calls anyway.
        // Measured live in the browser build: joining a singleplayer world
        // panics at `library/std/src/sys/time/unsupported.rs:13:9` the instant
        // the client reaches Play, and "Joining world..." spins forever because
        // the connection task that died was the one about to send the rest of
        // the view. `tick.rs` also names this symbol, but only inside
        // `run_tick_loop`, which wasm32's `open_in_memory` deliberately never
        // spawns (see `net.rs`'s own comment on that constructor) — a real,
        // documented gap rather than a live trap, so it is allowlisted rather
        // than making this rule impossible to turn green.
        ConfinementRule {
            label: "lodestone-server tokio-instant-ban",
            src_dir: "crates/lodestone-server/src",
            banned: "tokio::time::Instant",
            allowlist: &["tick.rs"],
        },
        // This crate's clock now goes through `lodestone_time::` rather than a
        // direct `web_time::` dependency — `Cargo.toml` no longer lists
        // `web-time` at all, so a reintroduced bare `web_time::` call would not
        // even compile. That is not a reason to skip a rule for it: the whole
        // point of a confinement guard, per the two rules above, is to catch a
        // regression by name before anyone waits on a build to find it. Empty
        // allowlist: every legitimate call site in this crate (including
        // `browser_timer.rs`, migrated alongside the rest — its `BrowserInstant`
        // alias is `lodestone_time::Instant`, the identical type on every
        // target) reads `lodestone_time::`, which this qualified `web_time::`
        // pattern does not match.
        ConfinementRule {
            label: "lodestone-server web-time-ban",
            src_dir: "crates/lodestone-server/src",
            banned: "web_time::",
            allowlist: &[],
        },
        ConfinementRule {
            label: "lodestone-worldgen instant-ban",
            src_dir: "crates/lodestone-worldgen/src",
            banned: "std::time::Instant",
            allowlist: &[],
        },
        ConfinementRule {
            label: "lodestone-worldgen systemtime-ban",
            src_dir: "crates/lodestone-worldgen/src",
            banned: "std::time::SystemTime",
            allowlist: &[],
        },
        ConfinementRule {
            label: "lodestone-particle instant-ban",
            src_dir: "crates/lodestone-particle/src",
            banned: "std::time::Instant",
            allowlist: &[],
        },
        ConfinementRule {
            label: "lodestone-particle systemtime-ban",
            src_dir: "crates/lodestone-particle/src",
            banned: "std::time::SystemTime",
            allowlist: &[],
        },
        ConfinementRule {
            label: "lodestone-net instant-ban",
            src_dir: "crates/lodestone-net/src",
            banned: "std::time::Instant",
            allowlist: &[],
        },
        ConfinementRule {
            label: "lodestone-net systemtime-ban",
            src_dir: "crates/lodestone-net/src",
            banned: "std::time::SystemTime",
            allowlist: &[],
        },
        // `async_task.rs`'s only clock hits are inside a `#[cfg(test)] mod`, which
        // never reaches a browser; a scanner cannot tell a test module from a
        // production one, so it is named.
        ConfinementRule {
            label: "lodestone-ecs instant-ban",
            src_dir: "crates/lodestone-ecs/src",
            banned: "std::time::Instant",
            allowlist: &["async_task.rs"],
        },
        ConfinementRule {
            label: "lodestone-ecs systemtime-ban",
            src_dir: "crates/lodestone-ecs/src",
            banned: "std::time::SystemTime",
            allowlist: &["async_task.rs"],
        },
        // `lodestone-auth` joined this qualified-pattern bucket once `flow.rs`
        // (which now compiles and runs on wasm32 — see that module's doc)
        // took a `lodestone-time` dependency for a real wall-clock deadline
        // (`PendingLogin::is_expired`). That makes this crate's own
        // `lodestone-auth systemtime-ban` rule below (still bare-pattern) an
        // exception rather than the rule for this crate now: `lodestone-time`
        // re-exports `Instant` but not `SystemTime` (see `lodestone-time`'s own
        // `src/lib.rs`), so there is no legitimate qualified
        // `lodestone_time::SystemTime::now()` spelling to protect — only
        // `Instant` needed to move buckets. An empty allowlist, matching
        // `lodestone-server`/`-worldgen`/`-particle`/`-net` above: neither
        // `browser_login.rs` nor `migrate.rs` (both still native-only, both
        // still allowlisted on the *systemtime* rule below) spells the type
        // out fully qualified — both reach it through a bare `use
        // std::time::{..., Instant}` import, which this qualified substring
        // does not match at all, so there is nothing here to allowlist.
        ConfinementRule {
            label: "lodestone-auth instant-ban",
            src_dir: "crates/lodestone-auth/src",
            banned: "std::time::Instant",
            allowlist: &[],
        },
        // --- crates outside the wasm build, tightened toward "no crate but
        // lodestone-time may name std::time's clocks" ---
        //
        // None of these three crates appears in wasm_crates() above: each is
        // either dev-dependency-only (lodestone-testsupport — every dependent
        // lists it under [dev-dependencies], so its lib target is never linked
        // into a --lib build, wasm or native), a native-only bin nothing depends
        // on (lodestone-allocbench, already excluded from the workspace-wide
        // --all-features sweep for its allocator mutual-exclusion), or reaches
        // wasm only via a #[cfg(test)] module that never enters a --lib build
        // either way (lodestone-world's
        // fill_region_lock_hold_time_on_a_large_synthetic_fill test).
        //
        // A rule here still earns its keep: it turns "this file structurally
        // cannot reach wasm" from a claim into something re-checked on every run,
        // and it is what stops a NEW file in one of these crates from growing an
        // ungated clock call unnoticed.
        //
        // PATTERN CHOICE: none of these three crates depends on lodestone-time, so
        // there is no legitimate lodestone_time::Instant::now() call anywhere in
        // them to avoid catching — unlike lodestone-server/worldgen/particle/net/
        // ecs/auth above, which must use the qualified std::time:: path
        // specifically so a legitimate lodestone_time::Instant::now() elsewhere in
        // the same crate does not false-positive. These three instead use the bare
        // Instant::now(/SystemTime::now( method-call spelling (as
        // lodestone-audio/lodestone-sound do, for the same "no legitimate caller
        // exists" reason) because their actual call sites mix qualified and
        // unqualified spellings and the bare form catches both.
        //
        // `lodestone-auth systemtime-ban` stays here (bare pattern) rather than
        // moving with `lodestone-auth instant-ban` above: `lodestone-time` has no
        // `SystemTime` re-export at all (see its own `src/lib.rs`), so unlike
        // `Instant` there is still no legitimate qualified
        // `lodestone_time::SystemTime::now()` spelling in this crate for a
        // qualified pattern to protect — the bare form remains the tightest
        // correct rule for this one hazard in this one crate.
        ConfinementRule {
            label: "lodestone-auth systemtime-ban",
            src_dir: "crates/lodestone-auth/src",
            banned: "SystemTime::now(",
            allowlist: &["browser_login.rs", "migrate.rs"],
        },
        ConfinementRule {
            label: "lodestone-world instant-ban",
            src_dir: "crates/lodestone-world/src",
            banned: "Instant::now(",
            allowlist: &["world.rs"],
        },
        ConfinementRule {
            label: "lodestone-world systemtime-ban",
            src_dir: "crates/lodestone-world/src",
            banned: "SystemTime::now(",
            allowlist: &["world.rs"],
        },
        ConfinementRule {
            label: "lodestone-testsupport instant-ban",
            src_dir: "crates/lodestone-testsupport/src",
            banned: "Instant::now(",
            allowlist: &["lib.rs"],
        },
        ConfinementRule {
            label: "lodestone-testsupport systemtime-ban",
            src_dir: "crates/lodestone-testsupport/src",
            banned: "SystemTime::now(",
            allowlist: &["lib.rs"],
        },
        ConfinementRule {
            label: "lodestone-allocbench instant-ban",
            src_dir: "crates/lodestone-allocbench/src",
            banned: "Instant::now(",
            allowlist: &["main.rs"],
        },
        ConfinementRule {
            label: "lodestone-allocbench systemtime-ban",
            src_dir: "crates/lodestone-allocbench/src",
            banned: "SystemTime::now(",
            allowlist: &["main.rs"],
        },
        // lodestone-time itself: the ONE place allowed to depend on
        // `web-time`, so every other crate's rule above can ban
        // `std::time::{Instant,SystemTime}` with an empty allowlist. This
        // crate is held to the identical rule, with an EMPTY allowlist too —
        // it has no special exemption to spell `std::time` directly, because
        // everything it re-exports comes from `web_time`, whose own non-wasm
        // arm is `pub use std::time::*` — that happens inside the `web-time`
        // dependency, not in this crate's own source.
        ConfinementRule {
            label: "lodestone-time instant-ban",
            src_dir: "crates/lodestone-time/src",
            banned: "std::time::Instant",
            allowlist: &[],
        },
        ConfinementRule {
            label: "lodestone-time systemtime-ban",
            src_dir: "crates/lodestone-time/src",
            banned: "std::time::SystemTime",
            allowlist: &[],
        },
    ]
}

/// A single banned-symbol hit outside the allowlisted file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfinementLeak {
    /// Path relative to the workspace root.
    pub path: PathBuf,
    /// 1-based line number.
    pub line: usize,
    /// Full line content.
    pub content: String,
}

/// Scans one rule's src dir for the banned symbol, skipping allowlisted file
/// basenames and comment lines. A missing src dir is an ERROR, not a silent pass —
/// a rule pointing at a typo'd path would otherwise report green forever.
///
/// Every rule has a positive control:
/// `every_confinement_rule_fires_under_a_planted_violation` plants a violating line
/// in the crate each rule names and requires the scan to report it by path.
pub fn scan_confinement(
    workspace_root: &Path,
    rule: &ConfinementRule,
) -> Result<Vec<ConfinementLeak>> {
    let root = workspace_root.join(rule.src_dir);
    if !root.is_dir() {
        bail!(
            "confinement rule {:?} scans a missing dir: {}",
            rule.label,
            root.display()
        );
    }
    let mut leaks = Vec::new();
    scan_confinement_dir(&root, workspace_root, rule, &mut leaks)?;
    leaks.sort_by(|a, b| a.path.cmp(&b.path).then(a.line.cmp(&b.line)));
    Ok(leaks)
}

/// True for a line whose first non-whitespace characters make it a comment, so a
/// banned symbol inside it cannot execute.
///
/// Parity with the reference script, which drops the same three openers. Every one
/// of these confinements is worth a sentence at its call site saying "use
/// `crate::platform::Instant`, not `std::time::Instant`, because the latter traps",
/// and a guard that fires on its own documentation trains people to delete the
/// documentation. Same reasoning that made a `"` legal inside a `.wgsl` comment.
///
/// SCOPE LIMIT, stated because a filter you trust further than it reaches is worse
/// than none: this is a line-opener test, not a lexer. It does not see a `/* … */`
/// block whose first line does not start with `*`, and it does not see a trailing
/// `// …` comment after code — which is the safe direction, since such a line has
/// executable content anyway. A hand-rolled Rust lexer would be wrong about
/// lifetimes; three scanners in this repo already were.
fn is_comment_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("//") || trimmed.starts_with('*') || trimmed.starts_with('#')
}

fn scan_confinement_dir(
    dir: &Path,
    workspace_root: &Path,
    rule: &ConfinementRule,
    leaks: &mut Vec<ConfinementLeak>,
) -> Result<()> {
    let entries = std::fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("read dir entry under {}", dir.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("stat {}", path.display()))?;
        if file_type.is_dir() {
            scan_confinement_dir(&path, workspace_root, rule, leaks)?;
        } else if file_type.is_file() {
            if rule
                .allowlist
                .contains(&entry.file_name().to_string_lossy().as_ref())
            {
                continue;
            }
            // Lossy read (not read_to_string) so a non-UTF-8 file is still
            // scanned rather than silently skipped, matching grep's behaviour.
            //
            // A file that disappeared between `read_dir` and here is skipped rather
            // than fatal: this is a shared checkout where another agent may delete a
            // file mid-walk, and a file that no longer exists cannot carry a hazard.
            // Only NotFound is tolerated — a permissions error still fails loudly,
            // because that one CAN hide a leak.
            let bytes = match std::fs::read(&path) {
                Ok(bytes) => bytes,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => {
                    return Err(err).with_context(|| format!("read {}", path.display()));
                }
            };
            let text = String::from_utf8_lossy(&bytes);
            for (index, line) in text.lines().enumerate() {
                if line.contains(rule.banned) && !is_comment_line(line) {
                    let rel = path
                        .strip_prefix(workspace_root)
                        .unwrap_or(&path)
                        .to_path_buf();
                    leaks.push(ConfinementLeak {
                        path: rel,
                        line: index + 1,
                        content: line.to_string(),
                    });
                }
            }
        }
    }
    Ok(())
}

/// Runs the full wasm-check tripwire: prereqs, per-crate compile, confinement
/// guards, then the trunk build of web/. Returns Err (non-zero exit) on any
/// failure — `cargo xtask wasm-check`, the tested replacement for
/// scripts/wasm-check.sh.
pub fn run_wasm_check(workspace_root: &Path) -> Result<()> {
    ensure_wasm_prereqs()?;

    println!("== Lodestone wasm32 compile guard ==");
    println!("target: {WASM_TARGET}");
    println!();

    let mut failures: Vec<String> = Vec::new();

    for wasm_crate in wasm_crates() {
        let display = if wasm_crate.extra_args.is_empty() {
            wasm_crate.name.to_string()
        } else {
            format!("{} {}", wasm_crate.name, wasm_crate.extra_args.join(" "))
        };
        print!("  {display:<34} ");
        match compile_crate_for_wasm(workspace_root, &wasm_crate) {
            Ok(()) => println!("PASS"),
            Err(failure) => {
                println!("FAIL");
                failures.push(format!(
                    "{} {}",
                    wasm_crate.name,
                    wasm_crate.extra_args.join(" ")
                ));
                report_build_failure(&failure);
                println!(
                    "      └─ two common causes: (a) a dependency pulled '{}' onto native-only",
                    wasm_crate.name
                );
                println!("         code (threads / std::fs / OS sockets / OS audio like cpal) — fix by gating");
                println!("         that dep or call behind cfg(not(target_arch = \"wasm32\")) or an");
                println!("         off-by-default feature; or (b) a plain compile error in '{}' or a crate", wasm_crate.name);
                println!("         it depends on — which, in this shared workspace, is often a sibling crate");
                println!("         mid-edit (see the named crate in the error above): wait and re-run.");
                println!(
                    "         Reproduce: cargo build -p {} --target {WASM_TARGET} {}",
                    wasm_crate.name,
                    wasm_crate.extra_args.join(" ")
                );
            }
        }
    }

    // A count with a verdict that depends on the count, printed unconditionally. A
    // confinement rule that reported neither clean nor leaked has measured nothing,
    // and the whole reason these guards exist is that five of them did exactly that
    // in the reference script for their entire life.
    let mut rules_scanned = 0usize;
    let rule_total = confinement_rules().len();
    for rule in confinement_rules() {
        print!("  {:<34} ", rule.label);
        match scan_confinement(workspace_root, &rule) {
            Ok(leaks) if leaks.is_empty() => {
                rules_scanned += 1;
                println!("PASS");
            }
            Ok(leaks) => {
                rules_scanned += 1;
                println!("FAIL");
                for leak in &leaks {
                    println!(
                        "      {}:{}:{}",
                        leak.path.display(),
                        leak.line,
                        leak.content
                    );
                }
                failures.push(format!(
                    "{}: '{}' used outside {{{}}}",
                    rule.label,
                    rule.banned,
                    rule.allowlist.join(",")
                ));
            }
            Err(err) => {
                println!("FAIL");
                println!("      {err:#}");
                failures.push(format!("{}: scanner error: {err:#}", rule.label));
            }
        }
    }
    println!();
    println!("  confinement rules that actually ran: {rules_scanned}/{rule_total}");
    if rules_scanned != rule_total {
        failures.push(format!(
            "only {rules_scanned} of {rule_total} confinement rules ran"
        ));
    }
    println!();

    // The browser app is its own workspace (outside the crates/ glob), built
    // through trunk so a wasm-bindgen-level break is caught, not just a rustc
    // one. Cheap because the crate graph above is already warm in the shared
    // target dir.
    if workspace_root.join("web").join("Cargo.toml").is_file() {
        print!("  {:<34} ", "lodestone-web (trunk build)");
        match build_web_with_trunk(workspace_root) {
            Ok(()) => println!("PASS"),
            Err(failure) => {
                println!("FAIL");
                failures.push("lodestone-web (trunk build)".to_string());
                report_build_failure(&failure);
                println!("      └─ the browser app failed to build. If the per-crate rows above are all");
                println!("         PASS, this is a wasm-bindgen/trunk-level break in web/ itself.");
                println!("         Reproduce: (cd web && trunk build)");
            }
        }
    }

    println!();
    if !failures.is_empty() {
        bail!(
            "RESULT: FAIL — {} item(s) failed the wasm check:\n  - {}",
            failures.len(),
            failures.join("\n  - ")
        );
    }

    println!("RESULT: PASS — all listed crates COMPILE to {WASM_TARGET}.");
    println!();
    println!("NOTE: the COMPILE pass proves compilation, NOT runtime, and is blind to the");
    println!("      'compiles on wasm, panics at runtime' family: std::fs, Instant::now,");
    println!("      std::thread::spawn, tokio::time all build green here. cfg(target_arch)");
    println!("      does NOT turn a fresh ungated call into a compile error (it only removes");
    println!("      existing native entry points), and a Cargo feature is weaker still");
    println!("      (unification re-enables it). The CONFINEMENT guards above are what");
    println!("      actually catch a leaked hazard, by reporting it back to file:line.");
    Ok(())
}

/// A check that cannot run must FAIL, not pass quietly (the script's own
/// philosophy, kept here): a missing wasm32 target or trunk is an error with
/// the install command, never a silent green.
fn ensure_wasm_prereqs() -> Result<()> {
    let installed = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).to_string())
        .unwrap_or_default();
    if !installed.contains(WASM_TARGET) {
        bail!(
            "error: rust target '{WASM_TARGET}' is not installed.\n       \
             this check CANNOT RUN without it — failing rather than passing quietly.\n       \
             run: rustup target add {WASM_TARGET}"
        );
    }

    let trunk_present = Command::new("trunk")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);
    if !trunk_present {
        bail!(
            "error: 'trunk' is not installed (required to build/serve the browser app).\n       \
             this check CANNOT RUN without it — failing rather than passing quietly.\n       \
             run: cargo install trunk --version 0.21.14\n       \
             or (prebuilt, faster): curl -sSL \\\n       \
             https://github.com/trunk-rs/trunk/releases/download/v0.21.14/trunk-$(uname -m)-apple-darwin.tar.gz \\\n       \
             | tar xz -C ~/.cargo/bin trunk"
        );
    }
    Ok(())
}

/// Where wasm-check writes the full output of each failed build, so the console
/// summary is never the only copy.
pub const WASM_CHECK_LOG_DIR: &str = "target/wasm-check";

/// How many summary lines a failed build may print before being cut off. Chosen
/// to fit a `Caused by:` chain or one rustc diagnostic with its `-->` frame,
/// which the previous 6/8-line caps could not.
const WASM_DIAGNOSTIC_MAX_LINES: usize = 40;

/// Substrings (matched case-insensitively, against ANSI-stripped text) that mark
/// a line worth showing from a failed build.
///
/// Deliberately **not anchored**. The anchored form (`line.starts_with("error")`)
/// is what destroyed the evidence in the only CI failure this check has ever
/// caught: `trunk` prefixes every line with an RFC-3339 timestamp and a level, so
/// nothing it writes starts with `error`, and `cargo` under
/// `CARGO_TERM_COLOR=always` (which CI sets globally) starts its error lines with
/// an escape sequence rather than a letter. An anchor is only as good as the
/// assumption that the producer writes bare, uncoloured, unprefixed lines, and
/// neither producer here does.
const WASM_DIAGNOSTIC_MARKERS: &[&str] = &[
    "error",
    "caused by",
    "could not compile",
    "is not supported",
    "unresolved import",
    "cannot find",
    "wasm-bindgen",
];

/// A captured failed build: the full combined stdout+stderr, plus the path the
/// whole thing was written to.
///
/// This type exists because of a measured failure of what it replaces. The
/// previous shape returned a bare `String` that the caller pushed through an
/// anchored `starts_with("error")` filter capped at 8 lines. The one CI failure
/// this check has ever caught therefore reported exactly two useless lines --
/// `error from build pipeline` and `trunk`'s own timestamped echo of it -- while
/// the `Caused by:` chain naming the actual missing file never reached the log at
/// all, and the failure had to be re-diagnosed from scratch. The filter that
/// makes output readable is also the filter that can invent a silence, so the
/// full output now always goes to a file and the console view is explicitly a
/// summary *of that file*.
#[derive(Debug)]
pub struct CapturedBuild {
    /// Combined stdout+stderr of the failed command, prefixed with the command
    /// line and its real exit status.
    pub output: String,
    /// Where the full output was written. `None` only when the log could not be
    /// written — which must never itself replace the build error.
    pub log_path: Option<PathBuf>,
}

/// Runs `command`, returning `Ok(())` on a zero exit and a [`CapturedBuild`]
/// otherwise.
///
/// The verdict comes from the process's own exit status, never from what its
/// output looks like: a build that prints the word `error` and exits 0 is a
/// warning, and a build that prints nothing and exits 1 is still a failure.
fn run_captured_build(
    command: &mut Command,
    workspace_root: &Path,
    log_name: &str,
) -> Result<(), CapturedBuild> {
    // Ask cargo not to colour output we are about to machine-match, which also
    // covers the cargo `trunk` shells out to. Belt-and-braces with `strip_ansi`
    // rather than a substitute for it: this keeps the *log file* readable, and the
    // strip keeps the *matching* correct for any producer that colours anyway.
    //
    configure_captured_build(command);
    let description = format!("{command:?}");
    let output = match command.output() {
        Ok(output) => output,
        // A spawn failure is a failure like any other and is reported through
        // the same path, so it can never be mistaken for a green.
        Err(err) => {
            let text = format!("failed to spawn {description}: {err}\n");
            let log_path = write_wasm_check_log(workspace_root, log_name, &text);
            return Err(CapturedBuild {
                output: text,
                log_path,
            });
        }
    };
    if output.status.success() {
        return Ok(());
    }
    let combined = format!(
        "$ {description}\nexit status: {}\n\n{}{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let log_path = write_wasm_check_log(workspace_root, log_name, &combined);
    Err(CapturedBuild {
        output: combined,
        log_path,
    })
}

/// Makes captured builds independent of presentation variables inherited from
/// the caller. Trunk maps `NO_COLOR` to a clap boolean, so the conventional
/// `NO_COLOR=1` value aborts before the browser build begins.
fn configure_captured_build(command: &mut Command) {
    command.env_remove("NO_COLOR");
    command.env("CARGO_TERM_COLOR", "never");
}

/// Writes `contents` to `target/wasm-check/<log_name>.log`, returning its path.
///
/// A write failure is reported inline and swallowed on purpose: losing the log
/// file must degrade the diagnosis, never replace the build error with a
/// filesystem error.
fn write_wasm_check_log(
    workspace_root: &Path,
    log_name: &str,
    contents: &str,
) -> Option<PathBuf> {
    let sanitised: String = log_name
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect();
    let dir = workspace_root.join(WASM_CHECK_LOG_DIR);
    if let Err(err) = std::fs::create_dir_all(&dir) {
        println!("      │ (could not create {}: {err})", dir.display());
        return None;
    }
    let path = dir.join(format!("{sanitised}.log"));
    match std::fs::write(&path, contents) {
        Ok(()) => Some(path),
        Err(err) => {
            println!("      │ (could not write {}: {err})", path.display());
            None
        }
    }
}

/// Strips ANSI escape sequences from captured output.
///
/// Load-bearing for every match in [`select_diagnostic_lines`], not cosmetic.
/// With `CARGO_TERM_COLOR=always` set — which this repo's CI sets for every job —
/// a line that reads `error: …` on a terminal is really
/// `ESC[1mESC[31merror ESC[0m: …` in the captured bytes, and any anchored match
/// against it silently fails. Handles the CSI (`ESC [` … final byte in
/// `0x40..=0x7E`) and OSC (`ESC ]` … BEL or `ESC \`) forms; any other byte after
/// an ESC drops the ESC alone, which cannot turn a matching line into a
/// non-matching one.
#[must_use]
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch != '\x1b' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for next in chars.by_ref() {
                    if ('\x40'..='\x7e').contains(&next) {
                        break;
                    }
                }
            }
            Some(']') => {
                let mut prev_was_esc = false;
                for next in chars.by_ref() {
                    if next == '\x07' || (prev_was_esc && next == '\\') {
                        break;
                    }
                    prev_was_esc = next == '\x1b';
                }
            }
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

/// Selects the lines of `output` worth showing: every line containing one of
/// `markers`, plus the indented continuation lines that follow it.
///
/// The continuation rule is the half the previous filter lacked, and it is where
/// the whole diagnosis lives: `Caused by:`'s numbered causes and rustc's
/// `--> file:line` / `|` frames all arrive *indented, on the lines after* the one
/// that matched, so a per-line filter drops precisely the payload and keeps only
/// the headline.
#[must_use]
pub fn select_diagnostic_lines(output: &str, markers: &[&str], max_lines: usize) -> Vec<String> {
    let mut selected = Vec::new();
    let mut in_continuation = false;
    for line in output.lines() {
        if selected.len() >= max_lines {
            break;
        }
        let lower = line.to_ascii_lowercase();
        if markers.iter().any(|marker| lower.contains(marker)) {
            selected.push(line.trim_end().to_owned());
            in_continuation = true;
            continue;
        }
        let blank = line.trim().is_empty();
        if in_continuation && !blank && (line.starts_with(' ') || line.starts_with('\t')) {
            selected.push(line.trim_end().to_owned());
            continue;
        }
        if !blank {
            in_continuation = false;
        }
    }
    selected
}

/// Prints a diagnosable summary of a failed build, and says where the full
/// output is.
///
/// Three properties, each of which the anchored-grep-and-truncate version it
/// replaces lacked: matching happens on ANSI-**stripped** text; a matched line
/// brings its continuation lines with it; and when nothing matches, the tail is
/// printed **verbatim** rather than nothing at all. That last one is the
/// mechanism fix — a filter that can yield an empty summary turns a failing build
/// into a silent one, and CLAUDE.md's rule is that output which prints nothing
/// must be read as a failure to run, never as an absence of findings.
fn report_build_failure(failure: &CapturedBuild) {
    if let Some(path) = &failure.log_path {
        println!("      │ full output: {}", path.display());
    }
    let stripped = strip_ansi(&failure.output);
    let selected = select_diagnostic_lines(
        &stripped,
        WASM_DIAGNOSTIC_MARKERS,
        WASM_DIAGNOSTIC_MAX_LINES,
    );
    if !selected.is_empty() {
        for line in selected {
            println!("      │ {line}");
        }
        return;
    }
    println!(
        "      │ (no line matched the diagnostic markers — last {WASM_DIAGNOSTIC_MAX_LINES} \
         non-blank lines verbatim)"
    );
    let tail: Vec<&str> = stripped
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let start = tail.len().saturating_sub(WASM_DIAGNOSTIC_MAX_LINES);
    for line in &tail[start..] {
        println!("      │ {line}");
    }
}

/// Overrides `[profile.dev].codegen-backend = "cranelift"` (`.cargo/config.toml`)
/// back to LLVM for the one target that setting does not apply to.
///
/// `rustc_codegen_cranelift` has no wasm32 backend at all: "error: can't compile
/// for wasm32-unknown-unknown: Support for this target has not been implemented
/// yet". Cargo profiles are not target-scoped, so `[profile.dev]` reaches every
/// `--target wasm32-unknown-unknown` build the same as a native one — landing
/// Cranelift as the workspace default (see docs/compile-times.md) silently took
/// every row of this check from PASS to FAIL, compile error rather than a
/// runtime hazard, and nothing native-side could have shown it. `.cargo/config.toml`
/// cannot express "cranelift except for this target" (profile tables are not
/// conditional on target triple), so the override has to live at the call site
/// instead — the same reasoning `docs/compile-times.md` already gives for
/// `RUSTFLAGS` clobbering `build.rustflags`, just one level further out.
const WASM_CODEGEN_BACKEND_ENV: (&str, &str) = ("CARGO_PROFILE_DEV_CODEGEN_BACKEND", "llvm");

/// Runs `cargo build -p <name> --target wasm32-unknown-unknown [extra]` from
/// the workspace root, capturing the build to a log file on failure. The native
/// xtask binary's own `--target-dir` is deliberately NOT forwarded: the wasm
/// build shares the default target/ dir, exactly as the script did.
fn compile_crate_for_wasm(
    workspace_root: &Path,
    wasm_crate: &WasmCrate,
) -> Result<(), CapturedBuild> {
    let mut command = Command::new("cargo");
    command
        .arg("build")
        .arg("-p")
        .arg(wasm_crate.name)
        .arg("--target")
        .arg(WASM_TARGET)
        .args(wasm_crate.extra_args)
        .env(WASM_CODEGEN_BACKEND_ENV.0, WASM_CODEGEN_BACKEND_ENV.1)
        .current_dir(workspace_root);
    run_captured_build(&mut command, workspace_root, wasm_crate.name)
}

/// Runs `trunk build` inside web/, capturing the build to a log file on failure.
fn build_web_with_trunk(workspace_root: &Path) -> Result<(), CapturedBuild> {
    let mut command = Command::new("trunk");
    command
        .arg("build")
        .env(WASM_CODEGEN_BACKEND_ENV.0, WASM_CODEGEN_BACKEND_ENV.1)
        .current_dir(workspace_root.join("web"));
    run_captured_build(&mut command, workspace_root, "lodestone-web-trunk")
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use std::{collections::BTreeSet, ops::Deref, path::Path, process::Command};

    const REAL_REPORT: &str = ".cache/mc/26.2/generated/reports/packets.json";

    #[test]
    fn captured_builds_scrub_inherited_no_color_before_spawning() {
        let mut command = Command::new("trunk");
        command.env("NO_COLOR", "1");

        configure_captured_build(&mut command);

        let no_color = command
            .get_envs()
            .find(|(key, _)| *key == "NO_COLOR")
            .expect("the command must explicitly remove NO_COLOR from its child environment");
        assert!(no_color.1.is_none());
        assert_eq!(
            command
                .get_envs()
                .find(|(key, _)| *key == "CARGO_TERM_COLOR")
                .and_then(|(_, value)| value),
            Some(std::ffi::OsStr::new("never"))
        );
    }

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
        assert!(help.contains("version-table"));
        assert!(help.contains("gen-reports"));
        assert!(help.contains("gen-registries"));
        assert!(help.contains("codegen-ratio"));
        assert!(help.contains("new-version"));
        assert!(help.contains("conformance"));
        assert!(help.contains("wasm-check"));
        assert!(help.contains("check-ptr-const"));
        assert!(help.contains("check-comment-voice"));
    }

    #[test]
    fn cli_parses_check_ptr_const_command() -> Result<()> {
        assert_eq!(
            parse_cli_args(["check-ptr-const"])?,
            CliCommand::CheckPtrConst
        );
        Ok(())
    }

    #[test]
    fn cli_parses_check_comment_voice_command_with_default_allowlist() -> Result<()> {
        assert_eq!(
            parse_cli_args(["check-comment-voice"])?,
            CliCommand::CheckCommentVoice {
                allowlist: PathBuf::from(comment_voice::DEFAULT_ALLOWLIST)
            }
        );
        Ok(())
    }

    #[test]
    fn cli_parses_check_comment_voice_command_with_explicit_allowlist() -> Result<()> {
        assert_eq!(
            parse_cli_args(["check-comment-voice", "--allowlist", "custom.toml"])?,
            CliCommand::CheckCommentVoice {
                allowlist: PathBuf::from("custom.toml")
            }
        );
        Ok(())
    }

    #[test]
    fn cli_rejects_unknown_check_comment_voice_option() {
        assert!(parse_cli_args(["check-comment-voice", "--nope"]).is_err());
    }

    #[test]
    fn cli_parses_codegen_ratio_command() -> Result<()> {
        assert_eq!(parse_cli_args(["codegen-ratio"])?, CliCommand::CodegenRatio);
        Ok(())
    }

    // --- wasm-check --------------------------------------------------------

    fn write_fixture(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    fn demo_rule() -> ConfinementRule {
        ConfinementRule {
            label: "demo fs-confinement",
            src_dir: "crates/demo/src",
            banned: "std::fs::",
            allowlist: &[],
        }
    }

    #[test]
    fn cli_parses_wasm_check_command() -> Result<()> {
        // Flagless, like codegen-ratio / connectedness: trailing args are
        // ignored by the parser (the wasm-check run itself does the work).
        assert_eq!(parse_cli_args(["wasm-check"])?, CliCommand::WasmCheck);
        Ok(())
    }

    #[test]
    fn confinement_scanner_reports_path_line_and_content() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        write_fixture(
            tmp.path(),
            "crates/demo/src/lib.rs",
            "line one\nlet x = std::fs::read(\"a\");\nline three",
        );
        let leaks = scan_confinement(tmp.path(), &demo_rule())?;
        assert_eq!(leaks.len(), 1);
        assert_eq!(leaks[0].path, PathBuf::from("crates/demo/src/lib.rs"));
        assert_eq!(leaks[0].line, 2);
        assert_eq!(leaks[0].content, "let x = std::fs::read(\"a\");");
        Ok(())
    }

    #[test]
    fn confinement_scanner_honors_allowlist_by_basename() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        write_fixture(tmp.path(), "crates/demo/src/native.rs", "std::fs::read_allowed");
        write_fixture(tmp.path(), "crates/demo/src/lib.rs", "std::fs::read_banned");
        let rule = ConfinementRule {
            src_dir: "crates/demo/src",
            allowlist: &["native.rs"],
            ..demo_rule()
        };
        let leaks = scan_confinement(tmp.path(), &rule)?;
        assert_eq!(leaks.len(), 1);
        assert_eq!(leaks[0].path, PathBuf::from("crates/demo/src/lib.rs"));
        Ok(())
    }

    #[test]
    fn confinement_scanner_empty_allowlist_reports_every_file() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        write_fixture(tmp.path(), "crates/demo/src/a.rs", "Instant::now()");
        write_fixture(tmp.path(), "crates/demo/src/sub/b.rs", "Instant::now()");
        let rule = ConfinementRule {
            label: "demo time-confinement",
            banned: "Instant::now(",
            ..demo_rule()
        };
        let leaks = scan_confinement(tmp.path(), &rule)?;
        assert_eq!(leaks.len(), 2);
        assert_eq!(leaks[0].path, PathBuf::from("crates/demo/src/a.rs"));
        assert_eq!(leaks[1].path, PathBuf::from("crates/demo/src/sub/b.rs"));
        Ok(())
    }

    #[test]
    fn confinement_scanner_sorts_by_path_then_line() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        write_fixture(tmp.path(), "crates/demo/src/b.rs", "std::fs::\nstd::fs::\nstd::fs::");
        write_fixture(tmp.path(), "crates/demo/src/a.rs", "std::fs::");
        let leaks = scan_confinement(tmp.path(), &demo_rule())?;
        assert_eq!(leaks.len(), 4);
        assert_eq!(leaks[0].path, PathBuf::from("crates/demo/src/a.rs"));
        assert_eq!(leaks[1].path, PathBuf::from("crates/demo/src/b.rs"));
        assert_eq!(leaks[1].line, 1);
        assert_eq!(leaks[2].line, 2);
        assert_eq!(leaks[3].line, 3);
        Ok(())
    }

    #[test]
    fn confinement_scanner_missing_dir_is_an_error_not_a_pass() {
        let tmp = tempfile::tempdir().unwrap();
        let rule = ConfinementRule {
            src_dir: "crates/does-not-exist/src",
            ..demo_rule()
        };
        assert!(scan_confinement(tmp.path(), &rule).is_err());
    }

    /// `trunk` 0.21.14's real failure output, captured VERBATIM from a run that
    /// reproduced the CI failure (a detached worktree, which has no gitignored
    /// `.cache/`, exactly the runner's condition). Only the absolute path was
    /// shortened.
    ///
    /// This is the outside source the assertions below need: it is what the
    /// producer actually writes, not what we assume it writes. Note the shape
    /// that broke the old filter — every line carries an RFC-3339 timestamp and a
    /// level, so NO line starts with `error`.
    const TRUNK_FAILURE_SAMPLE: &str = concat!(
        "2026-08-09T22:13:07.017492Z  INFO 🚀 Starting trunk 0.21.14\n",
        "2026-08-09T22:13:07.018088Z  INFO 📦 starting build\n",
        "2026-08-09T22:13:07.343401Z ERROR ❌ error\n",
        "error from build pipeline\n",
        "\n",
        "Caused by:\n",
        "    0: error getting canonical path for \"/repo/web/../.cache/mc/26.2/client.jar\"\n",
        "    1: No such file or directory (os error 2)\n",
        "2026-08-09T22:13:07.343622Z ERROR error from build pipeline\n",
    );

    /// The regression this whole mechanism exists for. The previous filter was
    /// `line.starts_with(\"error\") || line.contains(\"error from\") || …`, capped
    /// at 8 lines, and against the sample above it selected exactly TWO lines,
    /// neither of which named a file or a cause — which is what CI printed.
    ///
    /// The control is the second half: the same anchored predicate is evaluated
    /// here and required to MISS the `Caused by:` chain, so this test fails if
    /// someone reintroduces an anchor and it happens to work by accident.
    #[test]
    fn diagnostic_selection_survives_ansi_and_keeps_the_caused_by_chain() {
        // Uncoloured output must survive the strip untouched; the coloured case
        // is covered by `diagnostic_selection_keeps_rustc_location_frames`.
        let stripped = strip_ansi(TRUNK_FAILURE_SAMPLE);
        assert_eq!(
            stripped, TRUNK_FAILURE_SAMPLE,
            "strip_ansi must be the identity on text with no escape sequences"
        );

        let selected = select_diagnostic_lines(&stripped, WASM_DIAGNOSTIC_MARKERS, 40);
        let joined = selected.join("\n");
        // The three things a reader needs, none of which reached the CI log.
        assert!(
            joined.contains("client.jar"),
            "the summary must name the missing file; got:\n{joined}"
        );
        assert!(
            joined.contains("No such file or directory"),
            "the summary must name the cause; got:\n{joined}"
        );
        assert!(
            joined.contains("Caused by:"),
            "the summary must keep the Caused by: header; got:\n{joined}"
        );

        // The control: the anchored predicate this replaced, run on the same
        // bytes. If it can see the cause, this test is not measuring anything.
        let anchored: Vec<&str> = TRUNK_FAILURE_SAMPLE
            .lines()
            .filter(|line| line.starts_with("error") || line.contains("error from"))
            .collect();
        assert!(
            !anchored.iter().any(|line| line.contains("client.jar")),
            "the anchored filter was supposed to MISS the cause, so this control \
             proves nothing; it selected: {anchored:?}"
        );
    }

    /// A filter that can return nothing must never *print* nothing: an empty
    /// summary reads as "no error found", which is the one thing a failing build
    /// cannot mean. Output nobody's markers match still has to be shown.
    #[test]
    fn diagnostic_selection_is_empty_only_when_the_tail_fallback_takes_over() {
        let opaque = "linker invoked\nsegmentation fault\n";
        let selected = select_diagnostic_lines(opaque, WASM_DIAGNOSTIC_MARKERS, 40);
        assert!(
            selected.is_empty(),
            "sample was meant to match no marker, so the fallback arm is what \
             `report_build_failure` would take; it selected: {selected:?}"
        );
        // The fallback prints the non-blank tail, so it is non-empty exactly when
        // the captured output is.
        let tail: Vec<&str> = opaque.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(tail, ["linker invoked", "segmentation fault"]);
    }

    /// A coloured rustc diagnostic must keep its `-->` location line, which is
    /// indented and therefore invisible to any per-line filter.
    #[test]
    fn diagnostic_selection_keeps_rustc_location_frames() {
        let sample = concat!(
            "\u{1b}[1m\u{1b}[31merror[E0433]\u{1b}[0m\u{1b}[1m: failed to resolve\u{1b}[0m\n",
            "  \u{1b}[1m\u{1b}[34m-->\u{1b}[0m crates/lodestone-server/src/lib.rs:12:5\n",
            "   \u{1b}[1m\u{1b}[34m|\u{1b}[0m\n",
        );
        let selected = select_diagnostic_lines(&strip_ansi(sample), WASM_DIAGNOSTIC_MARKERS, 40);
        let joined = selected.join("\n");
        assert!(
            joined.contains("--> crates/lodestone-server/src/lib.rs:12:5"),
            "the summary must keep rustc's location frame; got:\n{joined}"
        );
    }

    /// Serialises the two tests that walk the REAL crate directories: the positive
    /// control plants a probe file inside them, and
    /// `confinement_rules_hold_across_the_real_workspace` walks the same trees.
    ///
    /// Measured, not hypothesised: without this the control's probe was removed
    /// while the other test was mid-read and the walk died with ENOENT — a red test
    /// that says nothing about the code under guard.
    static REAL_WORKSPACE_SCAN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Workspace root, from the manifest dir rather than cwd so tests work from
    /// anywhere.
    fn wasm_test_workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .canonicalize()
            .expect("canonicalize workspace root")
    }

    /// Reads the reference script's text.
    fn reference_script_text() -> String {
        let path = wasm_test_workspace_root()
            .join("scripts")
            .join("wasm-check.sh");
        std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
    }

    /// Pulls the rows out of a `NAME=( … )` bash array of double-quoted strings,
    /// skipping the comment lines interleaved through it.
    ///
    /// The parity gates below PARSE the other implementation's table rather than
    /// restate it. That distinction is load-bearing: the previous version of
    /// `confinement_rules_match_the_reference_script_table` hard-coded a list of
    /// nine labels, so when the script grew to seventeen rules the test kept
    /// passing and `cargo xtask wasm-check` — the implementation CI runs — silently
    /// enforced eight fewer rules than the script it claimed parity with. A gate
    /// that compares one table against a copy of itself cannot tell you a third
    /// table exists.
    fn parse_script_array(script: &str, name: &str) -> Vec<String> {
        let opener = format!("{name}=(");
        let mut rows = Vec::new();
        let mut inside = false;
        for line in script.lines() {
            if !inside {
                if line.starts_with(&opener) {
                    inside = true;
                }
                continue;
            }
            if line.starts_with(')') {
                return rows;
            }
            let trimmed = line.trim();
            if let Some(body) = trimmed.strip_prefix('"').and_then(|r| r.strip_suffix('"')) {
                rows.push(body.to_string());
            }
        }
        panic!("{name}=( … ) array not found (or unterminated) in the reference script");
    }

    #[test]
    fn wasm_crates_match_the_reference_script_subset() {
        let script = reference_script_text();
        let rows = parse_script_array(&script, "CRATES");
        assert!(
            rows.len() > 10,
            "parsed only {} CRATES rows — the parser, not the table, is what broke",
            rows.len()
        );

        let expected: Vec<(String, Vec<String>)> = rows
            .iter()
            .map(|row| match row.split_once('|') {
                None => (row.clone(), Vec::new()),
                Some((pkg, extra)) => (
                    pkg.to_string(),
                    extra.split_whitespace().map(str::to_string).collect(),
                ),
            })
            .collect();

        let actual: Vec<(String, Vec<String>)> = wasm_crates()
            .iter()
            .map(|c| {
                (
                    c.name.to_string(),
                    c.extra_args.iter().map(|a| a.to_string()).collect(),
                )
            })
            .collect();

        assert_eq!(
            actual, expected,
            "wasm compile subset drifted from scripts/wasm-check.sh's CRATES table"
        );

        let names: Vec<&str> = wasm_crates().iter().map(|c| c.name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len(), "duplicate crate in wasm subset");
    }

    #[test]
    fn confinement_rules_match_the_reference_script_table() {
        let script = reference_script_text();
        let rows = parse_script_array(&script, "CONFINEMENT_RULES");
        assert!(
            rows.len() > 10,
            "parsed only {} CONFINEMENT_RULES rows — the parser, not the table, is what broke",
            rows.len()
        );

        // Every row must split into EXACTLY four `|` fields. This is the mechanical
        // catch for the defect that made five rules vacuous: they spelled a BRE
        // alternation `\(Instant\|SystemTime\)`, whose `\|` is the script's own field
        // separator, so `IFS='|' read` truncated the pattern to `std::time::\(Instant\`,
        // grep exited 2, and a swallowed error printed PASS. Under this assertion that
        // row is a red test instead.
        let malformed: Vec<&String> = rows
            .iter()
            .filter(|row| row.split('|').count() != 4)
            .collect();
        assert!(
            malformed.is_empty(),
            "confinement rule rows must have exactly 4 '|'-separated fields; a '|' \
             inside a pattern truncates it. Offending rows:\n{malformed:#?}"
        );

        let expected: Vec<(String, String, String, Vec<String>)> = rows
            .iter()
            .map(|row| {
                let fields: Vec<&str> = row.split('|').collect();
                (
                    fields[0].to_string(),
                    fields[1].to_string(),
                    fields[2].to_string(),
                    fields[3]
                        .split(',')
                        .filter(|f| !f.is_empty())
                        .map(str::to_string)
                        .collect(),
                )
            })
            .collect();

        let actual: Vec<(String, String, String, Vec<String>)> = confinement_rules()
            .iter()
            .map(|r| {
                (
                    r.label.to_string(),
                    r.src_dir.to_string(),
                    r.banned.to_string(),
                    r.allowlist.iter().map(|f| f.to_string()).collect(),
                )
            })
            .collect();

        assert_eq!(
            actual, expected,
            "confinement rules drifted from scripts/wasm-check.sh's CONFINEMENT_RULES table"
        );

        for rule in confinement_rules() {
            assert!(!rule.src_dir.is_empty(), "{} has empty src_dir", rule.label);
            assert!(!rule.banned.is_empty(), "{} has empty banned", rule.label);
            // A pattern is matched here with `str::contains` and in the script with
            // grep, so it must be a literal substring in both. `|` additionally
            // truncates the script's row; the rest would silently mean something
            // different on one side.
            for meta in ['|', '(', ')', '[', ']', '*', '+', '?', '\\'] {
                if meta == '(' && rule.banned.ends_with('(') {
                    // A trailing `(` is literal in grep's BRE and in `contains`, and
                    // `Instant::now(` deliberately relies on it to distinguish the
                    // call from the type.
                    continue;
                }
                assert!(
                    !rule.banned.contains(meta),
                    "{}: banned pattern {:?} contains regex metacharacter {meta:?}; \
                     patterns must be literal substrings in both implementations",
                    rule.label,
                    rule.banned
                );
            }
        }
    }

    #[test]
    fn every_confinement_rule_fires_under_a_planted_violation() -> Result<()> {
        // THE POSITIVE CONTROL. A confinement rule that has never been observed
        // failing is a rule you hope works — and five of them did not, for their
        // whole life, while printing PASS.
        //
        // For each rule: create ONE new file in the directory that rule scans,
        // carrying the rule's banned pattern on a non-comment line, and require the
        // scan to report it by path. No existing file is touched, so there is
        // nothing to restore; the probe uses a non-`.rs` extension so cargo never
        // considers it, and a basename no allowlist names.
        //
        // Mismatches are COLLECTED and asserted on the collection. An `assert!`
        // inside the loop would prove exactly one arm and leave the rest arguments
        // rather than observations.
        let _serial = REAL_WORKSPACE_SCAN_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let workspace_root = wasm_test_workspace_root();
        let rules = confinement_rules();
        assert!(rules.len() > 10, "suspiciously few rules to control");

        let mut fired: Vec<&str> = Vec::new();
        let mut silent: Vec<String> = Vec::new();

        for (index, rule) in rules.iter().enumerate() {
            // Per-rule filename: two rules scan the same dir, and a shared name
            // would let one rule's probe satisfy another's assertion.
            let probe_name = format!("zz_wasm_guard_control_{index}.probe");
            let probe = workspace_root.join(rule.src_dir).join(&probe_name);
            // Not a comment: the scanner drops lines opening with `//`, `*` or `#`.
            let planted = format!("let _positive_control = {}PLANTED;\n", rule.banned);
            std::fs::write(&probe, &planted)
                .with_context(|| format!("plant probe at {}", probe.display()))?;

            let outcome = scan_confinement(&workspace_root, rule);
            // Remove the probe before judging, so a failed assertion cannot leave it
            // behind and turn every later run red.
            let _ = std::fs::remove_file(&probe);

            match outcome {
                Ok(leaks) => {
                    let named = leaks
                        .iter()
                        .any(|leak| leak.path.ends_with(Path::new(&probe_name)));
                    if named {
                        fired.push(rule.label);
                    } else {
                        silent.push(format!(
                            "{}: planted {:?} in {} and the scan did not report it ({} other leak(s))",
                            rule.label,
                            rule.banned,
                            rule.src_dir,
                            leaks.len()
                        ));
                    }
                }
                Err(err) => silent.push(format!("{}: scanner errored: {err:#}", rule.label)),
            }
            assert!(
                !probe.exists(),
                "probe {} survived cleanup — remove it before re-running",
                probe.display()
            );
        }

        assert!(
            silent.is_empty(),
            "{} of {} confinement rules did NOT fire under a planted violation:\n{}",
            silent.len(),
            rules.len(),
            silent.join("\n")
        );
        assert_eq!(
            fired.len(),
            rules.len(),
            "every rule must be observed failing; fired: {fired:?}"
        );
        // Printed so `-- --nocapture` reports the count rather than only the verdict.
        println!(
            "confinement rules observed FAILING under a planted violation: {}/{}",
            fired.len(),
            rules.len()
        );
        Ok(())
    }

    #[test]
    fn confinement_scan_ignores_comment_lines_and_allowlisted_files() -> Result<()> {
        // The two ways a rule is allowed NOT to fire, each given an arm that fails
        // if the mechanism inverts. Without this, comment-stripping could silently
        // widen to "starts with any punctuation" and nothing would notice.
        let dir = tempfile::tempdir()?;
        let root = dir.path();
        let src = root.join("crates/probe/src");
        std::fs::create_dir_all(src.join("nested"))?;
        std::fs::write(
            src.join("confined.rs"),
            "use std::time::Instant;\n", // allowlisted file: must be ignored
        )?;
        std::fs::write(
            src.join("prose.rs"),
            "// use std::time::Instant; -- traps on wasm32, use crate::platform\n\
             /// `std::time::Instant` is the trapping one\n\
             //! std::time::Instant\n\
             * std::time::Instant\n\
             # std::time::Instant\n",
        )?;
        std::fs::write(
            src.join("nested/leaky.rs"),
            "fn f() {\n    let t = std::time::Instant::now();\n}\n",
        )?;

        let rule = ConfinementRule {
            label: "probe instant-ban",
            src_dir: "crates/probe/src",
            banned: "std::time::Instant",
            allowlist: &["confined.rs"],
        };
        let leaks = scan_confinement(root, &rule)?;
        let reported: Vec<String> = leaks
            .iter()
            .map(|l| format!("{}:{}", l.path.display(), l.line))
            .collect();
        assert_eq!(
            reported,
            vec!["crates/probe/src/nested/leaky.rs:2"],
            "exactly the one executable line, found recursively, must be reported"
        );

        // Control: the same tree with the allowlist emptied must report the
        // allowlisted file too — proving the previous arm's silence came from the
        // allowlist and not from the scanner failing to read the file at all.
        let unguarded = ConfinementRule {
            allowlist: &[],
            ..rule.clone()
        };
        let leaks = scan_confinement(root, &unguarded)?;
        assert_eq!(
            leaks.len(),
            2,
            "with an empty allowlist both executable lines must appear; got {leaks:#?}"
        );
        Ok(())
    }

    #[test]
    fn confinement_rules_hold_across_the_real_workspace() -> Result<()> {
        // The guard as a test: every configured rule must scan clean against
        // the real crates, so `cargo test -p xtask` (and thus `just health`)
        // trips on a leaked wasm hazard instead of waiting for a manual script
        // run. Env-var manifest dir, not cwd, so it works from any cwd.
        //
        // Shares a lock with the positive control, which plants probe files in these
        // same directories.
        let _serial = REAL_WORKSPACE_SCAN_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let workspace_root = wasm_test_workspace_root();
        let mut failures = Vec::new();
        for rule in confinement_rules() {
            let leaks = scan_confinement(&workspace_root, &rule)?;
            if !leaks.is_empty() {
                failures.push(format!("{}: {} leak(s)", rule.label, leaks.len()));
                for leak in &leaks {
                    failures.push(format!(
                        "  {}:{}:{}",
                        leak.path.display(),
                        leak.line,
                        leak.content
                    ));
                }
            }
        }
        assert!(
            failures.is_empty(),
            "wasm confinement guards leaked:\n{}",
            failures.join("\n")
        );
        Ok(())
    }

    #[test]
    fn cli_parses_version_table_command() -> Result<()> {
        assert_eq!(
            parse_cli_args(["version-table"])?,
            CliCommand::VersionTable {
                check: false,
                fetch_missing: false,
            }
        );
        assert_eq!(
            parse_cli_args(["version-table", "--check", "--fetch-missing"])?,
            CliCommand::VersionTable {
                check: true,
                fetch_missing: true,
            }
        );
        Ok(())
    }

    #[test]
    fn cli_parses_docs_index_command() -> Result<()> {
        assert_eq!(
            parse_cli_args(["docs-index"])?,
            CliCommand::DocsIndex { check: false }
        );
        assert_eq!(
            parse_cli_args(["docs-index", "--check"])?,
            CliCommand::DocsIndex { check: true }
        );
        assert!(parse_cli_args(["docs-index", "--nope"]).is_err());
        assert!(root_help().contains("docs-index"));
        Ok(())
    }

    #[test]
    fn cli_parses_islands_command() -> Result<()> {
        assert_eq!(
            parse_cli_args(["islands"])?,
            CliCommand::Islands { only_crate: None }
        );
        assert_eq!(
            parse_cli_args(["islands", "--crate", "lodestone-entity"])?,
            CliCommand::Islands {
                only_crate: Some("lodestone-entity".to_string())
            }
        );
        assert!(parse_cli_args(["islands", "--nope"]).is_err());
        assert!(root_help().contains("islands"));
        Ok(())
    }

    #[test]
    fn extract_doc_summary_prefers_what_it_is_section() -> Result<()> {
        let text = "# Example doc\n\n**Status:** a long preamble that should be skipped\nentirely because a real section follows.\n\n## What it is\n\nThe real summary paragraph,\nwrapped across two source lines.\n\n## How it works\n\nThis part must never be quoted.\n";
        let (title, summary) = extract_doc_summary(text, "example.md")?;
        assert_eq!(title, "Example doc");
        assert_eq!(
            summary,
            "The real summary paragraph, wrapped across two source lines."
        );
        Ok(())
    }

    #[test]
    fn extract_doc_summary_accepts_what_this_is_spelling() -> Result<()> {
        let text = "# Example\n\n## What this is\n\nA summary using the second spelling.\n";
        let (_, summary) = extract_doc_summary(text, "example.md")?;
        assert_eq!(summary, "A summary using the second spelling.");
        Ok(())
    }

    /// Regression for the real bug this generator's first draft shipped:
    /// `docs/research/combat-scope.md`'s summary paragraph contains
    /// `#12/#72/#98/#121)` (an issue-reference list), and a naive
    /// `starts_with('#')` heading check truncated the summary right before
    /// it, mid-sentence. A real ATX heading needs a space (or EOL) after the
    /// `#`s.
    #[test]
    fn extract_doc_summary_does_not_treat_issue_references_as_headings() -> Result<()> {
        let text = "# Scoping doc\n\n## What it is\n\nSee issues landed under\n#12/#72/#98/#121), which continue the sentence.\n\n## Next heading\n\nUnreachable.\n";
        let (_, summary) = extract_doc_summary(text, "scoping.md")?;
        assert_eq!(
            summary,
            "See issues landed under #12/#72/#98/#121), which continue the sentence."
        );
        Ok(())
    }

    #[test]
    fn extract_doc_summary_falls_back_to_paragraph_under_h1() -> Result<()> {
        let text = "# Legacy doc\n\nNo `What it is` heading exists in this one, so the first\nparagraph under the H1 is the summary.\n\n## Some other heading\n\nNot this.\n";
        let (title, summary) = extract_doc_summary(text, "legacy.md")?;
        assert_eq!(title, "Legacy doc");
        assert_eq!(
            summary,
            "No `What it is` heading exists in this one, so the first paragraph under the H1 is the summary."
        );
        Ok(())
    }

    /// Anti-vacuity control: a doc with no prose anywhere (no `What it is`
    /// section, and nothing but headings right after the H1) must fail
    /// loudly and name the file -- never emit a blank summary. Run and
    /// watched fail per `CLAUDE.md`'s evidence standard for a negative
    /// assertion.
    #[test]
    fn extract_doc_summary_fails_loudly_with_no_usable_prose() {
        let text = "# Heading-only doc\n\n## Immediately another heading\n\n### And another\n";
        let error = extract_doc_summary(text, "heading-only.md").unwrap_err();
        assert!(
            error.to_string().contains("heading-only.md"),
            "error must name the offending file: {error}"
        );
    }

    #[test]
    fn extract_doc_summary_fails_loudly_with_no_h1() {
        let text = "## What it is\n\nNo H1 above this.\n";
        let error = extract_doc_summary(text, "no-h1.md").unwrap_err();
        assert!(error.to_string().contains("no-h1.md"));
    }

    /// Every real doc under `docs/` (minus the explicit skip list) must
    /// produce a usable title and summary -- this is the check that would
    /// fail loudly, naming the file, the moment a new doc lands without a
    /// `## What it is` section and no prose under its H1 either.
    #[test]
    fn generate_docs_index_succeeds_over_the_real_doc_tree() -> Result<()> {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let generated = generate_docs_index(&workspace_root)?;
        assert!(generated.contains("# Lodestone docs"));
        assert!(generated.contains("## Roadmap"));
        assert!(generated.contains("## Plans and research"));
        // Every doc that exists on disk must appear as a link target
        // somewhere in the output, so nothing was silently dropped.
        for path in read_md_dir_sorted(&workspace_root.join("docs"))? {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            if name == "README.md" || DOCS_INDEX_SKIP.contains(&name) {
                continue;
            }
            assert!(
                generated.contains(name),
                "docs/{name} is missing from the generated index"
            );
        }
        Ok(())
    }

    /// The drift guard: `docs/README.md` must be exactly what the generator
    /// produces from the current doc tree. Regenerate with
    /// `LODESTONE_REGEN=1 cargo test -p xtask docs_index_matches_committed`
    /// (same pattern as `crates/lodestone-data/tests/hardness.rs`'s
    /// `committed_table_matches_dump`) or `cargo xtask docs-index`.
    #[test]
    fn docs_index_matches_committed() -> Result<()> {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let generated = generate_docs_index(&workspace_root)?;

        if std::env::var_os("LODESTONE_REGEN").is_some() {
            let out_path = docs_index_out_path(&workspace_root);
            std::fs::write(&out_path, &generated)?;
            eprintln!("regenerated {}", out_path.display());
            return Ok(());
        }

        let committed = std::fs::read_to_string(docs_index_out_path(&workspace_root))?;
        assert_eq!(
            generated, committed,
            "docs/README.md is stale vs the doc tree -- regenerate with `cargo xtask docs-index` \
             or `LODESTONE_REGEN=1 cargo test -p xtask docs_index_matches_committed`"
        );
        Ok(())
    }

    #[test]
    fn cli_parses_bench_compare_command() -> Result<()> {
        assert_eq!(
            parse_cli_args([
                "bench-compare",
                "bench-results/foo.jsonl",
                "--metric",
                "m",
                "--scene",
                "s",
            ])?,
            CliCommand::BenchCompare {
                path: PathBuf::from("bench-results/foo.jsonl"),
                metric: "m".to_string(),
                scene: "s".to_string(),
                baseline_sha: None,
                candidate_sha: None,
                tolerance: 0.25,
            }
        );
        assert_eq!(
            parse_cli_args([
                "bench-compare",
                "bench-results/foo.jsonl",
                "--metric",
                "m",
                "--scene",
                "s",
                "--baseline",
                "abc123",
                "--candidate",
                "def456",
                "--tolerance",
                "10",
            ])?,
            CliCommand::BenchCompare {
                path: PathBuf::from("bench-results/foo.jsonl"),
                metric: "m".to_string(),
                scene: "s".to_string(),
                baseline_sha: Some("abc123".to_string()),
                candidate_sha: Some("def456".to_string()),
                tolerance: 0.10,
            }
        );
        assert!(parse_cli_args(["bench-compare", "p.jsonl", "--metric", "m"]).is_err());
        assert!(root_help().contains("bench-compare"));
        Ok(())
    }

    fn bench_record(sha: &str, ts: u64, value: f64) -> BenchRecord {
        BenchRecord {
            timestamp: ts,
            git_sha: sha.to_string(),
            machine: "macbook.local".to_string(),
            profile: "release".to_string(),
            scene: "test scene".to_string(),
            metric: "test_metric".to_string(),
            value,
            unit: "ms".to_string(),
        }
    }

    #[test]
    fn compare_bench_records_defaults_to_latest_vs_immediately_preceding() -> Result<()> {
        let records = vec![
            bench_record("aaa000000000", 1, 10.0),
            bench_record("bbb000000000", 2, 11.0),
            bench_record("ccc000000000", 3, 20.0),
        ];
        let opts = BenchCompareOptions {
            metric: "test_metric".to_string(),
            scene: "test scene".to_string(),
            candidate_sha: None,
            baseline_sha: None,
            tolerance: 0.25,
        };
        let report = compare_bench_records(&records, &opts)?;
        assert_eq!(report.baseline.git_sha, "bbb000000000");
        assert_eq!(report.candidate.git_sha, "ccc000000000");
        assert!((report.ratio - (20.0 / 11.0)).abs() < 1e-9);
        assert!(!report.within_tolerance(), "20/11 ~= 1.818, well outside +/-25%");
        Ok(())
    }

    #[test]
    fn compare_bench_records_selects_by_explicit_sha_prefix() -> Result<()> {
        let records = vec![
            bench_record("aaa000000000", 1, 10.0),
            bench_record("bbb000000000", 2, 11.0),
            bench_record("ccc000000000", 3, 20.0),
        ];
        let opts = BenchCompareOptions {
            metric: "test_metric".to_string(),
            scene: "test scene".to_string(),
            candidate_sha: Some("ccc".to_string()),
            baseline_sha: Some("aaa".to_string()),
            tolerance: 0.25,
        };
        let report = compare_bench_records(&records, &opts)?;
        assert_eq!(report.baseline.value, 10.0);
        assert_eq!(report.candidate.value, 20.0);
        assert!((report.ratio - 2.0).abs() < 1e-9);
        Ok(())
    }

    /// Anti-vacuity control for the tolerance check itself: a ratio of
    /// exactly 1.0 (identical value) must read as within tolerance, run and
    /// watched to actually assert `true`, not merely constructed.
    #[test]
    fn compare_bench_records_within_tolerance_reports_ok_for_identical_values() -> Result<()> {
        let records = vec![bench_record("aaa000000000", 1, 5.0), bench_record("bbb000000000", 2, 5.0)];
        let opts = BenchCompareOptions {
            metric: "test_metric".to_string(),
            scene: "test scene".to_string(),
            candidate_sha: None,
            baseline_sha: None,
            tolerance: 0.25,
        };
        let report = compare_bench_records(&records, &opts)?;
        assert!(report.within_tolerance());
        assert!(report.render().contains("-> OK"));
        Ok(())
    }

    /// The negative control for the above: run and watched to actually
    /// fail the same `within_tolerance` predicate, not merely assumed to.
    #[test]
    fn compare_bench_records_outside_tolerance_reports_flagged() -> Result<()> {
        let records = vec![bench_record("aaa000000000", 1, 5.0), bench_record("bbb000000000", 2, 50.0)];
        let opts = BenchCompareOptions {
            metric: "test_metric".to_string(),
            scene: "test scene".to_string(),
            candidate_sha: None,
            baseline_sha: None,
            tolerance: 0.25,
        };
        let report = compare_bench_records(&records, &opts)?;
        assert!(!report.within_tolerance());
        assert!(report.render().contains("FLAGGED"));
        Ok(())
    }

    #[test]
    fn compare_bench_records_rejects_cross_machine_comparison() {
        let mut older = bench_record("aaa000000000", 1, 5.0);
        older.machine = "other-machine".to_string();
        let records = vec![older, bench_record("bbb000000000", 2, 5.0)];
        let opts = BenchCompareOptions {
            metric: "test_metric".to_string(),
            scene: "test scene".to_string(),
            candidate_sha: None,
            baseline_sha: Some("aaa".to_string()),
            tolerance: 0.25,
        };
        let error = compare_bench_records(&records, &opts).unwrap_err();
        assert!(error.to_string().contains("not the same machine"));
    }

    #[test]
    fn compare_bench_records_errors_when_no_records_match() {
        let records = vec![bench_record("aaa000000000", 1, 5.0)];
        let opts = BenchCompareOptions {
            metric: "nonexistent".to_string(),
            scene: "test scene".to_string(),
            candidate_sha: None,
            baseline_sha: None,
            tolerance: 0.25,
        };
        assert!(compare_bench_records(&records, &opts).is_err());
    }

    /// Demonstration against this repo's own real recorded data, satisfying
    /// the requirement that this tool be "used by at least one sibling
    /// benchmark as a demonstration". `#[ignore]`d because it depends on the *contents* of
    /// a gitignored, machine-local file that keeps growing every time anyone
    /// runs the bench -- not hermetic, but valuable to run by hand.
    #[test]
    #[ignore = "depends on the local, gitignored bench-results/light_propagation.jsonl history"]
    fn bench_compare_against_real_light_propagation_history() -> Result<()> {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let path = workspace_root.join("bench-results/light_propagation.jsonl");
        let records = read_bench_records(&path)?;
        let report = compare_bench_records(
            &records,
            &BenchCompareOptions {
                metric: "neighbourhood_factor_vs_single".to_string(),
                scene: "3x3 realistic terrain neighbourhood".to_string(),
                candidate_sha: None,
                baseline_sha: None,
                tolerance: 0.25,
            },
        )?;
        println!("{}", report.render());
        assert!(
            report.within_tolerance(),
            "issue #80's fixture consolidation should not have changed this bench's numbers"
        );
        Ok(())
    }

    #[test]
    fn epic_343_versions_lists_exactly_sixteen_versions_in_release_order() {
        assert_eq!(EPIC_343_VERSIONS.len(), 16);
        assert_eq!(EPIC_343_VERSIONS[0], "1.7.10");
        assert_eq!(EPIC_343_VERSIONS[EPIC_343_VERSIONS.len() - 1], "26.2");
        // No duplicates.
        let unique: BTreeSet<&str> = EPIC_343_VERSIONS.iter().copied().collect();
        assert_eq!(unique.len(), EPIC_343_VERSIONS.len());
    }

    #[test]
    fn render_version_table_source_is_deterministic_and_parses_expected_rows() -> Result<()> {
        let entries = vec![
            VersionTableEntry {
                minecraft_version: "1.7.10".to_owned(),
                protocol_version: 5,
                data_version: 18,
                release_date: "2014-05-14T17:29:23+00:00".to_owned(),
                protocol_source: VersionSource::MinecraftData,
                data_version_source: VersionSource::MinecraftData,
                cross_checked: false,
            },
            VersionTableEntry {
                minecraft_version: "26.2".to_owned(),
                protocol_version: 776,
                data_version: 4903,
                release_date: "2026-06-16T12:03:33+00:00".to_owned(),
                protocol_source: VersionSource::JarVersionJson,
                data_version_source: VersionSource::JarVersionJson,
                cross_checked: true,
            },
        ];

        let first = render_version_table_source(&entries)?;
        let second = render_version_table_source(&entries)?;
        assert_eq!(first, second, "rendering must be deterministic");
        assert!(first.contains("\"1.7.10\""));
        assert!(first.contains("\"26.2\""));
        assert!(first.contains("protocol_version: 5"));
        assert!(first.contains("protocol_version: 776"));
        assert!(first.contains("Source::MinecraftData"));
        assert!(first.contains("Source::JarVersionJson"));
        assert!(first.contains("@generated by `cargo run -p xtask -- version-table`"));
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
            "crates/versions/26.2/src/generated/packet_ids.rs",
        ])?;

        assert_eq!(
            command,
            CliCommand::GenPacketIds {
                minecraft_version: "26.2".to_owned(),
                protocol_version: 776,
                check: true,
                out: Some(PathBuf::from(
                    "crates/versions/26.2/src/generated/packet_ids.rs"
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
            "crates/versions/26.2/src/generated",
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
                    out_dir: PathBuf::from("crates/versions/26.2/src/generated"),
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

    /// `sound_events`/`particle_types`/`menus`/`items`/`data_component_types`
    /// are game data, not protocol data, and the registry extraction moved
    /// their committed tables to `crates/lodestone-data/src/generated`
    /// without anyone updating this default -- `gen-registries` (and
    /// `conformance`'s registry step, which shares this default) kept
    /// pointing at the old `crates/versions/26.2/src/generated` location,
    /// which has not held these tables since. This asserts the default
    /// resolves to where the tables actually live now.
    #[test]
    fn gen_registries_default_out_dir_is_lodestone_data() -> Result<()> {
        let command = parse_cli_args(["gen-registries", "--version", "26.2", "--protocol", "776"])?;

        let CliCommand::GenRegistries { options } = command else {
            panic!("expected GenRegistries, got {command:?}");
        };
        assert_eq!(
            options.out_dir,
            PathBuf::from("crates/lodestone-data/src/generated")
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
    fn cli_parses_connectedness_command() -> Result<()> {
        assert_eq!(
            parse_cli_args(["connectedness"])?,
            CliCommand::Connectedness
        );
        Ok(())
    }

    #[test]
    fn packet_id_play_counts_use_nested_play_modules() -> Result<()> {
        let source = r#"
pub mod login {
    pub mod clientbound {
        pub const LOGIN: i32 = 0;
        pub static ENTRIES: &[(&str, i32)] = &[("minecraft:login", LOGIN)];
    }
}
pub mod play {
    pub mod clientbound {
        pub const CHAT: i32 = 0;
        pub const BLOCK_UPDATE: i32 = 1;
        pub static ENTRIES: &[(&str, i32)] = &[
            ("minecraft:chat", CHAT),
            ("minecraft:block_update", BLOCK_UPDATE),
        ];
    }
    pub mod serverbound {
        pub const CHAT: i32 = 0;
        pub static ENTRIES: &[(&str, i32)] = &[("minecraft:chat", CHAT)];
    }
}
"#;

        let ids = parse_play_packet_id_summary(source)?;
        assert_eq!(ids.clientbound.len(), 2);
        assert_eq!(ids.serverbound.len(), 1);
        assert!(
            ids.clientbound
                .iter()
                .any(|packet| packet.const_name == "CHAT")
        );
        Ok(())
    }

    #[test]
    fn connectedness_classifier_ground_truths_direct_delegated_world_and_stranded() -> Result<()> {
        let adapter = r#"
fn handle_add_entity(payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
    Ok(vec![Directive::Emit(ClientEvent::EntitySpawned { id: 1 })])
}

fn handle_play(
    &self,
    world: &mut dyn WorldSink,
    packet_id: i32,
    payload: &[u8],
) -> Result<Vec<Directive>, AdapterError> {
    if packet_id == play::clientbound::SYSTEM_CHAT {
        return Ok(vec![Directive::Emit(ClientEvent::Chat { text })]);
    }
    if packet_id == play::clientbound::ADD_ENTITY {
        return handle_add_entity(payload);
    }
    if packet_id == play::clientbound::BLOCK_UPDATE {
        world.set_block(pos, state);
        return Ok(Vec::new());
    }
    if packet_id == play::clientbound::SET_OBJECTIVE {
        decode_and_validate::<SetObjective>(payload)?;
        return Ok(Vec::new());
    }
    if packet_id == play::clientbound::MYSTERY {
        return parse_mystery(payload);
    }
    Ok(Vec::new())
}
"#;

        let functions = extract_functions(adapter)?;
        let arms = classify_clientbound_dispatch(adapter, &functions, "src/adapter.rs", 4)?;
        assert_eq!(
            arms.get("SYSTEM_CHAT").map(|arm| &arm.verdict),
            Some(&ClientboundVerdict::Emits {
                outlet: ConsumerOutlet::ClientEvent,
                via: None,
            })
        );
        assert_eq!(
            arms.get("ADD_ENTITY").map(|arm| &arm.verdict),
            Some(&ClientboundVerdict::Emits {
                outlet: ConsumerOutlet::ClientEvent,
                via: Some("handle_add_entity".to_owned()),
            })
        );
        assert_eq!(
            arms.get("BLOCK_UPDATE").map(|arm| &arm.verdict),
            Some(&ClientboundVerdict::Emits {
                outlet: ConsumerOutlet::WorldSink,
                via: None,
            })
        );
        assert_eq!(
            arms.get("SET_OBJECTIVE").map(|arm| &arm.verdict),
            Some(&ClientboundVerdict::DecodedButStranded)
        );
        assert!(matches!(
            arms.get("MYSTERY").map(|arm| &arm.verdict),
            Some(ClientboundVerdict::Unclassified { .. })
        ));
        assert!(
            arms.len() >= 5,
            "anti-vacuity: classifier saw {}",
            arms.len()
        );
        Ok(())
    }

    /// `delegate_function_calls` scans raw source text (comments included --
    /// it has none of `find_outside_comments`/`matching_brace`'s comment
    /// awareness) for `identifier(` calls by walking backward from each `(`
    /// with `str::rfind` to find the start of the identifier. `rfind` hands
    /// back the **byte** index where the matching (non-identifier) character
    /// *starts*, and the old code did `idx + 1` to step past it -- correct
    /// only if that character is one byte (ASCII). A multi-byte character
    /// sitting directly against an identifier, with no space between (the
    /// shape a comment like `note—decode(payload)` takes), makes `idx + 1`
    /// land mid-character, and the subsequent `body[name_start..name_end]`
    /// panics with "byte index N is not a char boundary".
    ///
    /// This is a distinct bug from the lifetime-vs-char-literal one fixed in
    /// `e164d06` (`char_literal_span`): that one was in the three
    /// comment/string scanners and is already repaired. This one is in the
    /// unrelated identifier-boundary arithmetic here, still `idx + 1`, and it
    /// is exactly the class CLAUDE.md's evidence standard requires a test
    /// that visibly fails before the fix for. Em dash (3 bytes), `é` (2
    /// bytes), and `中` (3 bytes) are the minimal non-ASCII set: pure ASCII
    /// input cannot exercise a char-boundary bug at all.
    #[test]
    fn delegate_function_calls_does_not_panic_on_multibyte_characters_before_an_identifier() {
        let mut functions = BTreeMap::new();
        functions.insert("decode".to_owned(), FunctionBody { body: "" });
        for body in [
            "// note—decode(payload)\n",
            "// café—decode(payload)\n",
            "// 中—decode(payload)\n",
        ] {
            // Not just "does not panic": the identifier extraction must
            // still land on the right boundary and find the real call, or a
            // fix that merely avoided the panic (e.g. by giving up on the
            // whole line) would pass a vacuous version of this test.
            let delegates = delegate_function_calls(body, &functions);
            assert_eq!(
                delegates,
                vec!["decode".to_owned()],
                "wrong delegate extracted from {body:?}"
            );
        }
    }

    #[test]
    fn connectedness_report_uses_external_denominators_and_serverbound_encodes() -> Result<()> {
        let workspace = connectedness_fixture_workspace()?;

        let report = connectedness_report(&workspace)?;
        // v9 (the fixture's other family, with an empty adapter.rs and an
        // all-IGNORED packet_ids.rs) is measured too now that the hard
        // `family != "v770"` filter is gone — the whole point of job 1a.
        assert_eq!(
            report.families.iter().map(|f| f.family.as_str()).collect::<Vec<_>>(),
            vec!["v9", "26.2"]
        );
        assert!(
            report.skipped.is_empty(),
            "fixture families both have packet_ids.rs and adapter.rs: {:?}",
            report.skipped
        );
        let v9 = report
            .families
            .iter()
            .find(|family| family.family == "v9")
            .expect("v9 fixture family exists");
        assert_eq!(v9.play_clientbound_total, 1);
        assert_eq!(v9.examined_clientbound_arms, 0);
        assert_eq!(
            v9.serverbound_decode,
            ServerboundDecodeAxis::NotApplicable(
                "no src/server_protocol.rs — family does not implement ServerProtocol, so it \
                 cannot host"
                    .to_owned()
            )
        );

        let family = report
            .families
            .iter()
            .find(|family| family.family == "26.2")
            .expect("26.2 fixture family exists");
        assert_eq!(family.play_clientbound_total, 5);
        assert_eq!(family.play_clientbound_reaches_consumer, 3);
        assert_eq!(family.play_clientbound_decoded, 4);
        assert_eq!(family.play_clientbound_emits, 3);
        assert_eq!(
            family.play_clientbound_stranded_names,
            vec!["SET_OBJECTIVE".to_owned()]
        );
        assert_eq!(family.play_serverbound_total, 2);
        assert_eq!(family.play_serverbound_encoded, 1);
        assert_eq!(family.examined_clientbound_arms, 5);
        assert_eq!(family.unclassified.len(), 1);
        // This fixture's v770 has no src/server_protocol.rs either — the
        // fixture predates job 1b and is intentionally left that way so this
        // test still isolates the clientbound axis. The serverbound-decode
        // axis has its own fixture and tests below.
        assert_eq!(
            family.serverbound_decode,
            ServerboundDecodeAxis::NotApplicable(
                "no src/server_protocol.rs — family does not implement ServerProtocol, so it \
                 cannot host"
                    .to_owned()
            )
        );
        assert!(report.render().contains(
            "26.2  clientbound decoded 4/5; emits 3/5; decoded-but-stranded 1 [SET_OBJECTIVE]"
        ));
        assert!(!report.render().contains("consumed"));
        Ok(())
    }

    #[test]
    fn connectedness_report_names_families_it_could_not_scan_instead_of_dropping_them() -> Result<()>
    {
        let workspace = connectedness_fixture_workspace()?;
        // A third family directory matching the `vNN` naming convention but
        // missing `adapter.rs` — standing in for a family that has
        // bit-rotted past scannability. Before job 1a this test would have
        // been moot (only the flagship family was ever scanned); now every family directory
        // is examined, so a family that can't be measured has to say so.
        std::fs::create_dir_all(workspace.join("crates/versions/v5/src/generated"))?;
        std::fs::write(
            workspace.join("crates/versions/v5/src/generated/packet_ids.rs"),
            "pub mod play { pub mod clientbound { pub const X: i32 = 0; pub static ENTRIES: &[(&str, i32)] = &[(\"minecraft:x\", X)]; } pub mod serverbound { pub const X: i32 = 0; pub static ENTRIES: &[(&str, i32)] = &[(\"minecraft:x\", X)]; } }",
        )?;

        let report = connectedness_report(&workspace)?;
        assert_eq!(
            report
                .families
                .iter()
                .map(|f| f.family.as_str())
                .collect::<Vec<_>>(),
            vec!["v9", "26.2"],
            "v5 has no adapter.rs and must be named as skipped, not silently absent"
        );
        assert_eq!(report.skipped.len(), 1);
        assert_eq!(report.skipped[0].0, "v5");
        assert!(
            report.skipped[0].1.contains("adapter.rs"),
            "skip reason should name the missing file: {}",
            report.skipped[0].1
        );
        assert!(report.render().contains("SKIPPED"));
        assert!(report.render().contains("v5"));
        Ok(())
    }

    /// Positive control for the `src/adapter/` directory-module shape
    /// (26.2's actual layout, split across `mod.rs` + submodules such as
    /// `chat.rs`). Before this was handled, `connectedness_report` only ever
    /// looked for a flat `src/adapter.rs`, so any family shaped like this —
    /// 26.2 included, once its adapter grew past one file — was silently
    /// SKIPPED rather than scanned: the tool's own stated purpose
    /// ("Report v770 play packet reachability") was unmet by exactly the
    /// family it exists to check. This fixture also exercises cross-file
    /// delegate-following: `mod.rs`'s dispatch arm calls a helper defined in
    /// a sibling submodule, which only resolves if the functions table is
    /// built across every file in the module rather than one at a time.
    #[test]
    fn connectedness_scans_a_directory_module_adapter_and_follows_cross_file_delegates()
    -> Result<()> {
        let workspace = fresh_test_workspace("connectedness-dir-adapter")?;
        let root = workspace.deref();
        let family = root.join("crates/versions/v771");
        std::fs::create_dir_all(family.join("src/generated"))?;
        std::fs::write(
            family.join("src/generated/packet_ids.rs"),
            r#"
pub mod play {
    pub mod clientbound {
        pub const SYSTEM_CHAT: i32 = 0;
        pub static ENTRIES: &[(&str, i32)] = &[("minecraft:system_chat", SYSTEM_CHAT)];
    }
    pub mod serverbound {
        pub const CHAT: i32 = 0;
        pub static ENTRIES: &[(&str, i32)] = &[("minecraft:chat", CHAT)];
    }
}
"#,
        )?;
        std::fs::create_dir_all(family.join("src/adapter"))?;
        // mod.rs's dispatch arm delegates to a helper it does not itself
        // define -- `handle_system_chat` lives in the sibling `chat.rs`.
        std::fs::write(
            family.join("src/adapter/mod.rs"),
            r#"
mod chat;
use chat::handle_system_chat;

fn handle_play(
    &self,
    world: &mut dyn WorldSink,
    packet_id: i32,
    payload: &[u8],
) -> Result<Vec<Directive>, AdapterError> {
    if packet_id == play::clientbound::SYSTEM_CHAT {
        return handle_system_chat(payload);
    }
    Ok(Vec::new())
}
"#,
        )?;
        std::fs::write(
            family.join("src/adapter/chat.rs"),
            r#"
fn handle_system_chat(payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
    Ok(vec![Directive::Emit(ClientEvent::Chat { text: String::new() })])
}
"#,
        )?;

        let report = connectedness_report(&workspace)?;
        assert!(
            report.skipped.is_empty(),
            "a directory-module adapter must not be skipped: {:?}",
            report.skipped
        );
        let family_report = report
            .families
            .iter()
            .find(|f| f.family == "v771")
            .expect("v771 must appear in the scanned families, not be silently dropped");
        assert_eq!(
            family_report.play_clientbound_emits, 1,
            "SYSTEM_CHAT's handler lives in a sibling submodule (chat.rs); if the functions \
             table were built per-file instead of across the whole adapter module, the \
             delegate-follow would fail to resolve it and this would be 0"
        );
        assert!(
            family_report.unclassified.is_empty(),
            "unclassified: {:?}",
            family_report.unclassified
        );
        Ok(())
    }

    #[test]
    fn match_arm_body_stops_at_top_level_comma_for_bare_expression_arms() {
        // The naive scanner this replaces (`find('{')` from the clientbound
        // classifier) would, on FOO's bare-expression arm, keep searching
        // and swallow BAR's entire braced body instead.
        let source = "State::Play if packet_id == play::serverbound::FOO => ServerBound::Ignored,\nState::Play if packet_id == play::serverbound::BAR => {\n    ServerBound::Bar { id: 1 }\n}\n";
        let arrow = source.find("=>").expect("first arrow");
        let (start, end) = match_arm_body(source, arrow + 2).expect("body found");
        let body = source[start..end].trim();
        assert_eq!(body, "ServerBound::Ignored");
        assert!(
            !body.contains("Bar"),
            "swallowed the next arm's body: {body:?}"
        );
    }

    #[test]
    fn match_arm_body_handles_braced_arms_and_nested_delimiters_before_the_comma() {
        let source = "=> { ServerBound::Foo { id: vec![1, 2].len() as i64 } },\nnext";
        let (start, end) = match_arm_body(source, 2).expect("body found");
        assert_eq!(
            source[start..end].trim(),
            "ServerBound::Foo { id: vec![1, 2].len() as i64 }"
        );

        let source2 = "=> call(a, (b, c), [d, e]),\nnext";
        let (start2, end2) = match_arm_body(source2, 2).expect("body found");
        assert_eq!(source2[start2..end2].trim(), "call(a, (b, c), [d, e])");
    }

    #[test]
    fn find_outside_comments_skips_matches_inside_comments_and_strings() {
        let source = "// ServerBound::Foo mentioned here should not count\nlet s = \"ServerBound::Foo also should not count\";\nServerBound::Foo { x } => real_body(),\n";
        let found = find_outside_comments(source, 0, "ServerBound::Foo").expect("real occurrence found");
        let snippet = &source[found..(found + 24).min(source.len())];
        assert!(
            snippet.contains("Foo { x }"),
            "found the wrong occurrence: {snippet:?}"
        );
    }

    #[test]
    fn classify_serverbound_decode_ground_truths_emits_always_ignored_and_unclassified()
    -> Result<()> {
        let source = r#"
fn decode_mystery_action(payload: &[u8]) -> ServerBound {
    match decode_full::<MysteryAction>(payload) {
        Some(m) => ServerBound::MysteryAction { id: m.id },
        None => ServerBound::Ignored,
    }
}

impl ServerProtocol for V999ServerProtocol {
    fn decode(&self, state: lodestone_core::State, packet_id: i32, payload: &[u8]) -> ServerBound {
        match state {
            State::Play if packet_id == play::serverbound::KEEP_ALIVE => {
                match decode_full::<KeepAlive>(payload) {
                    Some(k) => ServerBound::KeepAlive { id: k.id },
                    None => ServerBound::Ignored,
                }
            }
            State::Play if packet_id == play::serverbound::PING => ServerBound::Ignored,
            State::Play if packet_id == play::serverbound::MYSTERY_ACTION => {
                decode_mystery_action(payload)
            }
            State::Play if packet_id == play::serverbound::WEIRD => {
                external_helper(payload)
            }
            _ => ServerBound::Ignored,
        }
    }
}
"#;
        let arms = classify_serverbound_decode(source, 4)?;
        assert_eq!(
            arms.get("KEEP_ALIVE").map(|arm| &arm.verdict),
            Some(&ServerboundDecodeVerdict::Emits {
                variants: vec!["KeepAlive".to_owned()],
                via: None,
            })
        );
        assert_eq!(
            arms.get("PING").map(|arm| &arm.verdict),
            Some(&ServerboundDecodeVerdict::AlwaysIgnored)
        );
        assert_eq!(
            arms.get("MYSTERY_ACTION").map(|arm| &arm.verdict),
            Some(&ServerboundDecodeVerdict::Emits {
                variants: vec!["MysteryAction".to_owned()],
                via: Some("decode_mystery_action".to_owned()),
            })
        );
        assert!(matches!(
            arms.get("WEIRD").map(|arm| &arm.verdict),
            Some(ServerboundDecodeVerdict::Unclassified { .. })
        ));
        assert!(
            arms.len() >= 4,
            "anti-vacuity: classifier saw {}",
            arms.len()
        );
        Ok(())
    }

    #[test]
    fn classify_serverbound_decode_reports_depth_limited_when_cap_is_too_low() -> Result<()> {
        let source = r#"
fn inner(payload: &[u8]) -> ServerBound {
    ServerBound::Foo { id: 1 }
}
fn middle(payload: &[u8]) -> ServerBound {
    inner(payload)
}
impl ServerProtocol for V999ServerProtocol {
    fn decode(&self, state: lodestone_core::State, packet_id: i32, payload: &[u8]) -> ServerBound {
        match state {
            State::Play if packet_id == play::serverbound::CHAINED => {
                middle(payload)
            }
            _ => ServerBound::Ignored,
        }
    }
}
"#;
        let arms = classify_serverbound_decode(source, 1)?;
        match arms.get("CHAINED").map(|arm| &arm.verdict) {
            Some(ServerboundDecodeVerdict::Unclassified { depth_limited, .. }) => {
                assert!(*depth_limited, "expected the depth cap to be the reason");
            }
            other => panic!("expected a depth-limited unclassified verdict, got {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn serverbound_decode_axis_detects_a_planted_stranded_variant() -> Result<()> {
        let workspace = serverbound_decode_fixture_workspace()?;
        let report = connectedness_report(&workspace)?;
        let family = report
            .families
            .iter()
            .find(|f| f.family == "v999")
            .expect("v999 fixture family exists");
        let ServerboundDecodeAxis::Measured(summary) = &family.serverbound_decode else {
            panic!(
                "expected a measured serverbound-decode axis, got {:?}",
                family.serverbound_decode
            );
        };
        assert_eq!(summary.total, 5);
        assert_eq!(summary.examined_arms, 4);
        assert_eq!(summary.decoded, 3);
        // The control: MYSTERY_ACTION decodes to a real ServerBound variant
        // whose only dispatch arm in server.rs is the empty `=> {}` group it
        // shares with `Ignored` — a planted island. If this assertion ever
        // passes with `connected == 2` (i.e. the planted island stops being
        // reported as stranded), the detector has gone blind.
        assert_eq!(summary.connected, 1);
        assert_eq!(summary.stranded_names, vec!["MYSTERY_ACTION".to_owned()]);
        assert_eq!(summary.always_ignored_names, vec!["PING".to_owned()]);
        assert_eq!(summary.unclassified.len(), 1);
        assert_eq!(summary.unclassified[0].packet, "WEIRD");
        assert!(report.has_unclassified());
        assert_eq!(report.unclassified_count(), 1);
        assert!(report.render().contains("serverbound decoded 3/5, connected 1/5"));
        assert!(report.render().contains("decode-but-stranded 1 [MYSTERY_ACTION]"));
        Ok(())
    }

    #[test]
    fn conformance_skip_cargo_checks_packet_ids_and_skips_absent_registry_report() -> Result<()> {
        let workspace = isolation_fixture(
            "conformance",
            &[("crates/versions/v999", "lodestone-v999", "")],
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
        let generated_dir = workspace.join("crates/versions/v999/src/generated");
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

    /// Reproduces the stale-location bug directly: `conformance`'s registry
    /// step used to point at `crates/versions/<family>/src/generated`, but
    /// the four registries it drift-checks (`sound_events`, `particle_types`,
    /// `menus`, `items`) have lived in `crates/lodestone-data/src/generated`
    /// since the `lodestone-data` extraction. Legacy families skip this step
    /// entirely (no `registries.json`), so the stale path was unreachable
    /// for three of four families and, before `check-connected` was fixed,
    /// unreachable for the fourth too -- two independent guards masking one
    /// bug. This plants a `registries.json` (making the family the one path
    /// that reaches the step) and pre-generates the committed tables at the
    /// *correct*, family-independent location, then asserts the step passes.
    /// Before the fix this failed with "No such file or directory" against
    /// `crates/versions/v999/src/generated/sound_events.rs`, which never
    /// existed.
    #[test]
    fn conformance_registry_check_reads_lodestone_data_not_the_family_generated_dir() -> Result<()>
    {
        let workspace = isolation_fixture(
            "conformance-registry-redirect",
            &[("crates/versions/v999", "lodestone-v999", "")],
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
        // Pairwise-distinct entries per registry so a transposition between
        // registries (all four go through the same generator) cannot survive
        // unnoticed.
        std::fs::write(
            cache_dir.join("registries.json"),
            r#"{
                "minecraft:sound_event": {"entries": {"minecraft:test_sound": {"protocol_id": 0}}},
                "minecraft:particle_type": {"entries": {"minecraft:test_particle": {"protocol_id": 0}}},
                "minecraft:menu": {"entries": {"minecraft:test_menu": {"protocol_id": 0}}},
                "minecraft:item": {"entries": {"minecraft:test_item": {"protocol_id": 0}}}
            }"#,
        )?;
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
        let family_generated_dir = workspace.join("crates/versions/v999/src/generated");
        std::fs::create_dir_all(&family_generated_dir)?;
        std::fs::write(
            family_generated_dir.join("packet_ids.rs"),
            generate_packet_ids_source(&report)?,
        )?;

        // Pre-generate the committed registry tables at the real location --
        // crates/lodestone-data/src/generated, not the family's own
        // generated/ -- exactly as they are actually committed in this repo.
        let registry_options = GenRegistriesOptions {
            minecraft_version: "test".to_owned(),
            protocol_version: 999,
            check: false,
            out_dir: PathBuf::from(DEFAULT_REGISTRY_OUT_DIR),
            registries: default_registry_specs()
                .iter()
                .map(|spec| spec.registry_key.to_owned())
                .collect(),
        };
        let written = generate_registries(&workspace, &registry_options)?;
        assert_eq!(written.len(), 4);
        // Positive control: the family's own (stale) location must stay
        // empty, or this test would not distinguish the fix from the bug.
        assert!(!family_generated_dir.join("sound_events.rs").exists());

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
        let registry_step = conformance
            .steps
            .iter()
            .find(|step| step.name == "gen-registries --check")
            .expect("conformance always reports a gen-registries --check step");
        assert_eq!(registry_step.outcome, ConformanceOutcome::Passed);

        // Negative control: corrupt the committed table at the real location
        // and confirm conformance's registry step actually reads it (rather
        // than, say, vacuously passing because the file it checks does not
        // exist and some earlier bug swallowed the read error).
        let items_path = workspace.join(DEFAULT_REGISTRY_OUT_DIR).join("items.rs");
        let pristine = std::fs::read_to_string(&items_path)?;
        std::fs::write(&items_path, pristine.replace("minecraft:test_item", "minecraft:corrupted"))?;
        let error = run_conformance(
            &workspace,
            &ConformanceOptions {
                family: "v999".to_owned(),
                minecraft_version: "test".to_owned(),
                protocol_version: 999,
                source: PacketSource::Mojang,
                skip_cargo: true,
            },
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("items.rs is out of date"), "{error}");
        assert!(
            error.contains(
                workspace
                    .join(DEFAULT_REGISTRY_OUT_DIR)
                    .join("items.rs")
                    .to_str()
                    .expect("workspace path is valid UTF-8")
            ),
            "expected the error to name the lodestone-data path, got: {error}"
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
        let out_dir = workspace.join("crates/versions/26.2/src/generated");
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
            out_dir: PathBuf::from("crates/versions/26.2/src/generated"),
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

    /// A hand-written `sounds.json` exercising every entry shape vanilla uses,
    /// with an index that declares one size per sample. The expected partition is
    /// stated by hand from the fixture, not read back out of the planner.
    const SOUNDS_FIXTURE: &[u8] = br#"{
        "block.stone.break":      { "sounds": ["dig/stone1", "dig/stone2"] },
        "entity.zombie.hurt":     { "sounds": [{ "name": "mob/zombie/hurt1" }] },
        "entity.player.hurt":     { "sounds": [{ "type": "event", "name": "entity.generic.hurt" }] },
        "ambient.cave":           { "sounds": [{ "name": "ambient/cave/cave1", "stream": true }] },
        "music.creative":         { "sounds": [{ "name": "music/game/creative/creative1", "stream": true }] },
        "music_disc.cat":         { "sounds": [{ "name": "records/cat", "stream": true }] },
        "jukebox.play":           { "sounds": ["records/cat"] }
    }"#;

    fn sounds_fixture_index() -> serde_json::Map<String, Value> {
        let mut index = serde_json::Map::new();
        // Sizes are arbitrary but distinct, so a byte total cannot come out right
        // by coincidence.
        for (name, size) in [
            ("minecraft/sounds/dig/stone1.ogg", 100),
            ("minecraft/sounds/dig/stone2.ogg", 200),
            ("minecraft/sounds/mob/zombie/hurt1.ogg", 400),
            ("minecraft/sounds/ambient/cave/cave1.ogg", 800),
            ("minecraft/sounds/music/game/creative/creative1.ogg", 1600),
            ("minecraft/sounds/records/cat.ogg", 3200),
            // An index-only sample no event names.
            ("minecraft/sounds/orphan.ogg", 6400),
            // A non-ogg object, which must not land in `unreferenced`.
            ("minecraft/sounds.json", 12800),
        ] {
            index.insert(
                name.to_string(),
                serde_json::json!({ "hash": "abcdef0123456789abcdef0123456789abcdef01", "size": size }),
            );
        }
        index
    }

    #[test]
    fn the_sound_corpus_excludes_music_only_samples_and_keeps_everything_else() -> Result<()> {
        let index = sounds_fixture_index();
        let corpus = plan_sound_corpus(&index, SOUNDS_FIXTURE, false)?;

        assert_eq!(corpus.events, 7, "one entry per top-level sounds.json key");
        let wanted: Vec<&str> = corpus.wanted.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            wanted,
            vec![
                // The cave loop is `stream: true` and still fetched: the policy is
                // "music events", not vanilla's streaming flag, precisely so
                // ambience survives.
                "minecraft/sounds/ambient/cave/cave1.ogg",
                "minecraft/sounds/dig/stone1.ogg",
                "minecraft/sounds/dig/stone2.ogg",
                "minecraft/sounds/mob/zombie/hurt1.ogg",
                // `records/cat` is referenced by `music_disc.cat` *and* by
                // `jukebox.play`, so "every referencing event is music" is false
                // and it must be fetched. This is the case an "any music event"
                // rule would silently drop.
                "minecraft/sounds/records/cat.ogg",
            ],
            "the wanted set is wrong"
        );
        assert_eq!(corpus.wanted_bytes(), 100 + 200 + 400 + 800 + 3200);

        assert_eq!(
            corpus
                .excluded
                .iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>(),
            vec!["minecraft/sounds/music/game/creative/creative1.ogg"],
            "only the sample referenced exclusively by a music event is excluded"
        );
        assert_eq!(corpus.excluded_bytes(), 1600);

        // The `type: event` indirection contributes no file of its own, so
        // `entity.generic.hurt` (which this fixture does not even define) must not
        // appear as a sample.
        assert!(
            !wanted
                .iter()
                .any(|name| name.contains("generic") || name.contains("entity.")),
            "a type:event entry must not be resolved as a file: {wanted:?}"
        );

        // Only `.ogg` objects count as unreferenced; `sounds.json` itself must not.
        assert_eq!(
            corpus
                .unreferenced
                .iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>(),
            vec!["minecraft/sounds/orphan.ogg"]
        );
        Ok(())
    }

    #[test]
    fn all_folds_music_back_in_and_leaves_nothing_excluded() -> Result<()> {
        let index = sounds_fixture_index();
        let default = plan_sound_corpus(&index, SOUNDS_FIXTURE, false)?;
        let all = plan_sound_corpus(&index, SOUNDS_FIXTURE, true)?;

        assert!(all.excluded.is_empty(), "--all excludes nothing");
        assert_eq!(all.wanted.len(), default.wanted.len() + 1);
        assert_eq!(
            all.wanted_bytes(),
            default.wanted_bytes() + default.excluded_bytes(),
            "the two modes must partition the same total"
        );
        // The orphan is fetched by neither mode: no event can select it.
        assert!(
            !all.wanted
                .iter()
                .any(|(name, _)| name == "minecraft/sounds/orphan.ogg")
        );
        Ok(())
    }

    #[test]
    fn a_sound_name_the_index_does_not_declare_is_an_error_not_a_skip() {
        // The resolution rule being wrong for a version must fail loudly: silently
        // dropping the name would fetch a short corpus and read as success.
        let mut index = sounds_fixture_index();
        index.remove("minecraft/sounds/mob/zombie/hurt1.ogg");
        let error = plan_sound_corpus(&index, SOUNDS_FIXTURE, false)
            .expect_err("a name missing from the index must fail");
        let error = error.to_string();
        assert!(error.contains("mob/zombie/hurt1.ogg"), "{error}");
        assert!(error.contains("resolution rule"), "{error}");

        // Control: with the entry restored the same input plans cleanly, so the
        // failure above was the missing declaration and not the fixture.
        assert!(plan_sound_corpus(&sounds_fixture_index(), SOUNDS_FIXTURE, false).is_ok());
    }

    #[test]
    fn a_namespaced_sound_name_resolves_under_its_own_namespace() {
        assert_eq!(
            sound_object_name("mob/zombie/hurt1"),
            "minecraft/sounds/mob/zombie/hurt1.ogg"
        );
        assert_eq!(
            sound_object_name("somepack:foo/bar"),
            "somepack/sounds/foo/bar.ogg"
        );
    }

    #[test]
    fn music_event_keys_are_recognised_and_world_events_are_not() {
        assert!(is_music_event("music.creative"));
        assert!(is_music_event("music_disc.cat"));
        assert!(is_music_event("music"));
        // The near-misses that a substring test would get wrong.
        assert!(!is_music_event("ambient.cave"));
        assert!(!is_music_event("block.note_block.harp"));
        assert!(!is_music_event("item.goat_horn.sound.0"));
        assert!(!is_music_event("musical"));
    }

    #[test]
    fn fetch_sounds_args_parse_and_reject_nonsense() -> Result<()> {
        assert_eq!(
            parse_cli_args(["fetch-sounds", "--version", "26.2"])?,
            CliCommand::FetchSounds {
                minecraft_version: "26.2".to_string(),
                all: false,
                force: false,
                jobs: None,
            }
        );
        assert_eq!(
            parse_cli_args([
                "fetch-sounds",
                "--version",
                "26.2",
                "--all",
                "--force",
                "--jobs",
                "4",
            ])?,
            CliCommand::FetchSounds {
                minecraft_version: "26.2".to_string(),
                all: true,
                force: true,
                jobs: Some(4),
            }
        );
        assert!(parse_cli_args(["fetch-sounds"]).is_err(), "--version is required");
        assert!(parse_cli_args(["fetch-sounds", "--version", "26.2", "--jobs", "0"]).is_err());
        assert!(parse_cli_args(["fetch-sounds", "--version", "26.2", "--jobs", "x"]).is_err());
        assert!(parse_cli_args(["fetch-sounds", "--nope"]).is_err());
        assert!(root_help().contains("fetch-sounds"));
        Ok(())
    }

    /// The real 26.2 corpus plan, against numbers derived by an **independent**
    /// walk of the same two files (a throwaway Python pass over
    /// `asset-index-32.json` and `sounds.json`) rather than by running this code
    /// and writing down what it said.
    ///
    /// `#[ignore]`d because it needs a populated `.cache/mc/26.2`; an opted-in run
    /// with no cache is a failure with a named fix, never a silent pass.
    #[test]
    #[ignore = "requires .cache/mc/26.2 (cargo run -p xtask -- fetch-assets --version 26.2)"]
    fn the_real_26_2_corpus_matches_an_independently_derived_partition() -> Result<()> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let cache = root.join(".cache/mc/26.2");
        let index_path = find_cached_asset_index(&cache)?;
        let index_json: Value = serde_json::from_slice(&std::fs::read(&index_path)?)?;
        let index = index_json
            .get("objects")
            .and_then(|o| o.as_object())
            .ok_or_else(|| anyhow!("no objects map"))?;
        let hash = index["minecraft/sounds.json"]["hash"]
            .as_str()
            .ok_or_else(|| anyhow!("no hash"))?;
        let sounds =
            std::fs::read(cache.join("objects").join(&hash[0..2]).join(hash)).with_context(|| {
                "minecraft/sounds.json is not in the store; run: cargo run -p xtask -- \
                 fetch-assets --version 26.2"
            })?;

        let corpus = plan_sound_corpus(index, &sounds, false)?;
        assert_eq!(corpus.events, 1968);
        assert_eq!(corpus.wanted.len(), 4751);
        assert_eq!(corpus.wanted_bytes(), 80_139_855);
        assert_eq!(corpus.excluded.len(), 92);
        assert_eq!(corpus.excluded_bytes(), 293_228_876);
        assert_eq!(corpus.unreferenced.len(), 28);
        // Every excluded object is under music/ or records/ — the derivation is by
        // *event key*, so agreement with the path layout is a real cross-check
        // rather than a restatement.
        for (name, _) in &corpus.excluded {
            assert!(
                name.starts_with("minecraft/sounds/music/")
                    || name.starts_with("minecraft/sounds/records/"),
                "excluded by event key but not a music/records path: {name}"
            );
        }
        // And the six streamed ambience loops are on the *fetched* side, which is
        // the whole reason the policy is not vanilla's `stream: true` flag.
        assert!(
            corpus
                .wanted
                .iter()
                .any(|(name, _)| name == "minecraft/sounds/ambient/underwater/underwater_ambience.ogg")
        );

        let all = plan_sound_corpus(index, &sounds, true)?;
        assert_eq!(all.wanted.len(), 4843);
        assert_eq!(all.wanted_bytes(), 373_368_731);
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
                    "crates/versions/v1",
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
                ("crates/versions/v1", "lodestone-v1", ""),
                (
                    "crates/versions/v2",
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
                ("crates/versions/v1", "lodestone-v1", ""),
                (
                    "crates/lodestone-client",
                    "lodestone-client",
                    r#"
[dependencies]
lodestone-v1 = { path = "../versions/v1" }
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
                ("crates/versions/v1", "lodestone-v1", ""),
                (
                    "crates/lodestone-client",
                    "lodestone-client",
                    r#"
[dependencies]
lodestone-v1 = { path = "../versions/v1", optional = true }

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
                ("crates/versions/v1", "lodestone-v1", ""),
                (
                    "crates/lodestone-client",
                    "lodestone-client",
                    r#"
[dev-dependencies]
lodestone-v1 = { path = "../versions/v1" }
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
                "crates/versions/v1",
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
        let family = workspace.join("crates/versions/v999");
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
            std::fs::read_to_string(workspace.join("crates/versions/v2/SHAPE_REVIEW.toml"))?;
        assert!(review.contains("name = \"minecraft:map_chunk\""));
        assert!(review.contains("reviewed = false"));
        assert!(
            workspace
                .join("crates/versions/v2/tests/shape_review.rs")
                .exists()
        );
        assert!(
            !workspace
                .join("crates/versions/v2/tests/live_chunk.rs")
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
            workspace.join("crates/versions/v1/src/generated/packet_ids.rs"),
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
        let v1_src = workspace.join("crates/versions/v1/src");
        let v2_src = workspace.join("crates/versions/v2/src");
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
                ("crates/versions/v1", "lodestone-v1", ""),
                (
                    "crates/lodestone-registry",
                    "lodestone-registry",
                    r#"
[package.metadata.lodestone-isolation]
role = "version-registry"

[dependencies]
lodestone-v1 = { path = "../versions/v1", optional = true }

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
                ("crates/versions/v2", "lodestone-v2", ""),
                (
                    "crates/lodestone-registry",
                    "lodestone-registry",
                    r#"
[package.metadata.lodestone-isolation]
role = "version-registry"

[dependencies]
lodestone-v2 = { path = "../versions/v2", optional = true }
"#,
                ),
            ],
        )?;
        std::fs::write(
            workspace.join("crates/versions/v2/SHAPE_REVIEW.toml"),
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
lodestone-v26-2 = { path = "../versions/26.2", optional = true }
"#,
                ),
                ("crates/versions/26.2", "lodestone-v26-2", false, ""),
            ],
            "",
        )?;

        let report = check_workspace_connected(&workspace)?;
        assert!(
            !report
                .violations()
                .any(|finding| finding.crate_name == "lodestone-v26-2"),
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

    /// `conformance --family v340` used to run `check-connected` workspace-
    /// wide and unconditionally: an orphan crate belonging to an unrelated
    /// family (or anything else in the workspace) failed v340's conformance
    /// run even though v340 itself was perfectly fine -- a per-family tool
    /// held hostage to state outside its own subject, the mirror image of
    /// the docs-index gate that scanned three directories and not a fourth.
    ///
    /// Builds two protocol families, only one wired into the shipped root:
    /// `lodestone-v999` is reachable, `lodestone-v888` is a genuine orphan.
    /// Two things must both be true, or this is a skip path rather than a
    /// scope: family-scoped v999 must NOT see v888's violation (that is the
    /// fix), and family-scoped v888 must still see its OWN violation (so a
    /// subject that exists cannot come back "no findings" -- an errored or
    /// vacuous detector is the failure mode this whole audit exists to catch).
    #[test]
    fn check_connected_for_family_scopes_violations_to_the_named_family() -> Result<()> {
        let workspace = connected_fixture(
            "connected-family-scope",
            &[(
                "apps/lodestone",
                "lodestone-shell",
                true,
                r#"
[dependencies]
lodestone-v999 = { path = "../../crates/versions/v999" }
"#,
            )],
            &[
                ("crates/versions/v999", "lodestone-v999", false, ""),
                ("crates/versions/v888", "lodestone-v888", false, ""),
            ],
            "",
        )?;

        // Sanity precondition: the global, unscoped check must actually see
        // both crates' status, or the scoped assertions below prove nothing.
        let global = check_workspace_connected(&workspace)?;
        assert!(
            global
                .violations()
                .any(|finding| finding.crate_name == "lodestone-v888"),
            "{}",
            global.violation_summary()
        );
        assert!(
            !global
                .violations()
                .any(|finding| finding.crate_name == "lodestone-v999"),
            "{}",
            global.violation_summary()
        );

        // The fix: v999's own conformance run must not see v888's orphan.
        let scoped_to_v999 = check_workspace_connected_for_family(&workspace, "v999")?;
        assert!(
            !scoped_to_v999.has_violations(),
            "v999 must not be held hostage by v888's unrelated violation: {}",
            scoped_to_v999.violation_summary()
        );

        // Not a skip path: v888's own conformance run must still catch its
        // own real violation.
        let scoped_to_v888 = check_workspace_connected_for_family(&workspace, "v888")?;
        assert!(scoped_to_v888.has_violations());
        assert!(
            scoped_to_v888
                .violations()
                .any(|finding| finding.crate_name == "lodestone-v888"),
            "{}",
            scoped_to_v888.violation_summary()
        );
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
                ("crates/versions/v1", "lodestone-v1", ""),
                (
                    "crates/lodestone-registry",
                    "lodestone-registry",
                    r#"
[package.metadata.lodestone-isolation]
role = "version-registry"

[dependencies]
lodestone-v1 = { path = "../versions/v1" }
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
                ("crates/versions/v1", "lodestone-v1", ""),
                (
                    "crates/lodestone-registry",
                    "lodestone-registry",
                    r#"
[package.metadata.lodestone-isolation]
role = "version-registry"

[dependencies]
lodestone-v1 = { path = "../versions/v1", optional = true }

[features]
v1 = ["dep:lodestone-v1"]
"#,
                ),
                (
                    "crates/lodestone-client",
                    "lodestone-client",
                    r#"
[dependencies]
lodestone-v1 = { path = "../versions/v1", optional = true }

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
                ("crates/versions/v1", "lodestone-v1", ""),
                (
                    "crates/lodestone-client",
                    "lodestone-client",
                    r#"
[dependencies]
lodestone-v1 = { path = "../versions/v1", optional = true }

[features]
live-v1 = ["dep:lodestone-v1"]
"#,
                ),
            ],
        )?;

        let report = check_workspace_deletable(&workspace, "v1")?;
        assert_eq!(report.target_crate, "lodestone-v1");
        assert_eq!(report.target_dir, "crates/versions/v1");
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
                .all(|line| !line.path.starts_with("crates/versions/v1"))
        );
        Ok(())
    }

    #[test]
    fn check_deletable_flags_required_dependent_as_blocker() -> Result<()> {
        let workspace = isolation_fixture(
            "deletable-required-dependent",
            &[
                ("crates/versions/v1", "lodestone-v1", ""),
                (
                    "crates/lodestone-client",
                    "lodestone-client",
                    r#"
[dependencies]
lodestone-v1 = { path = "../versions/v1" }
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
                ("crates/versions/v1", "lodestone-v1", ""),
                (
                    "crates/versions/v2",
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
            &[("crates/versions/v1", "lodestone-v1", "")],
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
                ("crates/versions/v1", "lodestone-v1", ""),
                (
                    "crates/lodestone-registry",
                    "lodestone-registry",
                    r#"
[dependencies]
lodestone-v1 = { path = "../versions/v1", optional = true }

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
                ("crates/versions/v1", "lodestone-v1", ""),
                (
                    "crates/lodestone-registry",
                    "lodestone-registry",
                    "[package.metadata.lodestone-isolation]\nrole = \"version-registry\"\n\n[dependencies]\nlodestone-v1 = { path = \"../versions/v1\", optional = true }\n\n[features]\nv1 = [\"dep:lodestone-v1\"]\n",
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

    fn connectedness_fixture_workspace() -> Result<TestWorkspace> {
        let workspace = fresh_test_workspace("connectedness")?;
        let root = workspace.deref();
        std::fs::create_dir_all(root.join("crates/versions/v9/src/generated"))?;
        std::fs::write(root.join("crates/versions/v9/src/adapter.rs"), "")?;
        std::fs::write(
            root.join("crates/versions/v9/src/generated/packet_ids.rs"),
            "pub mod play { pub mod clientbound { pub const IGNORED: i32 = 0; pub static ENTRIES: &[(&str, i32)] = &[(\"minecraft:ignored\", IGNORED)]; } pub mod serverbound { pub const IGNORED: i32 = 0; pub static ENTRIES: &[(&str, i32)] = &[(\"minecraft:ignored\", IGNORED)]; } }",
        )?;
        let family = root.join("crates/versions/26.2");
        std::fs::create_dir_all(family.join("src/generated"))?;
        std::fs::write(
            family.join("src/generated/packet_ids.rs"),
            r#"
pub mod play {
    pub mod clientbound {
        pub const SYSTEM_CHAT: i32 = 0;
        pub const ADD_ENTITY: i32 = 1;
        pub const BLOCK_UPDATE: i32 = 2;
        pub const SET_OBJECTIVE: i32 = 3;
        pub const MYSTERY: i32 = 4;
        pub static ENTRIES: &[(&str, i32)] = &[
            ("minecraft:system_chat", SYSTEM_CHAT),
            ("minecraft:add_entity", ADD_ENTITY),
            ("minecraft:block_update", BLOCK_UPDATE),
            ("minecraft:set_objective", SET_OBJECTIVE),
            ("minecraft:mystery", MYSTERY),
        ];
    }
    pub mod serverbound {
        pub const CHAT: i32 = 0;
        pub const MOVE: i32 = 1;
        pub static ENTRIES: &[(&str, i32)] = &[
            ("minecraft:chat", CHAT),
            ("minecraft:move", MOVE),
        ];
    }
}
"#,
        )?;
        std::fs::write(
            family.join("src/adapter.rs"),
            r#"
fn handle_add_entity(payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
    Ok(vec![Directive::Emit(ClientEvent::EntitySpawned { id: 1 })])
}

fn encode_action(action: ClientAction) -> Result<Option<(i32, Vec<u8>)>, AdapterError> {
    Ok(Some((play::serverbound::CHAT, Vec::new())))
}

fn handle_play(
    &self,
    world: &mut dyn WorldSink,
    packet_id: i32,
    payload: &[u8],
) -> Result<Vec<Directive>, AdapterError> {
    if packet_id == play::clientbound::SYSTEM_CHAT {
        return Ok(vec![Directive::Emit(ClientEvent::Chat { text })]);
    }
    if packet_id == play::clientbound::ADD_ENTITY {
        return handle_add_entity(payload);
    }
    if packet_id == play::clientbound::BLOCK_UPDATE {
        world.set_block(pos, state);
        return Ok(Vec::new());
    }
    if packet_id == play::clientbound::SET_OBJECTIVE {
        decode_and_validate::<SetObjective>(payload)?;
        return Ok(Vec::new());
    }
    if packet_id == play::clientbound::MYSTERY {
        return parse_mystery(payload);
    }
    Ok(Vec::new())
}
"#,
        )?;
        Ok(workspace)
    }

    /// A family with a `src/server_protocol.rs` (so `ServerboundDecodeAxis`
    /// is `Measured` rather than `NotApplicable`) plus a minimal
    /// `crates/lodestone-server/src/server.rs` for the second-hop join.
    ///
    /// Deliberately plants one **known** island — `MYSTERY_ACTION` decodes
    /// to a real `ServerBound::MysteryAction` variant, but the only arm
    /// handling that variant in `server.rs` is the empty `=> {}` group it
    /// shares with `Ignored`. This is the control the job's own writeup
    /// demands: a coverage tool that cannot detect a planted island is
    /// worthless, so [`serverbound_decode_axis_detects_a_planted_stranded_variant`]
    /// asserts the exact reported numbers, not just "some islands exist."
    ///
    /// Also plants the naive-scanner failure mode `match_arm_body` exists to
    /// fix: `PING`'s arm is a bare, unbraced expression immediately
    /// followed by `MYSTERY_ACTION`'s braced arm. A `find('{')`-based
    /// scanner would swallow `MYSTERY_ACTION`'s whole body as if it were
    /// `PING`'s.
    fn serverbound_decode_fixture_workspace() -> Result<TestWorkspace> {
        let workspace = fresh_test_workspace("serverbound-decode")?;
        let root = workspace.deref();
        let family = root.join("crates/versions/v999");
        std::fs::create_dir_all(family.join("src/generated"))?;
        std::fs::write(
            family.join("src/generated/packet_ids.rs"),
            r#"
pub mod play {
    pub mod clientbound {
        pub const NOOP: i32 = 0;
        pub static ENTRIES: &[(&str, i32)] = &[("minecraft:noop", NOOP)];
    }
    pub mod serverbound {
        pub const KEEP_ALIVE: i32 = 0;
        pub const PING: i32 = 1;
        pub const MYSTERY_ACTION: i32 = 2;
        pub const WEIRD: i32 = 3;
        pub const UNHANDLED: i32 = 4;
        pub static ENTRIES: &[(&str, i32)] = &[
            ("minecraft:keep_alive", KEEP_ALIVE),
            ("minecraft:ping", PING),
            ("minecraft:mystery_action", MYSTERY_ACTION),
            ("minecraft:weird", WEIRD),
            ("minecraft:unhandled", UNHANDLED),
        ];
    }
}
"#,
        )?;
        std::fs::write(family.join("src/adapter.rs"), "")?;
        std::fs::write(
            family.join("src/server_protocol.rs"),
            r#"
impl ServerProtocol for V999ServerProtocol {
    fn decode(&self, state: lodestone_core::State, packet_id: i32, payload: &[u8]) -> ServerBound {
        match state {
            State::Play if packet_id == play::serverbound::KEEP_ALIVE => {
                match decode_full::<KeepAlive>(payload) {
                    Some(k) => ServerBound::KeepAlive { id: k.id },
                    None => ServerBound::Ignored,
                }
            }
            State::Play if packet_id == play::serverbound::PING => ServerBound::Ignored,
            State::Play if packet_id == play::serverbound::MYSTERY_ACTION => {
                decode_mystery_action(payload)
            }
            State::Play if packet_id == play::serverbound::WEIRD => {
                external_helper(payload)
            }
            _ => ServerBound::Ignored,
        }
    }
}

fn decode_mystery_action(payload: &[u8]) -> ServerBound {
    match decode_full::<MysteryAction>(payload) {
        Some(m) => ServerBound::MysteryAction { id: m.id },
        None => ServerBound::Ignored,
    }
}
"#,
        )?;

        let server = root.join("crates/lodestone-server/src");
        std::fs::create_dir_all(&server)?;
        std::fs::write(
            server.join("server.rs"),
            r#"
/// Dispatches a decoded [`ServerBound::KeepAlive`] request — this doc
/// comment itself mentions the variant so a non-comment-aware scanner would
/// find this line first and get confused about where the real arm is.
fn dispatch(x: ServerBound) {
    match x {
        ServerBound::KeepAlive { id } => {
            respond(id);
        }
        ServerBound::MysteryAction { .. } | ServerBound::Ignored => {}
    }
}
"#,
        )?;

        Ok(workspace)
    }

    fn new_version_fixture_workspace() -> Result<TestWorkspace> {
        let workspace = fresh_test_workspace("new-version-shape-review")?;
        let root = workspace.deref();
        std::fs::write(
            root.join("Cargo.toml"),
            r#"[workspace]
resolver = "3"
members = ["crates/versions/*", "crates/lodestone-registry"]

[workspace.package]
version = "0.1.0"
edition = "2024"
license = "MIT OR Apache-2.0"

[workspace.dependencies]
lodestone-v1 = { path = "crates/versions/v1" }
"#,
        )?;

        let v1 = root.join("crates/versions/v1");
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

    #[test]
    fn mojang_sourced_entries_are_their_own_canonical_name() -> Result<()> {
        // Mojang's own report names already are canonical: every entry from
        // this source should self-alias, with no lookup involved.
        let packet_report_json = r#"{
            "configuration": {"clientbound": {}, "serverbound": {}},
            "handshake": {"serverbound": {"minecraft:intention": {"protocol_id": 0}}},
            "login": {"clientbound": {}, "serverbound": {}},
            "play": {
                "clientbound": {"minecraft:set_health": {"protocol_id": 5}},
                "serverbound": {}
            },
            "status": {"clientbound": {}, "serverbound": {}}
        }"#;
        let report = parse_packet_report(packet_report_json, "test", 999)?;
        for entry in report.all_entries() {
            assert_eq!(entry.canonical_name.as_deref(), Some(entry.name.as_str()));
        }
        Ok(())
    }

    #[test]
    fn minecraft_data_sourced_entries_default_to_no_canonical_name() -> Result<()> {
        // MINECRAFT_DATA_CANONICAL_ALIASES is empty today (no fabricated
        // guesses), so a minecraft-data-sourced entry must come back with
        // canonical_name: None rather than inventing a mapping.
        let json = minecraft_data_protocol_fixture("count");
        let report = parse_minecraft_data_report(&json, "1.8.8", 47)?;
        let entry = report
            .entries(PacketState::Play, PacketBound::Clientbound)
            .find(|entry| entry.name == "minecraft:map_chunk")
            .expect("fixture declares minecraft:map_chunk");
        assert_eq!(entry.canonical_name, None);
        Ok(())
    }

    #[test]
    fn resolve_canonical_alias_matches_by_exact_name_only() {
        // Pairwise-distinct entries so a transposition between the "from"
        // and "to" columns, or between two table rows, cannot survive
        // unnoticed.
        let table: &[(&str, &str)] = &[
            ("minecraft:named_entity_spawn", "minecraft:add_entity"),
            ("minecraft:update_health", "minecraft:set_health"),
        ];
        assert_eq!(
            resolve_canonical_alias(table, "minecraft:named_entity_spawn"),
            Some("minecraft:add_entity")
        );
        assert_eq!(
            resolve_canonical_alias(table, "minecraft:update_health"),
            Some("minecraft:set_health")
        );
        assert_eq!(resolve_canonical_alias(table, "minecraft:unmapped"), None);
    }

    #[test]
    fn generate_packet_ids_source_emits_canonical_names_table() -> Result<()> {
        let packet_report_json = r#"{
            "configuration": {"clientbound": {}, "serverbound": {}},
            "handshake": {"serverbound": {"minecraft:intention": {"protocol_id": 0}}},
            "login": {"clientbound": {}, "serverbound": {}},
            "play": {
                "clientbound": {"minecraft:set_health": {"protocol_id": 5}},
                "serverbound": {}
            },
            "status": {"clientbound": {}, "serverbound": {}}
        }"#;
        let report = parse_packet_report(packet_report_json, "test", 999)?;
        let generated = generate_packet_ids_source(&report)?;
        assert!(generated.contains("pub static CANONICAL_NAMES"));
        assert!(generated.contains(r#"("minecraft:set_health", "minecraft:set_health")"#));
        assert!(generated.contains(r#"("minecraft:intention", "minecraft:intention")"#));
        Ok(())
    }
}
