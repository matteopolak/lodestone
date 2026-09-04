//! Regression: `ZipSource::read` must not drive an unbounded allocation off
//! a zip entry's own declared (and unverified) uncompressed-size field.
//!
//! `fuzz/fuzz_targets/resource_pack_zip_source.rs` found this directly: one
//! execution against a hand-crafted entry declaring ~4 GiB uncompressed while
//! holding four real bytes aborted the process (libFuzzer reported
//! `out-of-memory (malloc(4294967294))`, i.e. an allocator abort, not a Rust
//! panic) before a single byte was read or the entry's own CRC-32 checked.
//! `read`'s `Vec::with_capacity(entry.size() as usize)` took that field at
//! face value; `size()` is read straight out of the archive's local/central
//! header and is never cross-checked against anything before this call.
//!
//! A resource pack is exactly this kind of untrusted input: a server names a
//! URL and a hash via `minecraft:resource_pack_push`, and the archive that
//! comes back is parsed entirely client-side.
//!
//! Hand-built rather than written through `zip::ZipWriter`, because that
//! writer always emits a size matching what it actually wrote — reproducing
//! the lie needs a local header and central directory record whose declared
//! uncompressed-size field disagrees with the real payload, which no honest
//! writer will produce.

use std::io::Write;

/// A table-free CRC-32 (ISO 3309 / zip's own polynomial), so a *not-lied*
/// entry's declared checksum is genuinely correct. Without this, `zip`'s own
/// reader rejects even the honest control on CRC mismatch, which would make
/// the control fail for a reason that has nothing to do with the declared
/// size — exactly the "the detector itself is untested" failure mode.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = 0u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// Builds a single-entry, STORE-method zip whose header and central
/// directory both declare `claimed_size` bytes uncompressed, while the body
/// is just `data` (much smaller). A real archive has `compressed_size ==
/// uncompressed_size` under STORE; this one deliberately lies about the
/// uncompressed size only — the CRC-32 is always the real one, so a
/// rejection is never mistaken for the size lie being caught some other way.
fn crafted_zip(name: &str, data: &[u8], claimed_size: u32) -> Vec<u8> {
    let mut out = Vec::new();
    let name_bytes = name.as_bytes();
    let local_header_offset = 0u32;
    let crc = crc32(data);

    // Local file header.
    out.write_all(&0x0403_4b50u32.to_le_bytes()).unwrap(); // signature
    out.write_all(&20u16.to_le_bytes()).unwrap(); // version needed
    out.write_all(&0u16.to_le_bytes()).unwrap(); // flags
    out.write_all(&0u16.to_le_bytes()).unwrap(); // method: store
    out.write_all(&0u16.to_le_bytes()).unwrap(); // mod time
    out.write_all(&0u16.to_le_bytes()).unwrap(); // mod date
    out.write_all(&crc.to_le_bytes()).unwrap(); // crc32: real, always
    out.write_all(&(data.len() as u32).to_le_bytes()).unwrap(); // compressed size: real
    out.write_all(&claimed_size.to_le_bytes()).unwrap(); // uncompressed size: LIE
    out.write_all(&(name_bytes.len() as u16).to_le_bytes()).unwrap();
    out.write_all(&0u16.to_le_bytes()).unwrap(); // extra length
    out.write_all(name_bytes).unwrap();
    out.write_all(data).unwrap();

    let cd_start = out.len();

    // Central directory file header.
    out.write_all(&0x0201_4b50u32.to_le_bytes()).unwrap();
    out.write_all(&20u16.to_le_bytes()).unwrap(); // version made by
    out.write_all(&20u16.to_le_bytes()).unwrap(); // version needed
    out.write_all(&0u16.to_le_bytes()).unwrap(); // flags
    out.write_all(&0u16.to_le_bytes()).unwrap(); // method
    out.write_all(&0u16.to_le_bytes()).unwrap(); // mod time
    out.write_all(&0u16.to_le_bytes()).unwrap(); // mod date
    out.write_all(&crc.to_le_bytes()).unwrap(); // crc32: real, always
    out.write_all(&(data.len() as u32).to_le_bytes()).unwrap(); // compressed size
    out.write_all(&claimed_size.to_le_bytes()).unwrap(); // uncompressed size: LIE
    out.write_all(&(name_bytes.len() as u16).to_le_bytes()).unwrap();
    out.write_all(&0u16.to_le_bytes()).unwrap(); // extra length
    out.write_all(&0u16.to_le_bytes()).unwrap(); // comment length
    out.write_all(&0u16.to_le_bytes()).unwrap(); // disk number start
    out.write_all(&0u16.to_le_bytes()).unwrap(); // internal attrs
    out.write_all(&0u32.to_le_bytes()).unwrap(); // external attrs
    out.write_all(&local_header_offset.to_le_bytes()).unwrap();
    out.write_all(name_bytes).unwrap();

    let cd_size = out.len() - cd_start;

    // End of central directory record.
    out.write_all(&0x0605_4b50u32.to_le_bytes()).unwrap();
    out.write_all(&0u16.to_le_bytes()).unwrap(); // disk number
    out.write_all(&0u16.to_le_bytes()).unwrap(); // disk with cd
    out.write_all(&1u16.to_le_bytes()).unwrap(); // entries this disk
    out.write_all(&1u16.to_le_bytes()).unwrap(); // entries total
    out.write_all(&(cd_size as u32).to_le_bytes()).unwrap();
    out.write_all(&(cd_start as u32).to_le_bytes()).unwrap();
    out.write_all(&0u16.to_le_bytes()).unwrap(); // comment length

    out
}

/// A sanity control: the same builder with an honest size must parse and
/// read back cleanly, so a failure below is about the lie, not about the
/// hand-built format being malformed in some unrelated way.
#[test]
fn an_honest_declared_size_round_trips() {
    use lodestone_assets::ResourceSource;
    let data = b"hello vanilla";
    let bytes = crafted_zip("assets/minecraft/lang/en_us.json", data, data.len() as u32);
    let source = lodestone_assets::ZipSource::from_bytes(bytes).expect("parse honest zip");
    let read = source
        .read("assets/minecraft/lang/en_us.json")
        .expect("entry present");
    assert_eq!(read, data);
}

/// The regression itself: a declared size of ~4 GiB over four real bytes
/// must read back as exactly those four bytes, not abort the process.
///
/// Before the fix this aborted (an allocator OOM, not a catchable panic) on
/// the very first call — the assertion below is what a fixed `read` owes the
/// caller once it stops trusting the declared size, not merely "returned
/// without crashing".
#[test]
fn a_lied_declared_size_of_four_gigabytes_reads_the_real_bytes() {
    use lodestone_assets::ResourceSource;
    let data = b"tiny";
    let bytes = crafted_zip("assets/minecraft/lang/en_us.json", data, u32::MAX - 1);
    let source = lodestone_assets::ZipSource::from_bytes(bytes).expect("parse zip with lied size");
    let read = source
        .read("assets/minecraft/lang/en_us.json")
        .expect("entry present despite the lied size");
    assert_eq!(read, data, "the real four bytes, not the declared ~4 GiB");
}
