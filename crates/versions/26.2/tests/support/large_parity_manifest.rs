//! Reader and record encoders for the frozen-world large-parity interchange
//! formats. This is test support rather than a game protocol.

use std::io::{self, Read};

use lodestone_core::{Nbt, Writer};
use lodestone_v26_2::packets::chunk::LevelChunkWithLight;
use lodestone_world::LightData;

pub const HEADER_BYTES: usize = 256;
/// Header bytes `70..72` identify the terrain-adaptation scope. The
/// authenticated world manifests all use the production structure scope;
/// composed stage fixtures carry their empty scope in their text schema.
pub const STRUCTURE_BEARD_SCOPE_PRODUCTION_REAL: u16 = 0;
pub const GRID_MIN: i32 = -250;
pub const GRID_MAX: i32 = 250;
pub const GRID_SIDE: i32 = GRID_MAX - GRID_MIN + 1;
pub const GRID_COUNT: u64 = (GRID_SIDE as u64) * (GRID_SIDE as u64);
/// Geometry of the v6 raw packet-hash manifest. The legacy `GRID_*` constants
/// above intentionally remain the v3-v5 501-square contract.
pub const RAW_GRID_MIN: i32 = -500;
pub const RAW_GRID_MAX: i32 = 500;
pub const RAW_GRID_SIDE: i32 = RAW_GRID_MAX - RAW_GRID_MIN + 1;
pub const RAW_GRID_COUNT: u64 = (RAW_GRID_SIDE as u64) * (RAW_GRID_SIDE as u64);
const MAGIC: &[u8; 8] = b"LWP26P03";
const MAGIC_V4: &[u8; 8] = b"LWP26P04";
const MAGIC_V5: &[u8; 8] = b"LWP26P05";
const MAGIC_V6: &[u8; 8] = b"LWP26P06";
const MAGIC_V7: &[u8; 8] = b"LWP26P07";
const PACKET_AUDIT_MAGIC: &[u8; 8] = b"LWP26A06";
const LIGHT_FREE_AUDIT_MAGIC: &[u8; 8] = b"LWP26A07";
const DOMAIN: &[u8] = b"lodestone.worldgen.large-parity.manifest/v3/semantic";
const DOMAIN_V4: &[u8] = b"lodestone.worldgen.large-parity.manifest/v4/semantic";
const DOMAIN_V5: &[u8] = b"lodestone.worldgen.large-parity.manifest/v5/semantic";
const DOMAIN_V6: &[u8] = b"lodestone.worldgen.large-parity.manifest/v6/raw-packet";
const DOMAIN_V7: &[u8] = b"lodestone.worldgen.large-parity.manifest/v7/light-free";
const PACKET_AUDIT_DOMAIN: &[u8] = b"lodestone.worldgen.large-parity.packet-audit/v6/raw-packet";
const LIGHT_FREE_AUDIT_DOMAIN: &[u8] = b"lodestone.worldgen.large-parity.audit/v7/light-free";
const RECORD_DOMAIN: &[u8] = b"lodestone.worldgen.large-parity.chunk/v3/semantic";
const RECORD_DOMAIN_V4: &[u8] = b"lodestone.worldgen.large-parity.chunk/v4/semantic";
const RECORD_DOMAIN_V5: &[u8] = b"lodestone.worldgen.large-parity.chunk/v5/semantic";
const RECORD_DOMAIN_V7: &[u8] = b"lodestone.worldgen.large-parity.chunk/v7/light-free";
const DIGEST_BYTES: u64 = 32;
pub const RAW_PACKET_HASH_BYTES: usize = 2;
pub const PACKET_AUDIT_RECORD_BYTES: usize = DIGEST_BYTES as usize;
pub const LIGHT_FREE_AUDIT_RECORD_BYTES: usize = DIGEST_BYTES as usize;
const MANIFEST_KIND: u16 = 2;
const PACKET_AUDIT_KIND: u16 = 3;

/// The dimension named by a large-parity manifest. The wire identity is the
/// SHA-256 of the canonical resource-location string stored in the header;
/// keeping the enum here prevents a shard from silently being compared using
/// the wrong vertical shape or generator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dimension {
    Overworld,
    Nether,
    End,
}

impl Dimension {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Overworld => "minecraft:overworld",
            Self::Nether => "minecraft:the_nether",
            Self::End => "minecraft:the_end",
        }
    }

    fn digest(self) -> [u8; 32] { sha256(self.name().as_bytes()) }

    fn from_digest(value: [u8; 32]) -> io::Result<Self> {
        [Self::Overworld, Self::Nether, Self::End]
            .into_iter()
            .find(|dimension| dimension.digest() == value)
            .ok_or_else(|| invalid("large-parity manifest has an unknown dimension identity"))
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Header {
    pub semantic_version: u16,
    pub cx0: i32,
    pub cx1: i32,
    pub cz0: i32,
    pub cz1: i32,
    pub count: u64,
    pub frozen_world: [u8; 32],
    /// v3 manifests are legacy overworld manifests; v4 and v5 store this identity
    /// explicitly in header bytes 168..200.
    pub dimension: Dimension,
    /// The per-coordinate record width. Legacy semantic manifests use 32;
    /// v6 raw packet manifests use the committed two-byte hash prefix.
    pub record_width: u16,
    /// Header format discriminator. Main manifests use 2; packet-audit
    /// sidecars use 3 and are parsed as [`PacketAuditHeader`].
    pub kind: u16,
}

/// Authenticated header for the v6 full-digest packet-audit sidecar.
///
/// The sidecar deliberately has a distinct magic and kind from the two-byte
/// main manifest. Keeping this type separate prevents a full audit stream
/// from being mistaken for semantic records or accepted as the main payload.
#[derive(Debug, Clone, Copy)]
pub struct PacketAuditHeader {
    pub semantic_version: u16,
    pub cx0: i32,
    pub cx1: i32,
    pub cz0: i32,
    pub cz1: i32,
    pub count: u64,
    pub frozen_world: [u8; 32],
    pub dimension: Dimension,
    pub record_width: u16,
    pub kind: u16,
    pub payload_digest: [u8; 32],
}

/// Authenticated full-digest sidecar for the v7 light-free content manifest.
/// Its distinct magic and schema keep it from being accepted as a v6 packet
/// audit or as a second main payload.
#[derive(Debug, Clone, Copy)]
pub struct LightFreeAuditHeader {
    pub semantic_version: u16,
    pub cx0: i32,
    pub cx1: i32,
    pub cz0: i32,
    pub cz1: i32,
    pub count: u64,
    pub frozen_world: [u8; 32],
    pub dimension: Dimension,
    pub record_width: u16,
    pub kind: u16,
    pub payload_digest: [u8; 32],
}

fn invalid(message: &'static str) -> io::Error { io::Error::new(io::ErrorKind::InvalidData, message) }
fn be_i32(b: &[u8]) -> i32 { i32::from_be_bytes(b.try_into().expect("fixed-width header field")) }
fn be_u64(b: &[u8]) -> u64 { u64::from_be_bytes(b.try_into().expect("fixed-width header field")) }

fn checked_record_count(cx0: i32, cx1: i32, cz0: i32, cz1: i32) -> Option<u64> {
    if cx0 > cx1 || cz0 > cz1 { return None; }
    let width = i64::from(cx1).checked_sub(i64::from(cx0))?.checked_add(1)?;
    let height = i64::from(cz1).checked_sub(i64::from(cz0))?.checked_add(1)?;
    u64::try_from(width).ok()?.checked_mul(u64::try_from(height).ok()?)
}

/// Reads a legacy or v6/v7 header. A v2 file is rejected explicitly: its hashes
/// describe non-canonical packet bytes and cannot be upgraded without re-exporting.
pub fn read_header(mut r: impl Read) -> io::Result<Header> {
    let mut b = [0; HEADER_BYTES]; r.read_exact(&mut b)?;
    if &b[..8] == b"LWP26P02" { return Err(invalid("v2 raw-packet manifest is rejected; regenerate frozen-world semantic v3")); }
    let magic = &b[..8];
    let version = u16::from_be_bytes(b[8..10].try_into().unwrap());
    let schema = u16::from_be_bytes(b[14..16].try_into().unwrap());
    let valid_v3 = magic == MAGIC && version == 3 && schema == 3;
    let valid_v4 = magic == MAGIC_V4 && version == 4 && schema == 4;
    let valid_v5 = magic == MAGIC_V5 && version == 5 && schema == 5;
    let valid_v6 = magic == MAGIC_V6 && version == 6 && schema == 6;
    let valid_v7 = magic == MAGIC_V7 && version == 7 && schema == 7;
    let record_width = u16::from_be_bytes(b[68..70].try_into().unwrap());
    let expected_width = if valid_v6 || valid_v7 { RAW_PACKET_HASH_BYTES as u16 } else { DIGEST_BYTES as u16 };
    if !(valid_v3 || valid_v4 || valid_v5 || valid_v6 || valid_v7) || u16::from_be_bytes(b[10..12].try_into().unwrap()) != HEADER_BYTES as u16 || u16::from_be_bytes(b[12..14].try_into().unwrap()) != MANIFEST_KIND || u32::from_be_bytes(b[16..20].try_into().unwrap()) != 776 || i64::from_be_bytes(b[20..28].try_into().unwrap()) != 42 || record_width != expected_width || u16::from_be_bytes(b[70..72].try_into().unwrap()) != STRUCTURE_BEARD_SCOPE_PRODUCTION_REAL {
        return Err(invalid("unsupported large-parity header"));
    }
    if valid_v3 && b[72..104] != sha256(DOMAIN) { return Err(invalid("large-parity semantic schema digest differs")); }
    if valid_v4 && b[72..104] != sha256(DOMAIN_V4) { return Err(invalid("large-parity dimension schema digest differs")); }
    if valid_v5 && b[72..104] != sha256(DOMAIN_V5) { return Err(invalid("large-parity normalized-light schema digest differs")); }
    if valid_v6 && b[72..104] != sha256(DOMAIN_V6) { return Err(invalid("large-parity raw-packet schema digest differs")); }
    if valid_v7 && b[72..104] != sha256(DOMAIN_V7) { return Err(invalid("large-parity light-free schema digest differs")); }
    let frozen_world: [u8; 32] = b[104..136].try_into().unwrap();
    if frozen_world == [0; 32] { return Err(invalid("large-parity manifest has no frozen-world identity")); }
    let dimension = if valid_v3 { Dimension::Overworld } else { Dimension::from_digest(b[168..200].try_into().unwrap())? };
    if (valid_v4 || valid_v5 || valid_v6 || valid_v7) && b[168..200] == [0; 32] { return Err(invalid("large-parity dimension manifest has no dimension identity")); }
    let (grid_min, grid_max) = if valid_v6 || valid_v7 { (RAW_GRID_MIN, RAW_GRID_MAX) } else { (GRID_MIN, GRID_MAX) };
    if (be_i32(&b[28..32]), be_i32(&b[32..36]), be_i32(&b[36..40]), be_i32(&b[40..44])) != (grid_min, grid_max, grid_min, grid_max) { return Err(invalid("manifest global bounds differ")); }
    let h = Header { semantic_version: version, cx0: be_i32(&b[44..48]), cx1: be_i32(&b[48..52]), cz0: be_i32(&b[52..56]), cz1: be_i32(&b[56..60]), count: be_u64(&b[60..68]), frozen_world, dimension, record_width, kind: u16::from_be_bytes(b[12..14].try_into().unwrap()) };
    let expected = checked_record_count(h.cx0, h.cx1, h.cz0, h.cz1).ok_or_else(|| invalid("invalid large-parity shard bounds"))?;
    if !(h.cx0 >= grid_min && h.cx1 <= grid_max && h.cz0 >= grid_min && h.cz1 <= grid_max && h.count == expected) { return Err(invalid("invalid large-parity shard bounds")); }
    Ok(h)
}

/// Reads the v6 full-digest packet-audit sidecar header. Its offset-12
/// discriminator is 3, intentionally distinct from the main manifest's 2.
pub fn read_packet_audit_header(mut r: impl Read) -> io::Result<PacketAuditHeader> {
    let mut b = [0; HEADER_BYTES]; r.read_exact(&mut b)?;
    let magic = &b[..8];
    let version = u16::from_be_bytes(b[8..10].try_into().unwrap());
    let kind = u16::from_be_bytes(b[12..14].try_into().unwrap());
    let schema = u16::from_be_bytes(b[14..16].try_into().unwrap());
    let record_width = u16::from_be_bytes(b[68..70].try_into().unwrap());
    if magic != PACKET_AUDIT_MAGIC
        || version != 6
        || u16::from_be_bytes(b[10..12].try_into().unwrap()) != HEADER_BYTES as u16
        || kind != PACKET_AUDIT_KIND
        || schema != 6
        || u32::from_be_bytes(b[16..20].try_into().unwrap()) != 776
        || i64::from_be_bytes(b[20..28].try_into().unwrap()) != 42
        || record_width != PACKET_AUDIT_RECORD_BYTES as u16
        || u16::from_be_bytes(b[70..72].try_into().unwrap()) != STRUCTURE_BEARD_SCOPE_PRODUCTION_REAL
    {
        return Err(invalid("unsupported large-parity packet-audit header"));
    }
    if b[72..104] != sha256(PACKET_AUDIT_DOMAIN) {
        return Err(invalid("large-parity packet-audit schema digest differs"));
    }
    let frozen_world: [u8; 32] = b[104..136].try_into().unwrap();
    if frozen_world == [0; 32] {
        return Err(invalid("large-parity packet-audit has no frozen-world identity"));
    }
    let dimension_digest: [u8; 32] = b[168..200].try_into().unwrap();
    if dimension_digest == [0; 32] {
        return Err(invalid("large-parity packet-audit has no dimension identity"));
    }
    let dimension = Dimension::from_digest(dimension_digest)
        .map_err(|_| invalid("large-parity packet-audit has an unknown dimension identity"))?;
    if (be_i32(&b[28..32]), be_i32(&b[32..36]), be_i32(&b[36..40]), be_i32(&b[40..44]))
        != (RAW_GRID_MIN, RAW_GRID_MAX, RAW_GRID_MIN, RAW_GRID_MAX)
    {
        return Err(invalid("packet-audit global bounds differ"));
    }
    let header = PacketAuditHeader {
        semantic_version: version,
        cx0: be_i32(&b[44..48]),
        cx1: be_i32(&b[48..52]),
        cz0: be_i32(&b[52..56]),
        cz1: be_i32(&b[56..60]),
        count: be_u64(&b[60..68]),
        frozen_world,
        dimension,
        record_width,
        kind,
        payload_digest: b[136..168].try_into().unwrap(),
    };
    let expected = checked_record_count(header.cx0, header.cx1, header.cz0, header.cz1)
        .ok_or_else(|| invalid("invalid packet-audit shard bounds"))?;
    if !(header.cx0 >= RAW_GRID_MIN
        && header.cx1 <= RAW_GRID_MAX
        && header.cz0 >= RAW_GRID_MIN
        && header.cz1 <= RAW_GRID_MAX
        && header.count == expected)
    {
        return Err(invalid("invalid packet-audit shard bounds"));
    }
    Ok(header)
}

/// Checks that a packet-audit sidecar is the full-digest companion for a v6
/// two-byte main manifest, rather than merely another valid raw artifact.
pub fn validate_packet_audit_header(main: &Header, audit: &PacketAuditHeader) -> io::Result<()> {
    if main.semantic_version != 6
        || main.kind != MANIFEST_KIND
        || main.record_width != RAW_PACKET_HASH_BYTES as u16
        || audit.semantic_version != 6
        || audit.kind != PACKET_AUDIT_KIND
        || audit.record_width != PACKET_AUDIT_RECORD_BYTES as u16
        || audit.cx0 != main.cx0
        || audit.cx1 != main.cx1
        || audit.cz0 != main.cz0
        || audit.cz1 != main.cz1
        || audit.count != main.count
        || audit.frozen_world != main.frozen_world
        || audit.dimension != main.dimension
    {
        return Err(invalid("packet-audit sidecar identity differs from v6 manifest"));
    }
    Ok(())
}

/// Reads and authenticates the v7 full-digest sidecar header.
pub fn read_light_free_audit_header(mut r: impl Read) -> io::Result<LightFreeAuditHeader> {
    let mut b = [0; HEADER_BYTES]; r.read_exact(&mut b)?;
    let magic = &b[..8];
    let version = u16::from_be_bytes(b[8..10].try_into().unwrap());
    let kind = u16::from_be_bytes(b[12..14].try_into().unwrap());
    let schema = u16::from_be_bytes(b[14..16].try_into().unwrap());
    let record_width = u16::from_be_bytes(b[68..70].try_into().unwrap());
    if magic != LIGHT_FREE_AUDIT_MAGIC
        || version != 7
        || u16::from_be_bytes(b[10..12].try_into().unwrap()) != HEADER_BYTES as u16
        || kind != PACKET_AUDIT_KIND
        || schema != 7
        || u32::from_be_bytes(b[16..20].try_into().unwrap()) != 776
        || i64::from_be_bytes(b[20..28].try_into().unwrap()) != 42
        || record_width != LIGHT_FREE_AUDIT_RECORD_BYTES as u16
        || u16::from_be_bytes(b[70..72].try_into().unwrap()) != STRUCTURE_BEARD_SCOPE_PRODUCTION_REAL
    {
        return Err(invalid("unsupported large-parity light-free audit header"));
    }
    if b[72..104] != sha256(LIGHT_FREE_AUDIT_DOMAIN) {
        return Err(invalid("large-parity light-free audit schema digest differs"));
    }
    let frozen_world: [u8; 32] = b[104..136].try_into().unwrap();
    if frozen_world == [0; 32] {
        return Err(invalid("large-parity light-free audit has no frozen-world identity"));
    }
    let dimension_digest: [u8; 32] = b[168..200].try_into().unwrap();
    if dimension_digest == [0; 32] {
        return Err(invalid("large-parity light-free audit has no dimension identity"));
    }
    let dimension = Dimension::from_digest(dimension_digest)
        .map_err(|_| invalid("large-parity light-free audit has an unknown dimension identity"))?;
    if (be_i32(&b[28..32]), be_i32(&b[32..36]), be_i32(&b[36..40]), be_i32(&b[40..44]))
        != (RAW_GRID_MIN, RAW_GRID_MAX, RAW_GRID_MIN, RAW_GRID_MAX)
    {
        return Err(invalid("light-free audit global bounds differ"));
    }
    let header = LightFreeAuditHeader {
        semantic_version: version,
        cx0: be_i32(&b[44..48]),
        cx1: be_i32(&b[48..52]),
        cz0: be_i32(&b[52..56]),
        cz1: be_i32(&b[56..60]),
        count: be_u64(&b[60..68]),
        frozen_world,
        dimension,
        record_width,
        kind,
        payload_digest: b[136..168].try_into().unwrap(),
    };
    let expected = checked_record_count(header.cx0, header.cx1, header.cz0, header.cz1)
        .ok_or_else(|| invalid("invalid light-free audit shard bounds"))?;
    if !(header.cx0 >= RAW_GRID_MIN
        && header.cx1 <= RAW_GRID_MAX
        && header.cz0 >= RAW_GRID_MIN
        && header.cz1 <= RAW_GRID_MAX
        && header.count == expected)
    {
        return Err(invalid("invalid light-free audit shard bounds"));
    }
    Ok(header)
}

/// Checks that a v7 sidecar is the full-digest companion for its two-byte main
/// manifest and carries the same shard, frozen-world, and dimension identity.
pub fn validate_light_free_audit_header(main: &Header, audit: &LightFreeAuditHeader) -> io::Result<()> {
    if main.semantic_version != 7
        || main.kind != MANIFEST_KIND
        || main.record_width != RAW_PACKET_HASH_BYTES as u16
        || audit.semantic_version != 7
        || audit.kind != PACKET_AUDIT_KIND
        || audit.record_width != LIGHT_FREE_AUDIT_RECORD_BYTES as u16
        || audit.cx0 != main.cx0
        || audit.cx1 != main.cx1
        || audit.cz0 != main.cz0
        || audit.cz1 != main.cz1
        || audit.count != main.count
        || audit.frozen_world != main.frozen_world
        || audit.dimension != main.dimension
    {
        return Err(invalid("light-free audit sidecar identity differs from v7 manifest"));
    }
    Ok(())
}

/// Streams full 32-byte semantic digests through SHA-256 before comparison.
pub fn verify_payload(mut r: impl Read, count: u64, expected: [u8; 32]) -> io::Result<()> {
    verify_payload_with_width(&mut r, count, DIGEST_BYTES as u16, expected)
}

/// Streams a payload of either legacy 32-byte semantic records or v6 two-byte
/// raw packet hashes through the authenticated payload checksum. The reader is
/// consumed exactly to the payload boundary; trailing bytes are rejected.
pub fn verify_payload_with_width(mut r: impl Read, count: u64, width: u16, expected: [u8; 32]) -> io::Result<()> {
    if width == 0 { return Err(invalid("large-parity payload has zero record width")); }
    let mut sha = Sha256::new(); let mut left = count.checked_mul(width as u64).ok_or_else(|| invalid("payload count overflow"))?; let mut buf = [0u8; 8192];
    while left != 0 { let n = usize::try_from(left.min(buf.len() as u64)).unwrap(); r.read_exact(&mut buf[..n])?; sha.update(&buf[..n]); left -= n as u64; }
    let mut trailing = [0u8; 1]; if r.read(&mut trailing)? != 0 { return Err(invalid("large-parity payload has trailing bytes")); }
    if sha.finish() != expected { return Err(invalid("large-parity payload checksum differs")); }
    Ok(())
}

/// Verifies a payload using the width authenticated by its header.
pub fn verify_manifest_payload(r: impl Read, header: &Header, expected: [u8; 32]) -> io::Result<()> {
    verify_payload_with_width(r, header.count, header.record_width, expected)
}

/// Authenticates both v6 payloads in lockstep and proves that each full audit
/// digest has the same two-byte prefix stored by the main manifest. Readers
/// must be positioned at their respective payload starts.
pub fn verify_raw_packet_audit_pair(
    mut main: impl Read,
    mut audit: impl Read,
    count: u64,
    main_expected: [u8; 32],
    audit_expected: [u8; 32],
) -> io::Result<()> {
    let mut main_sha = Sha256::new();
    let mut audit_sha = Sha256::new();
    let mut prefix = [0u8; RAW_PACKET_HASH_BYTES];
    let mut full = [0u8; PACKET_AUDIT_RECORD_BYTES];
    for index in 0..count {
        main.read_exact(&mut prefix)?;
        audit.read_exact(&mut full)?;
        main_sha.update(&prefix);
        audit_sha.update(&full);
        if full[..RAW_PACKET_HASH_BYTES] != prefix {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("packet-audit prefix differs from v6 manifest at record {index}"),
            ));
        }
    }
    let mut trailing = [0u8; 1];
    if main.read(&mut trailing)? != 0 {
        return Err(invalid("v6 raw-packet manifest has trailing bytes"));
    }
    if audit.read(&mut trailing)? != 0 {
        return Err(invalid("packet-audit sidecar has trailing bytes"));
    }
    if main_sha.finish() != main_expected {
        return Err(invalid("v6 raw-packet manifest checksum differs"));
    }
    if audit_sha.finish() != audit_expected {
        return Err(invalid("packet-audit sidecar checksum differs"));
    }
    Ok(())
}

/// Authenticates the v7 full-digest content sidecar in lockstep and proves
/// every full digest has the two-byte prefix stored in the main manifest.
pub fn verify_light_free_audit_pair(
    mut main: impl Read,
    mut audit: impl Read,
    count: u64,
    main_expected: [u8; 32],
    audit_expected: [u8; 32],
) -> io::Result<()> {
    let mut main_sha = Sha256::new();
    let mut audit_sha = Sha256::new();
    let mut prefix = [0u8; RAW_PACKET_HASH_BYTES];
    let mut full = [0u8; LIGHT_FREE_AUDIT_RECORD_BYTES];
    for index in 0..count {
        main.read_exact(&mut prefix)?;
        audit.read_exact(&mut full)?;
        main_sha.update(&prefix);
        audit_sha.update(&full);
        if full[..RAW_PACKET_HASH_BYTES] != prefix {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("light-free audit prefix differs from v7 manifest at record {index}"),
            ));
        }
    }
    let mut trailing = [0u8; 1];
    if main.read(&mut trailing)? != 0 { return Err(invalid("v7 light-free manifest has trailing bytes")); }
    if audit.read(&mut trailing)? != 0 { return Err(invalid("light-free audit sidecar has trailing bytes")); }
    if main_sha.finish() != main_expected { return Err(invalid("v7 light-free manifest checksum differs")); }
    if audit_sha.finish() != audit_expected { return Err(invalid("light-free audit sidecar checksum differs")); }
    Ok(())
}

/// Hashes the exact chunk-with-light packet body and commits its first two
/// SHA-256 bytes as the v6 per-coordinate record. The full digest is retained
/// by this helper for optional audit output, never widened in the manifest.
pub fn raw_packet_hash(packet_body: &[u8]) -> [u8; RAW_PACKET_HASH_BYTES] {
    let digest = sha256(packet_body);
    [digest[0], digest[1]]
}

/// Descriptive alias for callers that want to make the byte-level input
/// explicit at the comparison site.
pub fn hash_exact_chunk_with_light_body(packet_body: &[u8]) -> [u8; RAW_PACKET_HASH_BYTES] {
    raw_packet_hash(packet_body)
}

pub fn raw_packet_full_digest(packet_body: &[u8]) -> [u8; 32] { sha256(packet_body) }

pub fn payload_digest_from_header(b: &[u8; HEADER_BYTES]) -> [u8; 32] { b[136..168].try_into().unwrap() }

/// Produces the v3 record whose SHA-256 is stored in the external manifest.
/// It intentionally decodes palette containers and represents their resolved
/// ids, not their packet-specific palettes or map traversal order.
pub fn semantic_record(packet: &LevelChunkWithLight) -> Vec<u8> {
    let mut w = Writer::default(); w.bytes(RECORD_DOMAIN); w.i32(packet.x); w.i32(packet.z);
    let mut maps = packet.heightmaps.iter().collect::<Vec<_>>(); maps.sort_unstable_by_key(|(id, _)| *id);
    w.i32(maps.len() as i32);
    // Packet storage is offset from the overworld's -64 minimum; the semantic
    // record uses the decoded first-available Y value.
    for (id, map) in maps { w.i32(id as i32); for z in 0..16 { for x in 0..16 { w.i32(map.get(x, z) as i32 - 64); } } }
    for section_index in 0..24 {
        let section = packet.column.section(section_index);
        for cell in 0..4096 { w.u32(section.map_or(0, |section| section.block_states().get(cell))); }
        for cell in 0..64 { w.u32(section.map_or(0, |section| section.biomes().get(cell))); }
    }
    let mut entities = packet.block_entities.clone(); entities.sort_unstable_by_key(|entity| (entity.rel_x, entity.y, entity.rel_z, entity.type_id));
    w.i32(entities.len() as i32);
    for entity in &entities { w.u8(entity.rel_x); w.i16(entity.y); w.u8(entity.rel_z); w.u32(entity.type_id); canonical_nbt(&mut w, &entity.nbt); }
    canonical_light(&mut w, &packet.light, true); canonical_light(&mut w, &packet.light, false);
    w.as_slice().to_vec()
}

pub fn semantic_digest(packet: &LevelChunkWithLight) -> [u8; 32] { sha256(&semantic_record(packet)) }

/// Canonical record for a v4 dimension manifest. Unlike the legacy entry point,
/// this uses the packet's decoded vertical window and includes the dimension
/// resource location in the record domain. The v3 function above intentionally
/// remains byte-for-byte unchanged so the active overworld baseline is still a
/// valid reference.
pub fn semantic_record_for_dimension(packet: &LevelChunkWithLight, dimension: Dimension) -> Vec<u8> {
    let mut w = Writer::default();
    w.bytes(RECORD_DOMAIN_V4);
    w.i32(packet.x);
    w.i32(packet.z);
    w.bytes(dimension.name().as_bytes());
    let mut maps = packet.heightmaps.iter().collect::<Vec<_>>();
    maps.sort_unstable_by_key(|(id, _)| *id);
    w.i32(maps.len() as i32);
    for (id, map) in maps {
        w.i32(id as i32);
        for z in 0..16 { for x in 0..16 { w.i32(map.get(x, z) as i32 + packet.column.min_y()); } }
    }
    for section_index in 0..packet.column.section_count() {
        let section = packet.column.section(section_index);
        for cell in 0..4096 { w.u32(section.map_or(0, |section| section.block_states().get(cell))); }
        for cell in 0..64 { w.u32(section.map_or(0, |section| section.biomes().get(cell))); }
    }
    let mut entities = packet.block_entities.clone();
    entities.sort_unstable_by_key(|entity| (entity.rel_x, entity.y, entity.rel_z, entity.type_id));
    w.i32(entities.len() as i32);
    for entity in &entities { w.u8(entity.rel_x); w.i16(entity.y); w.u8(entity.rel_z); w.u32(entity.type_id); canonical_nbt(&mut w, &entity.nbt); }
    canonical_light(&mut w, &packet.light, true); canonical_light(&mut w, &packet.light, false);
    w.as_slice().to_vec()
}

pub fn semantic_digest_for_dimension(packet: &LevelChunkWithLight, dimension: Dimension) -> [u8; 32] {
    sha256(&semantic_record_for_dimension(packet, dimension))
}

/// Canonical record for a v5 dimension manifest. V5 retains the v4 decoded
/// terrain fields but removes only the redundant all-15 sky tail from an
/// initial chunk, so independent light-storage layouts compare by what the
/// client sees rather than by allocation history.
pub fn semantic_record_v5_for_dimension(packet: &LevelChunkWithLight, dimension: Dimension) -> Vec<u8> {
    let mut w = Writer::default();
    w.bytes(RECORD_DOMAIN_V5);
    w.i32(packet.x);
    w.i32(packet.z);
    w.bytes(dimension.name().as_bytes());
    let mut maps = packet.heightmaps.iter().collect::<Vec<_>>();
    maps.sort_unstable_by_key(|(id, _)| *id);
    w.i32(maps.len() as i32);
    for (id, map) in maps {
        w.i32(id as i32);
        for z in 0..16 { for x in 0..16 { w.i32(map.get(x, z) as i32 + packet.column.min_y()); } }
    }
    for section_index in 0..packet.column.section_count() {
        let section = packet.column.section(section_index);
        for cell in 0..4096 { w.u32(section.map_or(0, |section| section.block_states().get(cell))); }
        for cell in 0..64 { w.u32(section.map_or(0, |section| section.biomes().get(cell))); }
    }
    let mut entities = packet.block_entities.clone();
    entities.sort_unstable_by_key(|entity| (entity.rel_x, entity.y, entity.rel_z, entity.type_id));
    w.i32(entities.len() as i32);
    for entity in &entities { w.u8(entity.rel_x); w.i16(entity.y); w.u8(entity.rel_z); w.u32(entity.type_id); canonical_nbt(&mut w, &entity.nbt); }
    canonical_light_v5(&mut w, &packet.light, true);
    canonical_light_v5(&mut w, &packet.light, false);
    w.as_slice().to_vec()
}

pub fn semantic_digest_v5_for_dimension(packet: &LevelChunkWithLight, dimension: Dimension) -> [u8; 32] {
    sha256(&semantic_record_v5_for_dimension(packet, dimension))
}

/// Produces the v7 content-only record directly from a generated server
/// column. No packet shape, light container, or light settlement is involved.
/// The byte order mirrors the independent Java exporter: domain/coordinates/
/// dimension, the three client heightmaps, then section-major Y/Z/X state
/// cells and 4×4×4 biome cells, followed by canonical block entities.
pub fn light_free_record(
    column: &lodestone_server::ChunkColumn,
    cx: i32,
    cz: i32,
    dimension: Dimension,
) -> Vec<u8> {
    let mut w = Writer::default();
    w.bytes(RECORD_DOMAIN_V7);
    w.i32(cx);
    w.i32(cz);
    w.bytes(dimension.name().as_bytes());

    let predicates = [1, 4, 5];
    w.i32(predicates.len() as i32);
    for type_id in predicates {
        w.i32(type_id as i32);
        for z in 0..16i32 {
            for x in 0..16i32 {
                let height = if let Some(map) = column
                    .client_heightmaps()
                    .and_then(|maps| maps.get(type_id))
                {
                    column.min_y + map.get(x as usize, z as usize) as i32
                } else {
                    (column.min_y..column.min_y + column.height)
                        .rev()
                        .find(|&y| lodestone_v26_2::server_protocol::client_heightmap_includes(type_id, column.resolved_block_state_id(x, y, z)))
                        .map_or(column.min_y, |y| y + 1)
                };
                w.i32(height);
            }
        }
    }

    let biome_ids = column
        .biome_cell_palette()
        .iter()
        .map(|name| lodestone_v26_2::server_protocol::biome_registry_id(name))
        .collect::<Vec<_>>();
    w.i32(column.section_count() as i32);
    for section in 0..column.section_count() {
        let base_y = column.min_y + (section * 16) as i32;
        for y in 0..16i32 {
            for z in 0..16i32 {
                for x in 0..16i32 {
                    w.u32(column.block_state_id(x, base_y + y, z));
                }
            }
        }
        for y in 0..4usize {
            for z in 0..4usize {
                for x in 0..4usize {
                    let index = column.biome_cell_index(x, section * 4 + y, z) as usize;
                    w.u32(biome_ids[index]);
                }
            }
        }
    }

    let mut entities = column.block_entities().to_vec();
    entities.sort_unstable_by_key(|(pos, entity)| {
        (
            pos.x.rem_euclid(16),
            pos.y,
            pos.z.rem_euclid(16),
            lodestone_data::block_entity_types::block_entity_type_id(entity.type_id())
                .map_or(u32::MAX, |id| id.raw()),
        )
    });
    w.i32(entities.len() as i32);
    for (pos, entity) in entities {
        w.u8(pos.x.rem_euclid(16) as u8);
        w.i16(pos.y as i16);
        w.u8(pos.z.rem_euclid(16) as u8);
        let type_id = lodestone_data::block_entity_types::block_entity_type_id(entity.type_id())
            .unwrap_or_else(|| panic!("unknown generated block entity type {}", entity.type_id()));
        w.u32(type_id.raw());
        canonical_nbt(&mut w, &lodestone_server::chunk_nbt::block_entity_update_nbt(pos, &entity));
    }
    w.as_slice().to_vec()
}

fn canonical_light(w: &mut Writer, light: &lodestone_world::ColumnLight, sky: bool) {
    for section in 0..light.light_section_count() {
        let value = if sky { light.sky(section) } else { light.block(section) };
        match value {
            LightData::Missing => w.u8(0),
            LightData::Uniform(0) => w.u8(1),
            LightData::Uniform(value) => { w.u8(2); w.bytes(&[*value | (*value << 4); 2048]); }
            LightData::Values(values) => { w.u8(2); w.bytes(values.as_bytes()); }
        }
    }
}

fn full_sky(value: &LightData) -> bool {
    match value {
        LightData::Uniform(15) => true,
        LightData::Values(values) => values.as_bytes().iter().all(|value| *value == 0xff),
        LightData::Missing | LightData::Uniform(_) => false,
    }
}

fn canonical_light_v5(w: &mut Writer, light: &lodestone_world::ColumnLight, sky: bool) {
    let mut last_required = light.light_section_count();
    if sky {
        while last_required != 0 {
            let value = light.sky(last_required - 1);
            if matches!(value, LightData::Missing) || full_sky(value) { last_required -= 1; }
            else { break; }
        }
    }
    for section in 0..light.light_section_count() {
        let value = if sky { light.sky(section) } else { light.block(section) };
        if sky && section >= last_required && full_sky(value) {
            w.u8(0);
            continue;
        }
        match value {
            LightData::Missing => w.u8(0),
            LightData::Uniform(0) => w.u8(1),
            LightData::Uniform(value) => { w.u8(2); w.bytes(&[*value | (*value << 4); 2048]); }
            LightData::Values(values) => { w.u8(2); w.bytes(values.as_bytes()); }
        }
    }
}

pub(crate) fn canonical_nbt(w: &mut Writer, value: &Nbt) {
    match value {
        Nbt::End => w.u8(0),
        Nbt::Byte(value) => { w.u8(1); w.i8(*value); }
        Nbt::Short(value) => { w.u8(2); w.i16(*value); }
        Nbt::Int(value) => { w.u8(3); w.i32(*value); }
        Nbt::Long(value) => { w.u8(4); w.i64(*value); }
        Nbt::Float(value) => { w.u8(5); w.u32(value.to_bits()); }
        Nbt::Double(value) => { w.u8(6); w.u64(value.to_bits()); }
        Nbt::ByteArray(values) => { w.u8(7); w.i32(values.len() as i32); for value in values { w.i8(*value); } }
        Nbt::String(value) => { w.u8(8); canonical_utf8(w, value); }
        Nbt::List { elements, .. } => { w.u8(9); w.i32(elements.len() as i32); for value in elements { canonical_nbt(w, value); } }
        Nbt::Compound(fields) => { w.u8(10); let mut fields = fields.iter().collect::<Vec<_>>(); fields.sort_unstable_by(|a, b| a.0.cmp(&b.0)); w.i32(fields.len() as i32); for (name, value) in fields { canonical_utf8(w, name); canonical_nbt(w, value); } }
        Nbt::IntArray(values) => { w.u8(11); w.i32(values.len() as i32); for value in values { w.i32(*value); } }
        Nbt::LongArray(values) => { w.u8(12); w.i32(values.len() as i32); for value in values { w.i64(*value); } }
    }
}

fn canonical_utf8(w: &mut Writer, value: &str) { w.i32(value.len() as i32); w.bytes(value.as_bytes()); }

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_world::{ColumnLight, NibbleArray};
    use std::io::Cursor;

    fn encoded_light(light: &ColumnLight, sky: bool) -> Vec<u8> {
        let mut writer = Writer::default();
        canonical_light_v5(&mut writer, light, sky);
        writer.into_vec()
    }

    #[test]
    fn v5_elides_only_the_redundant_full_sky_tail() {
        let missing = ColumnLight::new(0);
        let mut full = ColumnLight::new(0);
        *full.sky_mut(1) = LightData::Uniform(15);
        assert_eq!(encoded_light(&missing, true), encoded_light(&full, true));
        assert_eq!(encoded_light(&full, true).len(), 2, "full sky tail remains compact");

        let mut near_full = ColumnLight::new(0);
        let mut bytes = [0xff; 2048];
        bytes[1487] = 0xef;
        *near_full.sky_mut(1) = LightData::Values(NibbleArray::from_bytes(&bytes).unwrap());
        assert_ne!(encoded_light(&near_full, true), encoded_light(&full, true), "a real sky value must survive normalization");
    }

    #[test]
    fn v5_preserves_block_light_representations() {
        let missing = ColumnLight::new(0);
        let mut empty = ColumnLight::new(0);
        *empty.block_mut(0) = LightData::Uniform(0);
        let mut full = ColumnLight::new(0);
        *full.block_mut(0) = LightData::Uniform(15);
        assert_ne!(encoded_light(&missing, false), encoded_light(&empty, false));
        assert_ne!(encoded_light(&missing, false), encoded_light(&full, false));
    }

    #[test]
    fn v6_accepts_raw_packet_hash_geometry_and_streams_two_byte_records() {
        let mut header = [0u8; HEADER_BYTES];
        header[..8].copy_from_slice(b"LWP26P06");
        header[8..10].copy_from_slice(&6u16.to_be_bytes());
        header[10..12].copy_from_slice(&(HEADER_BYTES as u16).to_be_bytes());
        header[12..14].copy_from_slice(&2u16.to_be_bytes());
        header[14..16].copy_from_slice(&6u16.to_be_bytes());
        header[16..20].copy_from_slice(&776u32.to_be_bytes());
        header[20..28].copy_from_slice(&42i64.to_be_bytes());
        for (offset, value) in [(28, RAW_GRID_MIN), (32, RAW_GRID_MAX), (36, RAW_GRID_MIN), (40, RAW_GRID_MAX), (44, 0), (48, 0), (52, 0), (56, 0)] {
            header[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
        }
        header[60..68].copy_from_slice(&1u64.to_be_bytes());
        header[68..70].copy_from_slice(&(RAW_PACKET_HASH_BYTES as u16).to_be_bytes());
        header[72..104].copy_from_slice(&sha256(DOMAIN_V6));
        header[104..136].copy_from_slice(&[9; 32]);
        header[168..200].copy_from_slice(&sha256(b"minecraft:the_end"));
        let payload = [0x12, 0xA4];
        header[136..168].copy_from_slice(&sha256(&payload));
        let parsed = read_header(&header[..]).expect("v6 raw packet header");
        assert_eq!((parsed.kind, parsed.record_width, parsed.dimension), (2, 2, Dimension::End));
        verify_manifest_payload(&payload[..], &parsed, sha256(&payload)).expect("v6 payload stream");
        assert_eq!(raw_packet_hash(b"packet body"), { let digest = sha256(b"packet body"); [digest[0], digest[1]] });
        assert_eq!(hash_exact_chunk_with_light_body(b"packet body"), raw_packet_hash(b"packet body"));
        let mut wrong = header;
        wrong[104..136].fill(0);
        assert!(read_header(&wrong[..]).is_err(), "zero frozen-world identity must fail closed");
        let mut wrong_scope = header;
        wrong_scope[70..72].copy_from_slice(&1u16.to_be_bytes());
        assert!(read_header(&wrong_scope[..]).is_err(), "empty structure-beard scope must fail closed for an authenticated production manifest");
    }

    #[test]
    fn v7_light_free_header_and_full_digest_sidecar_are_authenticated() {
        let full = sha256(b"light-free content record");
        let prefix = [full[0], full[1]];
        let frozen = [0x5a; 32];
        let mut main = [0u8; HEADER_BYTES];
        main[..8].copy_from_slice(MAGIC_V7);
        main[8..10].copy_from_slice(&7u16.to_be_bytes());
        main[10..12].copy_from_slice(&(HEADER_BYTES as u16).to_be_bytes());
        main[12..14].copy_from_slice(&MANIFEST_KIND.to_be_bytes());
        main[14..16].copy_from_slice(&7u16.to_be_bytes());
        main[16..20].copy_from_slice(&776u32.to_be_bytes());
        main[20..28].copy_from_slice(&42i64.to_be_bytes());
        for (offset, value) in [(28, RAW_GRID_MIN), (32, RAW_GRID_MAX), (36, RAW_GRID_MIN), (40, RAW_GRID_MAX), (44, 0), (48, 0), (52, 0), (56, 0)] {
            main[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
        }
        main[60..68].copy_from_slice(&1u64.to_be_bytes());
        main[68..70].copy_from_slice(&(RAW_PACKET_HASH_BYTES as u16).to_be_bytes());
        main[72..104].copy_from_slice(&sha256(DOMAIN_V7));
        main[104..136].copy_from_slice(&frozen);
        main[136..168].copy_from_slice(&sha256(&prefix));
        main[168..200].copy_from_slice(&Dimension::Overworld.digest());
        let parsed = read_header(&main[..]).expect("v7 light-free header");
        assert_eq!((parsed.semantic_version, parsed.record_width, parsed.dimension), (7, 2, Dimension::Overworld));

        let mut audit = [0u8; HEADER_BYTES];
        audit[..8].copy_from_slice(LIGHT_FREE_AUDIT_MAGIC);
        audit[8..10].copy_from_slice(&7u16.to_be_bytes());
        audit[10..12].copy_from_slice(&(HEADER_BYTES as u16).to_be_bytes());
        audit[12..14].copy_from_slice(&PACKET_AUDIT_KIND.to_be_bytes());
        audit[14..16].copy_from_slice(&7u16.to_be_bytes());
        audit[16..20].copy_from_slice(&776u32.to_be_bytes());
        audit[20..28].copy_from_slice(&42i64.to_be_bytes());
        for (offset, value) in [(28, RAW_GRID_MIN), (32, RAW_GRID_MAX), (36, RAW_GRID_MIN), (40, RAW_GRID_MAX), (44, 0), (48, 0), (52, 0), (56, 0)] {
            audit[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
        }
        audit[60..68].copy_from_slice(&1u64.to_be_bytes());
        audit[68..70].copy_from_slice(&(LIGHT_FREE_AUDIT_RECORD_BYTES as u16).to_be_bytes());
        audit[72..104].copy_from_slice(&sha256(LIGHT_FREE_AUDIT_DOMAIN));
        audit[104..136].copy_from_slice(&frozen);
        audit[136..168].copy_from_slice(&sha256(&full));
        audit[168..200].copy_from_slice(&Dimension::Overworld.digest());
        let parsed_audit = read_light_free_audit_header(&audit[..]).expect("v7 light-free audit header");
        validate_light_free_audit_header(&parsed, &parsed_audit).expect("v7 sidecar identity");
        verify_light_free_audit_pair(Cursor::new(prefix), Cursor::new(full), 1, sha256(&prefix), sha256(&full)).expect("v7 prefix/full pair");
        let mut wrong_prefix = prefix;
        wrong_prefix[0] ^= 1;
        assert!(verify_light_free_audit_pair(Cursor::new(wrong_prefix), Cursor::new(full), 1, sha256(&wrong_prefix), sha256(&full)).is_err(), "v7 prefix collision control must fail");
    }

    fn test_v6_header(
        magic: &[u8; 8],
        kind: u16,
        width: u16,
        schema: &[u8],
        frozen: [u8; 32],
        payload_digest: [u8; 32],
    ) -> [u8; HEADER_BYTES] {
        let mut header = [0u8; HEADER_BYTES];
        header[..8].copy_from_slice(magic);
        header[8..10].copy_from_slice(&6u16.to_be_bytes());
        header[10..12].copy_from_slice(&(HEADER_BYTES as u16).to_be_bytes());
        header[12..14].copy_from_slice(&kind.to_be_bytes());
        header[14..16].copy_from_slice(&6u16.to_be_bytes());
        header[16..20].copy_from_slice(&776u32.to_be_bytes());
        header[20..28].copy_from_slice(&42i64.to_be_bytes());
        for (offset, value) in [
            (28, RAW_GRID_MIN),
            (32, RAW_GRID_MAX),
            (36, RAW_GRID_MIN),
            (40, RAW_GRID_MAX),
            (44, 0),
            (48, 0),
            (52, 0),
            (56, 0),
        ] {
            header[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
        }
        header[60..68].copy_from_slice(&1u64.to_be_bytes());
        header[68..70].copy_from_slice(&width.to_be_bytes());
        header[72..104].copy_from_slice(&sha256(schema));
        header[104..136].copy_from_slice(&frozen);
        header[136..168].copy_from_slice(&payload_digest);
        header[168..200].copy_from_slice(&Dimension::Nether.digest());
        header
    }

    #[test]
    fn v6_packet_audit_header_and_prefix_pair_are_authenticated() {
        let full = sha256(b"packet body");
        let prefix = [full[0], full[1]];
        let main_payload = prefix.to_vec();
        let audit_payload = full.to_vec();
        let frozen = [0x4b; 32];
        let main_bytes = test_v6_header(
            MAGIC_V6,
            MANIFEST_KIND,
            RAW_PACKET_HASH_BYTES as u16,
            DOMAIN_V6,
            frozen,
            sha256(&main_payload),
        );
        let audit_bytes = test_v6_header(
            PACKET_AUDIT_MAGIC,
            PACKET_AUDIT_KIND,
            PACKET_AUDIT_RECORD_BYTES as u16,
            PACKET_AUDIT_DOMAIN,
            frozen,
            sha256(&audit_payload),
        );
        let main = read_header(&main_bytes[..]).expect("main v6 header");
        let audit = read_packet_audit_header(&audit_bytes[..]).expect("packet-audit header");
        validate_packet_audit_header(&main, &audit).expect("sidecar must match main identity");
        verify_raw_packet_audit_pair(
            Cursor::new(main_payload),
            Cursor::new(audit_payload),
            1,
            sha256(&prefix),
            sha256(&full),
        )
        .expect("matching v6 prefix/full streams");

        let mut wrong_prefix = prefix;
        wrong_prefix[0] ^= 1;
        let error = verify_raw_packet_audit_pair(
            Cursor::new(wrong_prefix),
            Cursor::new(full),
            1,
            sha256(&wrong_prefix),
            sha256(&full),
        )
        .expect_err("a sidecar prefix collision pair must be rejected");
        assert!(error.to_string().contains("prefix differs"));
    }

    #[test]
    fn v6_header_rejects_extreme_shard_arithmetic_without_panicking() {
        let mut header = test_v6_header(
            MAGIC_V6,
            MANIFEST_KIND,
            RAW_PACKET_HASH_BYTES as u16,
            DOMAIN_V6,
            [0x23; 32],
            [0; 32],
        );
        header[44..48].copy_from_slice(&i32::MIN.to_be_bytes());
        header[48..52].copy_from_slice(&i32::MAX.to_be_bytes());
        header[52..56].copy_from_slice(&i32::MIN.to_be_bytes());
        header[56..60].copy_from_slice(&i32::MAX.to_be_bytes());
        let result = std::panic::catch_unwind(|| read_header(&header[..]));
        assert!(result.is_ok(), "malformed shard arithmetic must not panic");
        assert!(result.unwrap().is_err(), "extreme coordinates must be rejected");
    }

    #[test]
    fn sha256_incremental_two_byte_updates_match_contiguous_payload() {
        for length in 0..=5202 {
            let payload = (0..length).map(|value| value as u8).collect::<Vec<_>>();
            let mut incremental = Sha256::new();
            for record in payload.chunks_exact(RAW_PACKET_HASH_BYTES) {
                incremental.update(record);
            }
            assert_eq!(
                incremental.finish(),
                sha256(&payload[..payload.len() / RAW_PACKET_HASH_BYTES * RAW_PACKET_HASH_BYTES]),
                "incremental checksum diverged at {length} bytes"
            );
        }
    }
}

/// Incremental SHA-256 used by provenance checks that must not materialize a
/// frozen world's complete file tree in memory.
pub struct IncrementalSha256(Sha256);

impl IncrementalSha256 {
    #[must_use]
    pub fn new() -> Self { Self(Sha256::new()) }

    pub fn update(&mut self, input: &[u8]) { self.0.update(input); }

    #[must_use]
    pub fn finish(self) -> [u8; 32] { self.0.finish() }
}

pub fn sha256(input: &[u8]) -> [u8; 32] { let mut s = Sha256::new(); s.update(input); s.finish() }
struct Sha256 { state: [u32; 8], len: u64, buf: [u8; 64], used: usize }
impl Sha256 {
    fn new() -> Self { Self { state: [0x6a09e667,0xbb67ae85,0x3c6ef372,0xa54ff53a,0x510e527f,0x9b05688c,0x1f83d9ab,0x5be0cd19], len: 0, buf: [0;64], used: 0 } }
    fn update(&mut self, mut data: &[u8]) {
        self.len += data.len() as u64;
        if self.used != 0 {
            let n = (64 - self.used).min(data.len());
            self.buf[self.used..self.used + n].copy_from_slice(&data[..n]);
            self.used += n;
            data = &data[n..];
            if self.used == 64 {
                Self::block(&mut self.state, &self.buf);
                self.used = 0;
            }
        }
        while data.len() >= 64 {
            Self::block(&mut self.state, data[..64].try_into().unwrap());
            data = &data[64..];
        }
        if !data.is_empty() {
            self.buf[..data.len()].copy_from_slice(data);
            self.used = data.len();
        }
    }
    fn finish(mut self) -> [u8;32] { let bits=self.len*8; self.buf[self.used]=0x80; self.used+=1; if self.used>56 { self.buf[self.used..].fill(0); Self::block(&mut self.state,&self.buf); self.used=0; } self.buf[self.used..56].fill(0); self.buf[56..].copy_from_slice(&bits.to_be_bytes()); Self::block(&mut self.state,&self.buf); let mut out=[0;32]; for (i,v) in self.state.iter().enumerate(){out[i*4..i*4+4].copy_from_slice(&v.to_be_bytes());} out }
    fn block(s: &mut [u32;8], b: &[u8;64]) { const K:[u32;64]=[0x428a2f98,0x71374491,0xb5c0fbcf,0xe9b5dba5,0x3956c25b,0x59f111f1,0x923f82a4,0xab1c5ed5,0xd807aa98,0x12835b01,0x243185be,0x550c7dc3,0x72be5d74,0x80deb1fe,0x9bdc06a7,0xc19bf174,0xe49b69c1,0xefbe4786,0x0fc19dc6,0x240ca1cc,0x2de92c6f,0x4a7484aa,0x5cb0a9dc,0x76f988da,0x983e5152,0xa831c66d,0xb00327c8,0xbf597fc7,0xc6e00bf3,0xd5a79147,0x06ca6351,0x14292967,0x27b70a85,0x2e1b2138,0x4d2c6dfc,0x53380d13,0x650a7354,0x766a0abb,0x81c2c92e,0x92722c85,0xa2bfe8a1,0xa81a664b,0xc24b8b70,0xc76c51a3,0xd192e819,0xd6990624,0xf40e3585,0x106aa070,0x19a4c116,0x1e376c08,0x2748774c,0x34b0bcb5,0x391c0cb3,0x4ed8aa4a,0x5b9cca4f,0x682e6ff3,0x748f82ee,0x78a5636f,0x84c87814,0x8cc70208,0x90befffa,0xa4506ceb,0xbef9a3f7,0xc67178f2]; let mut w=[0u32;64]; for i in 0..16{w[i]=u32::from_be_bytes(b[i*4..i*4+4].try_into().unwrap());} for i in 16..64 {w[i]=w[i-16].wrapping_add(w[i-15].rotate_right(7)^w[i-15].rotate_right(18)^(w[i-15]>>3)).wrapping_add(w[i-7]).wrapping_add(w[i-2].rotate_right(17)^w[i-2].rotate_right(19)^(w[i-2]>>10));} let(mut a,mut b0,mut c,mut d,mut e,mut f,mut g,mut h)=(s[0],s[1],s[2],s[3],s[4],s[5],s[6],s[7]); for i in 0..64 {let t1=h.wrapping_add(e.rotate_right(6)^e.rotate_right(11)^e.rotate_right(25)).wrapping_add((e&f)^(!e&g)).wrapping_add(K[i]).wrapping_add(w[i]);let t2=(a.rotate_right(2)^a.rotate_right(13)^a.rotate_right(22)).wrapping_add((a&b0)^(a&c)^(b0&c));h=g;g=f;f=e;e=d.wrapping_add(t1);d=c;c=b0;b0=a;a=t1.wrapping_add(t2);} s[0]=s[0].wrapping_add(a);s[1]=s[1].wrapping_add(b0);s[2]=s[2].wrapping_add(c);s[3]=s[3].wrapping_add(d);s[4]=s[4].wrapping_add(e);s[5]=s[5].wrapping_add(f);s[6]=s[6].wrapping_add(g);s[7]=s[7].wrapping_add(h); }
}
