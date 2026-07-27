//! Parsing of the `pack.mcmeta` pack-metadata file ([`PackMeta`]).

use crate::error::AssetError;
use serde_json::Value;

/// A pack's `description`, which may be a plain string or a JSON text component.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackDescription {
    /// A plain string description.
    Text(String),
    /// A raw JSON text component (object or array of components).
    Component(Value),
}

impl PackDescription {
    /// Extracts a best-effort plain-text rendering of the description.
    ///
    /// For [`PackDescription::Text`] this is the string verbatim. For a text
    /// component it concatenates the `text` fields of the component and any
    /// `extra`/array children.
    pub fn plain_text(&self) -> String {
        match self {
            PackDescription::Text(s) => s.clone(),
            PackDescription::Component(v) => {
                let mut out = String::new();
                collect_component_text(v, &mut out);
                out
            }
        }
    }
}

fn collect_component_text(value: &Value, out: &mut String) {
    match value {
        Value::String(s) => out.push_str(s),
        Value::Array(items) => {
            for item in items {
                collect_component_text(item, out);
            }
        }
        Value::Object(map) => {
            if let Some(Value::String(text)) = map.get("text") {
                out.push_str(text);
            }
            if let Some(extra) = map.get("extra") {
                collect_component_text(extra, out);
            }
        }
        _ => {}
    }
}

/// Parsed contents of a `pack.mcmeta` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackMeta {
    /// The declared `pack_format`.
    pub pack_format: u32,
    /// The pack description.
    pub description: PackDescription,
    /// The inclusive `(min, max)` range from `supported_formats`, if present.
    pub supported_formats: Option<(u32, u32)>,
    /// The major/minor resource pack version, when derived from `version.json`.
    ///
    /// This is `None` for a `pack.mcmeta`, which only carries a flat
    /// `pack_format`.
    pub pack_version: Option<PackVersion>,
}

impl PackMeta {
    /// Parses `pack.mcmeta` bytes.
    ///
    /// Returns [`AssetError::MetaMalformed`] on invalid JSON or a missing/invalid
    /// `pack` object or `pack_format`.
    pub fn parse(bytes: &[u8]) -> Result<Self, AssetError> {
        let root: Value =
            serde_json::from_slice(bytes).map_err(|e| AssetError::MetaMalformed(e.to_string()))?;
        let pack = root
            .get("pack")
            .and_then(Value::as_object)
            .ok_or_else(|| AssetError::MetaMalformed("missing \"pack\" object".to_string()))?;

        let pack_format = pack
            .get("pack_format")
            .and_then(Value::as_u64)
            .ok_or_else(|| AssetError::MetaMalformed("missing \"pack_format\"".to_string()))?
            as u32;

        let description = match pack.get("description") {
            Some(Value::String(s)) => PackDescription::Text(s.clone()),
            Some(other) => PackDescription::Component(other.clone()),
            None => PackDescription::Text(String::new()),
        };

        let supported_formats = pack
            .get("supported_formats")
            .map(parse_supported_formats)
            .transpose()?;

        Ok(Self {
            pack_format,
            description,
            supported_formats,
            pack_version: None,
        })
    }

    /// Builds pack metadata from a vanilla `version.json`.
    ///
    /// Vanilla's `client.jar` has no root `pack.mcmeta`; the built-in pack's
    /// metadata is derived from `version.json` instead. The `pack_format` is
    /// taken from the resource pack version's major number and the version id
    /// becomes the description.
    pub fn from_version_json(bytes: &[u8]) -> Result<Self, AssetError> {
        let version = VersionMeta::parse(bytes)?;
        Ok(Self::from(version))
    }

    /// Whether this pack declares compatibility with a host running the given
    /// resource `pack_format`, mirroring vanilla's pack-format gating.
    ///
    /// A pack with an explicit `supported_formats` range is accepted when the
    /// host format falls inside that inclusive range; otherwise the pack's flat
    /// `pack_format` must match the host exactly. This is what lets an older pack
    /// declare forward compatibility without editing every version bump.
    pub fn accepts(&self, host_format: u32) -> bool {
        match self.supported_formats {
            Some((lo, hi)) => host_format >= lo && host_format <= hi,
            None => self.pack_format == host_format,
        }
    }
}

impl From<VersionMeta> for PackMeta {
    fn from(version: VersionMeta) -> Self {
        Self {
            pack_format: version.resource_format.major,
            description: PackDescription::Text(version.id),
            supported_formats: None,
            pack_version: Some(version.resource_format),
        }
    }
}

/// A major/minor pack version, as found in a modern `version.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackVersion {
    /// The major version (the resource/data pack "format" number).
    pub major: u32,
    /// The minor version (`0` for the older flat/int shapes).
    pub minor: u32,
}

/// Parsed contents of a vanilla `version.json`.
///
/// This is how the built-in pack advertises its resource/data pack formats. The
/// `protocol_version` is a handy independent cross-check against the network
/// stack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionMeta {
    /// The version id, for example `"26.2"`.
    pub id: String,
    /// The wire protocol version, when present.
    pub protocol_version: Option<i32>,
    /// The resource pack format (major/minor).
    pub resource_format: PackVersion,
    /// The data pack format (major/minor).
    pub data_format: PackVersion,
}

impl VersionMeta {
    /// Parses a `version.json`.
    ///
    /// Copes with all three historical shapes of `pack_version`: the modern
    /// `{resource_major, resource_minor, data_major, data_minor}` object, the
    /// older `{resource, data}` flat-integer object, and the very old single
    /// integer.
    pub fn parse(bytes: &[u8]) -> Result<Self, AssetError> {
        let root: Value =
            serde_json::from_slice(bytes).map_err(|e| AssetError::MetaMalformed(e.to_string()))?;
        let id = root
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let protocol_version = root
            .get("protocol_version")
            .and_then(Value::as_i64)
            .map(|v| v as i32);
        let pack_version = root
            .get("pack_version")
            .ok_or_else(|| AssetError::MetaMalformed("missing \"pack_version\"".to_string()))?;
        let (resource_format, data_format) = parse_pack_version(pack_version)?;
        Ok(Self {
            id,
            protocol_version,
            resource_format,
            data_format,
        })
    }
}

/// Parses the `pack_version` field into `(resource, data)` versions.
fn parse_pack_version(value: &Value) -> Result<(PackVersion, PackVersion), AssetError> {
    let malformed = || AssetError::MetaMalformed("invalid \"pack_version\"".to_string());
    match value {
        // Very old: a single integer applies to both.
        Value::Number(_) => {
            let n = value.as_u64().ok_or_else(malformed)? as u32;
            let v = PackVersion { major: n, minor: 0 };
            Ok((v, v))
        }
        Value::Object(map) => {
            let pick = |major_key: &str, minor_key: &str, flat_key: &str| -> Option<PackVersion> {
                if let Some(major) = map.get(major_key).and_then(Value::as_u64) {
                    let minor = map.get(minor_key).and_then(Value::as_u64).unwrap_or(0);
                    Some(PackVersion {
                        major: major as u32,
                        minor: minor as u32,
                    })
                } else {
                    map.get(flat_key)
                        .and_then(Value::as_u64)
                        .map(|n| PackVersion {
                            major: n as u32,
                            minor: 0,
                        })
                }
            };
            let resource =
                pick("resource_major", "resource_minor", "resource").ok_or_else(malformed)?;
            let data = pick("data_major", "data_minor", "data").ok_or_else(malformed)?;
            Ok((resource, data))
        }
        _ => Err(malformed()),
    }
}

/// Parses the `supported_formats` field, which may be a single integer, a
/// two-element `[min, max]` array, or a `{min_inclusive, max_inclusive}` object.
fn parse_supported_formats(value: &Value) -> Result<(u32, u32), AssetError> {
    let malformed = || AssetError::MetaMalformed("invalid \"supported_formats\"".to_string());
    match value {
        Value::Number(_) => {
            let n = value.as_u64().ok_or_else(malformed)? as u32;
            Ok((n, n))
        }
        Value::Array(items) if items.len() == 2 => {
            let min = items[0].as_u64().ok_or_else(malformed)? as u32;
            let max = items[1].as_u64().ok_or_else(malformed)? as u32;
            Ok((min, max))
        }
        Value::Object(map) => {
            let min = map
                .get("min_inclusive")
                .and_then(Value::as_u64)
                .ok_or_else(malformed)? as u32;
            let max = map
                .get("max_inclusive")
                .and_then(Value::as_u64)
                .ok_or_else(malformed)? as u32;
            Ok((min, max))
        }
        _ => Err(malformed()),
    }
}
