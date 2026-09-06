//! Reader and semantic-record encoder for the frozen-world v3 large-parity
//! interchange format. This is test support rather than a game protocol.

use std::io::{self, Read};

use lodestone_core::{Nbt, Writer};
use lodestone_v26_2::packets::chunk::LevelChunkWithLight;
use lodestone_world::LightData;

pub const HEADER_BYTES: usize = 256;
const MAGIC: &[u8; 8] = b"LWP26P03";
const DOMAIN: &[u8] = b"lodestone.worldgen.large-parity.manifest/v3/semantic";
const RECORD_DOMAIN: &[u8] = b"lodestone.worldgen.large-parity.chunk/v3/semantic";
const DIGEST_BYTES: u64 = 32;

#[derive(Debug, Clone, Copy)]
pub struct Header {
    pub cx0: i32,
    pub cx1: i32,
    pub cz0: i32,
    pub cz1: i32,
    pub count: u64,
    pub frozen_world: [u8; 32],
}

fn invalid(message: &'static str) -> io::Error { io::Error::new(io::ErrorKind::InvalidData, message) }
fn be_i32(b: &[u8]) -> i32 { i32::from_be_bytes(b.try_into().expect("fixed-width header field")) }
fn be_u64(b: &[u8]) -> u64 { u64::from_be_bytes(b.try_into().expect("fixed-width header field")) }

/// Reads a v3 header. A v2 file is rejected explicitly: its hashes describe
/// non-canonical packet bytes and cannot be upgraded without re-exporting.
pub fn read_header(mut r: impl Read) -> io::Result<Header> {
    let mut b = [0; HEADER_BYTES]; r.read_exact(&mut b)?;
    if &b[..8] == b"LWP26P02" { return Err(invalid("v2 raw-packet manifest is rejected; regenerate frozen-world semantic v3")); }
    if &b[..8] != MAGIC || u16::from_be_bytes(b[8..10].try_into().unwrap()) != 3 || u16::from_be_bytes(b[10..12].try_into().unwrap()) != HEADER_BYTES as u16 || u16::from_be_bytes(b[12..14].try_into().unwrap()) != 2 || u16::from_be_bytes(b[14..16].try_into().unwrap()) != 3 || u32::from_be_bytes(b[16..20].try_into().unwrap()) != 776 || i64::from_be_bytes(b[20..28].try_into().unwrap()) != 42 || u16::from_be_bytes(b[68..70].try_into().unwrap()) != DIGEST_BYTES as u16 || u16::from_be_bytes(b[70..72].try_into().unwrap()) != 0 {
        return Err(invalid("unsupported large-parity v3 header"));
    }
    if b[72..104] != sha256(DOMAIN) { return Err(invalid("large-parity semantic schema digest differs")); }
    let frozen_world: [u8; 32] = b[104..136].try_into().unwrap();
    if frozen_world == [0; 32] { return Err(invalid("large-parity v3 manifest has no frozen-world identity")); }
    if (be_i32(&b[28..32]), be_i32(&b[32..36]), be_i32(&b[36..40]), be_i32(&b[40..44])) != (-500, 500, -500, 500) { return Err(invalid("manifest global bounds differ")); }
    let h = Header { cx0: be_i32(&b[44..48]), cx1: be_i32(&b[48..52]), cz0: be_i32(&b[52..56]), cz1: be_i32(&b[56..60]), count: be_u64(&b[60..68]), frozen_world };
    let expected = (i64::from(h.cx1 - h.cx0 + 1) * i64::from(h.cz1 - h.cz0 + 1)) as u64;
    if !(h.cx0 >= -500 && h.cx1 <= 500 && h.cz0 >= -500 && h.cz1 <= 500 && h.cx0 <= h.cx1 && h.cz0 <= h.cz1 && h.count == expected) { return Err(invalid("invalid large-parity shard bounds")); }
    Ok(h)
}

/// Streams full 32-byte semantic digests through SHA-256 before comparison.
pub fn verify_payload(mut r: impl Read, count: u64, expected: [u8; 32]) -> io::Result<()> {
    let mut sha = Sha256::new(); let mut left = count.checked_mul(DIGEST_BYTES).ok_or_else(|| invalid("payload count overflow"))?; let mut buf = [0u8; 8192];
    while left != 0 { let n = usize::try_from(left.min(buf.len() as u64)).unwrap(); r.read_exact(&mut buf[..n])?; sha.update(&buf[..n]); left -= n as u64; }
    let mut trailing = [0u8; 1]; if r.read(&mut trailing)? != 0 { return Err(invalid("large-parity payload has trailing bytes")); }
    if sha.finish() != expected { return Err(invalid("large-parity payload checksum differs")); }
    Ok(())
}

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

fn canonical_light(w: &mut Writer, light: &lodestone_world::ColumnLight, sky: bool) {
    for section in 0..26 {
        let value = if sky { light.sky(section) } else { light.block(section) };
        match value {
            LightData::Missing => w.u8(0),
            LightData::Uniform(0) => w.u8(1),
            LightData::Uniform(value) => { w.u8(2); w.bytes(&[*value | (*value << 4); 2048]); }
            LightData::Values(values) => { w.u8(2); w.bytes(values.as_bytes()); }
        }
    }
}

fn canonical_nbt(w: &mut Writer, value: &Nbt) {
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

pub fn sha256(input: &[u8]) -> [u8; 32] { let mut s = Sha256::new(); s.update(input); s.finish() }
struct Sha256 { state: [u32; 8], len: u64, buf: [u8; 64], used: usize }
impl Sha256 {
    fn new() -> Self { Self { state: [0x6a09e667,0xbb67ae85,0x3c6ef372,0xa54ff53a,0x510e527f,0x9b05688c,0x1f83d9ab,0x5be0cd19], len: 0, buf: [0;64], used: 0 } }
    fn update(&mut self, mut data: &[u8]) { self.len += data.len() as u64; if self.used != 0 { let n=(64-self.used).min(data.len()); self.buf[self.used..self.used+n].copy_from_slice(&data[..n]); self.used+=n; data=&data[n..]; if self.used==64 { Self::block(&mut self.state,&self.buf); self.used=0; } } while data.len()>=64 { Self::block(&mut self.state, data[..64].try_into().unwrap()); data=&data[64..]; } self.buf[..data.len()].copy_from_slice(data); self.used=data.len(); }
    fn finish(mut self) -> [u8;32] { let bits=self.len*8; self.buf[self.used]=0x80; self.used+=1; if self.used>56 { self.buf[self.used..].fill(0); Self::block(&mut self.state,&self.buf); self.used=0; } self.buf[self.used..56].fill(0); self.buf[56..].copy_from_slice(&bits.to_be_bytes()); Self::block(&mut self.state,&self.buf); let mut out=[0;32]; for (i,v) in self.state.iter().enumerate(){out[i*4..i*4+4].copy_from_slice(&v.to_be_bytes());} out }
    fn block(s: &mut [u32;8], b: &[u8;64]) { const K:[u32;64]=[0x428a2f98,0x71374491,0xb5c0fbcf,0xe9b5dba5,0x3956c25b,0x59f111f1,0x923f82a4,0xab1c5ed5,0xd807aa98,0x12835b01,0x243185be,0x550c7dc3,0x72be5d74,0x80deb1fe,0x9bdc06a7,0xc19bf174,0xe49b69c1,0xefbe4786,0x0fc19dc6,0x240ca1cc,0x2de92c6f,0x4a7484aa,0x5cb0a9dc,0x76f988da,0x983e5152,0xa831c66d,0xb00327c8,0xbf597fc7,0xc6e00bf3,0xd5a79147,0x06ca6351,0x14292967,0x27b70a85,0x2e1b2138,0x4d2c6dfc,0x53380d13,0x650a7354,0x766a0abb,0x81c2c92e,0x92722c85,0xa2bfe8a1,0xa81a664b,0xc24b8b70,0xc76c51a3,0xd192e819,0xd6990624,0xf40e3585,0x106aa070,0x19a4c116,0x1e376c08,0x2748774c,0x34b0bcb5,0x391c0cb3,0x4ed8aa4a,0x5b9cca4f,0x682e6ff3,0x748f82ee,0x78a5636f,0x84c87814,0x8cc70208,0x90befffa,0xa4506ceb,0xbef9a3f7,0xc67178f2]; let mut w=[0u32;64]; for i in 0..16{w[i]=u32::from_be_bytes(b[i*4..i*4+4].try_into().unwrap());} for i in 16..64 {w[i]=w[i-16].wrapping_add(w[i-15].rotate_right(7)^w[i-15].rotate_right(18)^(w[i-15]>>3)).wrapping_add(w[i-7]).wrapping_add(w[i-2].rotate_right(17)^w[i-2].rotate_right(19)^(w[i-2]>>10));} let(mut a,mut b0,mut c,mut d,mut e,mut f,mut g,mut h)=(s[0],s[1],s[2],s[3],s[4],s[5],s[6],s[7]); for i in 0..64 {let t1=h.wrapping_add(e.rotate_right(6)^e.rotate_right(11)^e.rotate_right(25)).wrapping_add((e&f)^(!e&g)).wrapping_add(K[i]).wrapping_add(w[i]);let t2=(a.rotate_right(2)^a.rotate_right(13)^a.rotate_right(22)).wrapping_add((a&b0)^(a&c)^(b0&c));h=g;g=f;f=e;e=d.wrapping_add(t1);d=c;c=b0;b0=a;a=t1.wrapping_add(t2);} s[0]=s[0].wrapping_add(a);s[1]=s[1].wrapping_add(b0);s[2]=s[2].wrapping_add(c);s[3]=s[3].wrapping_add(d);s[4]=s[4].wrapping_add(e);s[5]=s[5].wrapping_add(f);s[6]=s[6].wrapping_add(g);s[7]=s[7].wrapping_add(h); }
}
