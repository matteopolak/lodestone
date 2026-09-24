use super::*;

pub(crate) fn load_packet_report(
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
pub(crate) struct MinecraftDataVersionInfo {
    minecraft_version: String,
    protocol_version: i32,
    major_version: String,
}

pub(crate) fn load_minecraft_data_protocol_json(
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

pub(crate) fn minecraft_data_fallback_protocol_dir(
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
pub(crate) fn minecraft_data_version_info(
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
pub(crate) const fn default_out_for_source(source: PacketSource) -> &'static str {
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
