//! Manual million-chunk parity-manifest gate. It intentionally remains ignored:
//! a full 1001² run is an external oracle job, not a regular unit test.
mod support { pub mod large_parity_manifest; }

use std::{fs::File, io::{BufReader, Read, Seek, SeekFrom}};
use support::large_parity_manifest::{HEADER_BYTES, read_header, payload_digest_from_header, verify_payload};

#[test]
fn sha256_control_and_bit_flip_are_detected() {
    use support::large_parity_manifest::sha256;
    assert_eq!(hex(&sha256(b"abc")), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad", "known external SHA-256 control");
    let payload = [1u8,2]; let good = sha256(&payload);
    verify_payload(&payload[..], 1, good).expect("control payload must authenticate");
    let mut corrupt = payload; corrupt[1] ^= 1;
    assert!(verify_payload(&corrupt[..], 1, good).is_err(), "one changed fingerprint bit must be detected");
}

/// Reads the manifest strictly sequentially and uses only one 2-byte expected
/// fingerprint at a time. Set `LODESTONE_LARGE_PARITY_MANIFEST` to a merged
/// 1001² manifest after the production packet encoder has a comparison seam.
#[test]
#[ignore = "manual external oracle comparison; see docs/worldgen-large-parity.md"]
fn full_grid_manifest_streams_before_rust_comparison() {
    let path = std::env::var("LODESTONE_LARGE_PARITY_MANIFEST").expect("set LODESTONE_LARGE_PARITY_MANIFEST=/absolute/path/to/merged.lwp");
    let mut raw_header = [0; HEADER_BYTES]; let mut f = File::open(&path).expect("open manifest"); f.read_exact(&mut raw_header).expect("read header");
    let h = read_header(&raw_header[..]).expect("valid required-grid header");
    assert_eq!((h.cx0,h.cx1,h.cz0,h.cz1,h.count), (-500,500,-500,500,1_002_001));
    let mut payload_file = File::open(&path).expect("reopen manifest"); payload_file.seek(SeekFrom::Start(HEADER_BYTES as u64)).expect("seek payload");
    verify_payload(BufReader::new(payload_file), h.count, payload_digest_from_header(&raw_header)).expect("payload integrity");
    panic!("manifest is authentic and stream-addressable, but this v26-2 test needs a narrow public production chunk-packet encoding seam; do not duplicate serialization in test support");
}
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() }
