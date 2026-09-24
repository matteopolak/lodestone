//! `explode`'s `explosionParticle` field must be skippable for **any**
//! argument-less particle type, not just the two ids our own server happens to
//! send.
//!
//! The owner hit this on a real public server: a wind charge detonating sends
//! `minecraft:gust_emitter_small` (registry id 34), and the whole packet was
//! rejected with *"unmodeled explosionParticle registry id 34"*. The guard was
//! keyed on the two ids we draw rather than on the question that actually
//! matters — **does this particle's stream codec read any further bytes** —
//! and 103 of the 125 registered types are `SimpleParticleType`s that read
//! none, so the allowlist rejected the large majority of legal packets.
//!
//! The discriminating pair below is therefore a *simple* id we never send (34)
//! against a *parameterised* one (21, `minecraft:dust`, which carries a colour).
//! Testing 29/30 alone cannot see the bug, since those pass under both the old
//! allowlist and the correct rule.

use lodestone_model::adapter::{ConnectionState, VersionAdapter};
use lodestone_v26_2::V770Adapter;
use lodestone_v26_2::packet_ids::play;
use lodestone_world::World;

/// `minecraft:gust_emitter_small` — argument-less, and the id from the real
/// session's dropped packet.
const GUST_EMITTER_SMALL: i32 = 34;
/// `minecraft:dust` — carries an RGB colour, so its trailing bytes cannot be
/// skipped byte-accurately.
const DUST: i32 = 21;

/// A complete `ClientboundExplodePacket` payload whose `explosionParticle` is
/// `particle_id`, with an inline (holder id 0) sound so the test depends on no
/// registry index.
fn explode_payload(particle_id: i32) -> Vec<u8> {
    let mut p = Vec::new();
    for v in [1.0f64, 64.0, -3.0] {
        p.extend_from_slice(&v.to_be_bytes());
    }
    p.extend_from_slice(&3.5f32.to_be_bytes()); // radius
    p.extend_from_slice(&0i32.to_be_bytes()); // blockCount
    p.push(0); // playerKnockback: Optional<Vec3> = empty
    write_var_i32(&mut p, particle_id);
    write_var_i32(&mut p, 0); // sound holder: 0 => inline definition follows
    let name = "minecraft:entity.generic.explode";
    write_var_i32(&mut p, name.len() as i32);
    p.extend_from_slice(name.as_bytes());
    p.push(0); // fixedRange: Optional<f32> = empty
    p
}

fn write_var_i32(out: &mut Vec<u8>, mut value: i32) {
    loop {
        let byte = (value & 0x7F) as u8;
        value = ((value as u32) >> 7) as i32;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn decode(particle_id: i32) -> Result<usize, String> {
    let adapter = V770Adapter::new();
    let mut world = World::new();
    adapter
        .handle_packet(
            &mut world,
            ConnectionState::Play,
            play::clientbound::EXPLODE,
            &explode_payload(particle_id),
        )
        .map(|directives| directives.len())
        .map_err(|e| format!("{e:?}"))
}

#[test]
fn a_simple_particle_we_never_send_still_decodes_and_a_parameterised_one_is_refused() {
    // Both arms are collected before either is asserted: an `assert!` between
    // them would abort on the first failure and leave the second arm an
    // argument rather than an observation.
    let gust = decode(GUST_EMITTER_SMALL);
    let dust = decode(DUST);

    assert!(
        gust.is_ok(),
        "a wind charge's gust_emitter_small is argument-less, so the packet must \
         decode whole — this is the exact id a real session dropped: {gust:?}"
    );
    assert!(
        dust.is_err(),
        "dust carries a colour this decoder does not consume, so accepting it \
         would desynchronise the stream rather than degrade one field"
    );

    // The control against the *old* rule: the two ids our own server sends must
    // keep working, so this fix widens the guard rather than moving it.
    assert!(decode(29).is_ok(), "explosion_emitter must still decode");
    assert!(decode(30).is_ok(), "explosion must still decode");
}
