//! Tests for `pack.mcmeta` parsing ([`PackMeta`]).

use lodestone_assets::{PackDescription, PackMeta};

#[test]
fn parses_string_description() {
    let json = br#"{"pack":{"pack_format":55,"description":"My cool pack"}}"#;
    let meta = PackMeta::parse(json).unwrap();
    assert_eq!(meta.pack_format, 55);
    assert!(matches!(meta.description, PackDescription::Text(ref s) if s == "My cool pack"));
    assert_eq!(meta.description.plain_text(), "My cool pack");
    assert!(meta.supported_formats.is_none());
}

#[test]
fn parses_text_component_description() {
    let json = br#"{"pack":{"pack_format":55,"description":{"text":"Fancy","color":"gold"}}}"#;
    let meta = PackMeta::parse(json).unwrap();
    assert!(matches!(meta.description, PackDescription::Component(_)));
    // Best-effort plain text extraction pulls the "text" field.
    assert_eq!(meta.description.plain_text(), "Fancy");
}

#[test]
fn parses_component_array_description() {
    let json = br#"{"pack":{"pack_format":55,"description":[{"text":"a"},{"text":"b"}]}}"#;
    let meta = PackMeta::parse(json).unwrap();
    assert!(matches!(meta.description, PackDescription::Component(_)));
    assert_eq!(meta.description.plain_text(), "ab");
}

#[test]
fn parses_supported_formats_array() {
    let json = br#"{"pack":{"pack_format":57,"description":"x","supported_formats":[55,57]}}"#;
    let meta = PackMeta::parse(json).unwrap();
    assert_eq!(meta.supported_formats, Some((55, 57)));
}

#[test]
fn parses_supported_formats_object() {
    let json = br#"{"pack":{"pack_format":57,"description":"x","supported_formats":{"min_inclusive":55,"max_inclusive":60}}}"#;
    let meta = PackMeta::parse(json).unwrap();
    assert_eq!(meta.supported_formats, Some((55, 60)));
}

#[test]
fn parses_supported_formats_single_int() {
    let json = br#"{"pack":{"pack_format":57,"description":"x","supported_formats":57}}"#;
    let meta = PackMeta::parse(json).unwrap();
    assert_eq!(meta.supported_formats, Some((57, 57)));
}

#[test]
fn missing_pack_format_is_malformed() {
    let json = br#"{"pack":{"description":"x"}}"#;
    assert!(PackMeta::parse(json).is_err());
}

#[test]
fn missing_pack_object_is_malformed() {
    let json = br#"{"description":"x"}"#;
    assert!(PackMeta::parse(json).is_err());
}

#[test]
fn invalid_json_is_malformed_not_panic() {
    let json = br#"{"pack": not json"#;
    let err = PackMeta::parse(json).unwrap_err();
    // A clear error, never a panic.
    assert!(format!("{err}").contains("malformed"));
}

// --- version.json (vanilla built-in pack metadata) ---

use lodestone_assets::{PackVersion, VersionMeta};

#[test]
fn parses_version_json_major_minor() {
    let json = br#"{"id":"26.2","protocol_version":776,"pack_version":{"resource_major":88,"resource_minor":0,"data_major":107,"data_minor":1}}"#;
    let v = VersionMeta::parse(json).unwrap();
    assert_eq!(v.id, "26.2");
    assert_eq!(v.protocol_version, Some(776));
    assert_eq!(
        v.resource_format,
        PackVersion {
            major: 88,
            minor: 0
        }
    );
    assert_eq!(
        v.data_format,
        PackVersion {
            major: 107,
            minor: 1
        }
    );
    assert_eq!(v.resource_format.major, 88);
}

#[test]
fn parses_version_json_flat_ints() {
    // Older shape: pack_version is a flat {resource, data} of single ints.
    let json = br#"{"id":"1.20","pack_version":{"resource":15,"data":18}}"#;
    let v = VersionMeta::parse(json).unwrap();
    assert_eq!(
        v.resource_format,
        PackVersion {
            major: 15,
            minor: 0
        }
    );
    assert_eq!(
        v.data_format,
        PackVersion {
            major: 18,
            minor: 0
        }
    );
}

#[test]
fn parses_version_json_single_int() {
    // Very old shape: pack_version is a single integer.
    let json = br#"{"id":"1.14","pack_version":4}"#;
    let v = VersionMeta::parse(json).unwrap();
    assert_eq!(v.resource_format, PackVersion { major: 4, minor: 0 });
    assert_eq!(v.data_format, PackVersion { major: 4, minor: 0 });
}

#[test]
fn version_json_into_pack_meta() {
    let json = br#"{"id":"26.2","pack_version":{"resource_major":88,"resource_minor":0,"data_major":107,"data_minor":1}}"#;
    let meta = PackMeta::from_version_json(json).unwrap();
    assert_eq!(meta.pack_format, 88);
    assert_eq!(
        meta.pack_version,
        Some(PackVersion {
            major: 88,
            minor: 0
        })
    );
    assert_eq!(meta.description.plain_text(), "26.2");
}

#[test]
fn version_json_malformed_is_error() {
    assert!(VersionMeta::parse(br#"{"id":"x"}"#).is_err()); // missing pack_version
    assert!(VersionMeta::parse(br#"not json"#).is_err());
}

#[test]
fn pack_mcmeta_has_no_pack_version() {
    let json = br#"{"pack":{"pack_format":55,"description":"x"}}"#;
    let meta = PackMeta::parse(json).unwrap();
    assert_eq!(meta.pack_version, None);
}
