//! Loading and validating a split browser `client.jar` asset.
//!
//! A deployment may place `client.jar.parts.json` beside the page to work around
//! static-host file-size limits. All names in that manifest are plain relative
//! filenames, so `fetch` resolves them below the page's own base path rather
//! than from the site's root.

use sha2::{Digest, Sha256};
use serde::Deserialize;

/// Relative manifest URL. It deliberately has no leading slash: Pages may
/// serve Lodestone below a project subpath.
pub const PARTS_MANIFEST_URL: &str = "client.jar.parts.json";

const MANIFEST_VERSION: u32 = 1;
const PART_PREFIX: &str = "client.jar.part-";
/// Kept well below Cloudflare Pages' 25 MiB per-file limit so deployment
/// packaging has room for representation and tooling overhead.
const MAX_PART_BYTES: u64 = 20 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 128 * 1024 * 1024;
const MAX_PARTS: usize = 128;

/// The checked shape produced by `web/scripts/stage_client_jar_parts.py`.
#[derive(Debug, Deserialize)]
pub struct ClientJarParts {
    version: u32,
    asset: String,
    total_bytes: u64,
    sha256: String,
    parts: Vec<ClientJarPart>,
}

#[derive(Debug, Deserialize)]
pub struct ClientJarPart {
    pub name: String,
    pub bytes: u64,
    pub sha256: String,
}

impl ClientJarParts {
    /// Parses an untrusted JSON manifest and checks its static invariants
    /// before any part URL is fetched.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let manifest: Self = serde_json::from_slice(bytes)
            .map_err(|error| format!("client.jar parts manifest is not valid JSON: {error}"))?;
        manifest.validate()?;
        Ok(manifest)
    }

    fn validate(&self) -> Result<(), String> {
        if self.version != MANIFEST_VERSION {
            return Err(format!(
                "client.jar parts manifest has unsupported version {}",
                self.version
            ));
        }
        if self.asset != "client.jar" {
            return Err("client.jar parts manifest names a different asset".to_string());
        }
        if self.parts.is_empty() || self.parts.len() > MAX_PARTS {
            return Err(format!(
                "client.jar parts manifest must contain 1..={MAX_PARTS} parts"
            ));
        }
        if self.total_bytes == 0 || self.total_bytes > MAX_TOTAL_BYTES {
            return Err(format!(
                "client.jar parts manifest total must be 1..={MAX_TOTAL_BYTES} bytes"
            ));
        }
        validate_sha256("client.jar parts manifest", &self.sha256)?;

        let mut total = 0_u64;
        for (index, part) in self.parts.iter().enumerate() {
            let expected_prefix = format!("{PART_PREFIX}{index:03}-");
            let expected_name = format!("{expected_prefix}{}", part.sha256.to_ascii_lowercase());
            if part.name != expected_name {
                return Err(format!(
                    "client.jar parts manifest part {index} must be named {expected_name}, got {}",
                    part.name
                ));
            }
            if part.bytes == 0 || part.bytes > MAX_PART_BYTES {
                return Err(format!(
                    "client.jar part {} must be 1..={MAX_PART_BYTES} bytes",
                    part.name
                ));
            }
            validate_sha256(&format!("client.jar part {index}"), &part.sha256)?;
            total = total
                .checked_add(part.bytes)
                .ok_or("client.jar parts manifest byte total overflow")?;
        }
        if total != self.total_bytes {
            return Err(format!(
                "client.jar parts manifest declares {} bytes but parts total {total}",
                self.total_bytes
            ));
        }
        Ok(())
    }

    /// The total allocation required after static manifest validation.
    pub fn total_len(&self) -> usize {
        // `validate` bounds this well below usize::MAX on supported targets.
        self.total_bytes as usize
    }

    /// The ordered parts. Callers must fetch these in this exact order.
    pub fn parts(&self) -> &[ClientJarPart] {
        &self.parts
    }

    /// Verifies the reconstructed jar against the manifest's whole-file digest.
    pub fn verify_complete(&self, bytes: &[u8]) -> Result<(), String> {
        if bytes.len() != self.total_len() {
            return Err(format!(
                "client.jar reconstruction has {} bytes, expected {}",
                bytes.len(),
                self.total_bytes
            ));
        }
        verify_hash("reconstructed client.jar", bytes, &self.sha256)
    }
}

impl ClientJarPart {
    /// Verifies one downloaded part before it is appended to the jar buffer.
    pub fn verify_download(&self, bytes: &[u8]) -> Result<(), String> {
        if bytes.len() as u64 != self.bytes {
            return Err(format!(
                "client.jar part {} has {} bytes, expected {}",
                self.name,
                bytes.len(),
                self.bytes
            ));
        }
        verify_hash(&format!("client.jar part {}", self.name), bytes, &self.sha256)
    }
}

fn validate_sha256(subject: &str, value: &str) -> Result<(), String> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("{subject} has an invalid SHA-256 digest"));
    }
    Ok(())
}

fn verify_hash(subject: &str, bytes: &[u8], expected: &str) -> Result<(), String> {
    let actual = format!("{:x}", Sha256::digest(bytes));
    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(format!("{subject} SHA-256 does not match its manifest"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A_SHA256: &str = "ca978112ca1bbdcafac231b39a23dc4da786eff8147c4e72b9807785afee48bb";
    const B_SHA256: &str = "3e23e8160039594a33894f6564e1b1348bbd7a0088d42c4acb73eeaed59c009d";
    const AB_SHA256: &str = "fb8e20fc2e4c3f248c60c39bd652f3c1347298bb977b8b4d5903b85055620603";

    fn part_name(index: usize, digest: &str) -> String {
        format!("client.jar.part-{index:03}-{digest}")
    }

    fn manifest(part_name: &str, part_bytes: u64, total_bytes: u64) -> String {
        format!(
            r#"{{"version":1,"asset":"client.jar","total_bytes":{total_bytes},"sha256":"{A_SHA256}","parts":[{{"name":"{part_name}","bytes":{part_bytes},"sha256":"{A_SHA256}"}}]}}"#
        )
    }

    fn two_part_manifest() -> String {
        format!(
            r#"{{"version":1,"asset":"client.jar","total_bytes":2,"sha256":"{AB_SHA256}","parts":[{{"name":"client.jar.part-000-{A_SHA256}","bytes":1,"sha256":"{A_SHA256}"}},{{"name":"client.jar.part-001-{B_SHA256}","bytes":1,"sha256":"{B_SHA256}"}}]}}"#
        )
    }

    #[test]
    fn accepts_an_ordered_bounded_manifest() {
        let name = part_name(0, A_SHA256);
        let parsed = ClientJarParts::parse(manifest(&name, 1, 1).as_bytes())
            .expect("valid manifest");
        assert_eq!(parsed.parts()[0].name, name);
        assert_eq!(parsed.total_len(), 1);
    }

    #[test]
    fn verifies_downloaded_parts_and_their_reconstructed_order() {
        let parsed = ClientJarParts::parse(two_part_manifest().as_bytes())
            .expect("ordered two-part manifest");
        parsed.parts()[0].verify_download(b"a").expect("first part");
        parsed.parts()[1].verify_download(b"b").expect("second part");
        parsed.verify_complete(b"ab").expect("ordered reconstruction");
        assert!(parsed.verify_complete(b"ba").is_err());
    }

    #[test]
    fn rejects_out_of_order_part_names() {
        let error = ClientJarParts::parse(manifest(&part_name(1, A_SHA256), 1, 1).as_bytes())
            .expect_err("the sequence is part of the integrity contract");
        assert!(error.contains("must be named client.jar.part-000"));
    }

    #[test]
    fn rejects_parts_over_the_hosting_limit() {
        let error = ClientJarParts::parse(
            manifest(&part_name(0, A_SHA256), MAX_PART_BYTES + 1, MAX_PART_BYTES + 1).as_bytes(),
        )
        .expect_err("oversized part");
        assert!(error.contains("must be 1..="));
    }

    #[test]
    fn rejects_manifest_size_mismatches() {
        let error = ClientJarParts::parse(manifest(&part_name(0, A_SHA256), 1, 2).as_bytes())
            .expect_err("declared total must be exact");
        assert!(error.contains("parts total"));
    }

    #[test]
    fn rejects_a_truncated_or_tampered_download() {
        let parsed = ClientJarParts::parse(manifest(&part_name(0, A_SHA256), 1, 1).as_bytes())
            .expect("valid manifest");
        assert!(parsed.parts()[0].verify_download(b"").is_err());
        assert!(parsed.verify_complete(b"b").is_err());
    }

    #[test]
    fn rejects_arbitrary_or_traversing_part_names() {
        for name in ["../client.jar", "client.jar.part-000", "client.jar.part-000-not-a-digest"] {
            let error = ClientJarParts::parse(manifest(name, 1, 1).as_bytes())
                .expect_err("only content-addressed sibling names are safe to fetch");
            assert!(error.contains("must be named"), "unexpected error for {name}: {error}");
        }
    }
}
