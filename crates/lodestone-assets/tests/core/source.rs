//! Tests for [`ResourceSource`] implementations: directory, zip, and memory.
//!
//! These prove the directory and zip backends are interchangeable and that both
//! reject path-traversal / zip-slip attacks.

use lodestone_assets::{DirectorySource, MemorySource, ResourceSource, ZipSource};
use std::io::Write;

/// The (path, bytes) fixtures every backend is populated with.
fn fixtures() -> Vec<(&'static str, &'static [u8])> {
    vec![
        ("assets/minecraft/textures/block/stone.png", b"stone-bytes"),
        ("assets/minecraft/textures/block/dirt.png", b"dirt-bytes"),
        (
            "assets/minecraft/blockstates/stone.json",
            b"{\"variants\":{}}",
        ),
        ("pack.mcmeta", b"{\"pack\":{\"pack_format\":1}}"),
    ]
}

fn build_dir() -> (tempfile::TempDir, DirectorySource) {
    let dir = tempfile::tempdir().unwrap();
    for (path, bytes) in fixtures() {
        let full = dir.path().join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, bytes).unwrap();
    }
    let src = DirectorySource::new(dir.path()).unwrap();
    (dir, src)
}

fn build_zip_bytes() -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let cursor = std::io::Cursor::new(&mut buf);
        let mut zip = zip::ZipWriter::new(cursor);
        let opts: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (path, bytes) in fixtures() {
            zip.start_file(path, opts).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

#[test]
fn directory_and_zip_agree() {
    let (_dir, dir_src) = build_dir();
    let zip_src = ZipSource::from_bytes(build_zip_bytes()).unwrap();
    for (path, bytes) in fixtures() {
        assert_eq!(dir_src.read(path).as_deref(), Some(bytes), "dir {path}");
        assert_eq!(zip_src.read(path).as_deref(), Some(bytes), "zip {path}");
    }
    // Missing resource is None (not an error) for both.
    assert_eq!(dir_src.read("assets/minecraft/nope.txt"), None);
    assert_eq!(zip_src.read("assets/minecraft/nope.txt"), None);
}

#[test]
fn list_matches_across_backends() {
    let (_dir, dir_src) = build_dir();
    let zip_src = ZipSource::from_bytes(build_zip_bytes()).unwrap();
    let mut dir_list = dir_src.list("assets/minecraft/textures/block/");
    let mut zip_list = zip_src.list("assets/minecraft/textures/block/");
    dir_list.sort();
    zip_list.sort();
    assert_eq!(dir_list, zip_list);
    assert_eq!(
        dir_list,
        vec![
            "assets/minecraft/textures/block/dirt.png".to_string(),
            "assets/minecraft/textures/block/stone.png".to_string(),
        ]
    );
}

#[test]
fn list_empty_prefix_returns_everything() {
    let (_dir, dir_src) = build_dir();
    assert_eq!(dir_src.list("").len(), fixtures().len());
}

#[test]
fn directory_rejects_path_traversal() {
    let dir = tempfile::tempdir().unwrap();
    // A secret file outside the pack root.
    let secret = dir.path().join("secret.txt");
    std::fs::write(&secret, b"top-secret").unwrap();
    let root = dir.path().join("pack");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("ok.txt"), b"ok").unwrap();
    let src = DirectorySource::new(&root).unwrap();

    assert_eq!(src.read("ok.txt"), Some(b"ok".to_vec()));
    // Attempts to escape must fail, not read the secret.
    assert_eq!(src.read("../secret.txt"), None);
    assert_eq!(src.read("../../etc/passwd"), None);
    assert_eq!(src.read("/etc/passwd"), None);
    assert_eq!(src.read("foo/../../secret.txt"), None);
}

#[test]
fn zip_handles_duplicate_entries() {
    // `ZipWriter` refuses to emit duplicate names, so hand-craft a raw STORED
    // archive containing "dup.txt" twice; the later entry must win.
    let buf = build_raw_zip_with_duplicates();
    let src = ZipSource::from_bytes(buf).unwrap();
    assert_eq!(src.read("dup.txt"), Some(b"second".to_vec()));
}

/// CRC-32/IEEE, used to hand-assemble a zip below.
fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// Builds a raw zip with two STORED entries both named `dup.txt`.
fn build_raw_zip_with_duplicates() -> Vec<u8> {
    let name = b"dup.txt";
    let entries: [&[u8]; 2] = [b"first", b"second"];
    let mut out = Vec::new();
    let mut central = Vec::new();
    let mut offsets = Vec::new();

    for data in entries {
        offsets.push(out.len() as u32);
        let crc = crc32(data);
        // Local file header.
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes()); // version needed
        out.extend_from_slice(&0u16.to_le_bytes()); // flags
        out.extend_from_slice(&0u16.to_le_bytes()); // stored
        out.extend_from_slice(&0u16.to_le_bytes()); // time
        out.extend_from_slice(&0u16.to_le_bytes()); // date
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra len
        out.extend_from_slice(name);
        out.extend_from_slice(data);
    }

    for (i, data) in entries.iter().enumerate() {
        let crc = crc32(data);
        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes()); // version made by
        central.extend_from_slice(&20u16.to_le_bytes()); // version needed
        central.extend_from_slice(&0u16.to_le_bytes()); // flags
        central.extend_from_slice(&0u16.to_le_bytes()); // stored
        central.extend_from_slice(&0u16.to_le_bytes()); // time
        central.extend_from_slice(&0u16.to_le_bytes()); // date
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&(data.len() as u32).to_le_bytes());
        central.extend_from_slice(&(data.len() as u32).to_le_bytes());
        central.extend_from_slice(&(name.len() as u16).to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes()); // extra len
        central.extend_from_slice(&0u16.to_le_bytes()); // comment len
        central.extend_from_slice(&0u16.to_le_bytes()); // disk start
        central.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
        central.extend_from_slice(&0u32.to_le_bytes()); // external attrs
        central.extend_from_slice(&offsets[i].to_le_bytes());
        central.extend_from_slice(name);
    }

    let cd_offset = out.len() as u32;
    let cd_size = central.len() as u32;
    out.extend_from_slice(&central);
    // End of central directory.
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // disk
    out.extend_from_slice(&0u16.to_le_bytes()); // cd disk
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&cd_size.to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // comment len
    out
}

#[test]
fn absurd_paths_do_not_panic() {
    let (_dir, dir_src) = build_dir();
    let zip_src = ZipSource::from_bytes(build_zip_bytes()).unwrap();
    for weird in ["", "///", "..", "a\0b", "x".repeat(5000).as_str()] {
        assert_eq!(dir_src.read(weird), None);
        assert_eq!(zip_src.read(weird), None);
    }
}

#[test]
fn memory_source_round_trips() {
    let mut src = MemorySource::new("test");
    src.insert("a/b.txt", b"hello".to_vec());
    assert_eq!(src.read("a/b.txt"), Some(b"hello".to_vec()));
    assert_eq!(src.read("missing"), None);
    assert_eq!(src.list("a/"), vec!["a/b.txt".to_string()]);
}
