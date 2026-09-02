//! Acceptance gate: our own server encoding a creeper's swell
//! metadata and its detonation, decoded back through the **real** v26-2
//! client adapter — the same one `tests/live_creeper_explosion.rs` already
//! validated against bytes captured from a real vanilla server.
//!
//! # Why this is the strongest hermetic check available
//!
//! Per `CLAUDE.md`'s evidence standard, `decode(encode(x)) == x` is satisfied
//! by two symmetric misunderstandings when the same author writes both
//! halves. This gate cannot fully escape that shape — there is no live
//! vanilla client available to point at *our* server, and the brief
//! explicitly says not to launch one — but it narrows the risk two ways:
//!
//! 1. **The decoder is not authored for this issue.** `V770Adapter`'s
//!    `SET_ENTITY_DATA`/`EXPLODE` decoding already existed and was already
//!    validated against real captured vanilla bytes in
//!    `tests/live_creeper_explosion.rs` (`swell_dir == Some(1)`,
//!    `ignited == Some(true)`, the exact sound/category/volume/pitch-band
//!    assertions this file's own `assert_eq!`s below restate). Only the
//!    *encoder* (`V770ServerProtocol::encode_set_entity_data`/
//!    `encode_explode`, `crates/versions/26.2/src/server_protocol.rs`) is
//!    new here.
//! 2. **The wire-format claims are cross-checked against Mojang's own
//!    decompiled source**, not just against what our own decoder accepts —
//!    see `encode_explode`'s own doc comment for the `ClientboundExplodePacket`
//!    field order and the `vanilla's own byte buf codecs's own holder` registry-reference encoding
//!    it cites directly.
//!
//! # What this proves, concretely
//!
//! - A creeper spawned via `MobSim::spawn_species` + `ignite()` — the same
//!   production entry point `crate::tick::run_tick_loop` drives every real
//!   server tick — produces, through our own encoder, a `SET_ENTITY_DATA`
//!   payload that the real client decodes to `creeper_swell_dir == Some(1)`
//!   on the very first tick, matching `vanilla's own creeper's own java`'s
//!   `this.swellDir = 1` and the live-captured assertion in
//!   `tests/live_creeper_explosion.rs`.
//! - The same creeper's fuse reaching `MAX_SWELL` (tick 30) makes
//!   `MobSim::tick` call `MobSim::explode` and populate
//!   `MobSim::take_detonations` — previously discarded entirely — and the
//!   resulting `EXPLODE` payload, through our
//!   encoder, decodes to a `Particles` directive then a `Sound` directive
//!   naming `minecraft:entity.generic.explode` at `SoundCategory::Block`,
//!   volume `4.0`, pitch in vanilla's rolled `0.56..=0.84` band — again
//!   matching `tests/live_creeper_explosion.rs`'s own live-captured
//!   assertions exactly.
//!
//! So: a real client that joined our integrated server while this creeper
//! primed and detonated would now see the swell animation and the
//! explosion, closing both gaps this gate was written to catch.

use lodestone_model::adapter::{ConnectionState, Directive, VersionAdapter};
use lodestone_model::{ClientEvent, ResourceKey, SoundCategory, Vec3};
use lodestone_server::{ChunkWorld, MetadataField, MobSim, ServerProtocol};
use lodestone_v26_2::V770ServerProtocol;
use lodestone_v26_2::packet_ids::play;
use lodestone_world::World;

fn rk(name: &str) -> ResourceKey {
    name.parse().expect("valid resource key")
}

/// Vanilla `vanilla's own creeper's own max swell` (`vanilla's own creeper's own java`) — the tick the fuse
/// completes and `explodeCreeper` fires. Restated as a literal rather than
/// imported from `lodestone_entity::ai::MAX_SWELL` because this crate has no
/// dependency on `lodestone-entity` to add for one constant; already pinned
/// by `crates/lodestone-server/tests/mob_sim.rs`'s own
/// `ignited_creeper_climbs_by_exactly_one_per_tick_and_detonates_at_tick_30`.
const MAX_SWELL: u64 = 30;

/// Replays the raw `SET_ENTITY_DATA`/`EXPLODE` payloads captured below
/// through the real client adapter, exactly as `serve_play`'s connection
/// loop would deliver them, and asserts what a real client's own decoder
/// reports — never a hand-rolled reader.
#[test]
fn our_own_server_encodes_a_creepers_swell_and_detonation_byte_accurately_for_the_real_client() {
    let world = ChunkWorld::new(-4, 24);
    let mut sim = MobSim::new(&world);
    let proto = V770ServerProtocol;
    let adapter = lodestone_v26_2::adapter();

    let creeper_id = {
        let creeper = sim.spawn_species(rk("minecraft:creeper"), Vec3::new(0.0, 0.0, 0.0));
        creeper.ignite();
        creeper.id()
    };

    // The real client adapter resolves a `SET_ENTITY_DATA` payload's
    // `MetadataClass` from its own per-connection `variants` map
    // (`V770Adapter`'s `Arc<Mutex<HashMap<i32, TrackedEntity>>>`), populated
    // when it decodes that entity's `ADD_ENTITY` — never from the world. So
    // this test's `SET_ENTITY_DATA` calls below would silently decode as
    // "unrecognised species, field dropped" (never a hard error — the same
    // guarded-arm behaviour `read_entity_metadata`'s own doc comment
    // describes) unless the same `adapter` instance has already seen this
    // creeper's spawn. One `decode_world`, reused for every call below,
    // mirrors a real connection's single persistent world too.
    let mut decode_world = World::new();
    let initial_snapshot = sim
        .snapshots()
        .into_iter()
        .find(|s| s.id == creeper_id)
        .expect("the freshly spawned creeper must appear in its own first snapshot");
    {
        let lodestone_server::ServerDirective::Send { packet_id, payload } =
            proto.encode_add_entity(&initial_snapshot)
        else {
            panic!("encode_add_entity must return Send for a real entity");
        };
        assert_eq!(packet_id, play::clientbound::ADD_ENTITY);
        adapter
            .handle_packet(&mut decode_world, ConnectionState::Play, packet_id, &payload)
            .expect("the real client adapter must accept our own ADD_ENTITY payload");
    }

    // Mirrors `crate::server::EntityStreamer::sync`'s own metadata diff
    // (`crates/lodestone-server/src/server.rs`), simplified to one entity:
    // call the encoder only when the current tick's metadata differs from
    // the last one actually sent. `EntityStreamer` itself is private to
    // that module, so this test drives the same diff logic `sync` applies
    // rather than constructing one.
    let mut last_metadata: Vec<MetadataField> = Vec::new();
    let mut swell_dir_first_seen: Option<i32> = None;
    let mut ignited_first_seen: Option<bool> = None;
    let mut detonation_tick: Option<u64> = None;
    let mut explode_directives: Option<Vec<Directive>> = None;

    for tick in 1..=MAX_SWELL {
        sim.tick();

        if let Some(snapshot) = sim.snapshots().into_iter().find(|s| s.id == creeper_id)
            && snapshot.metadata != last_metadata
        {
            let directive = proto.encode_set_entity_data(creeper_id, &snapshot.metadata);
            let lodestone_server::ServerDirective::Send { packet_id, payload } = directive else {
                panic!(
                    "encode_set_entity_data must return Send for non-empty metadata, got \
                     {directive:?}"
                );
            };
            assert_eq!(packet_id, play::clientbound::SET_ENTITY_DATA);

            let directives = adapter
                .handle_packet(
                    &mut decode_world,
                    ConnectionState::Play,
                    packet_id,
                    &payload,
                )
                .expect("the real client adapter must accept our own SET_ENTITY_DATA payload");
            for directive in directives {
                if let Directive::Emit(ClientEvent::EntityMetadataUpdated { metadata, .. }) =
                    directive
                {
                    if swell_dir_first_seen.is_none() {
                        swell_dir_first_seen = metadata.creeper_swell_dir;
                    }
                    if let Some(v) = metadata.creeper_ignited {
                        ignited_first_seen = Some(v);
                    }
                }
            }
            last_metadata = snapshot.metadata;
        }

        let detonations = sim.take_detonations();
        if !detonations.is_empty() {
            assert_eq!(
                detonations.len(),
                1,
                "exactly one creeper primed, so exactly one detonation is expected"
            );
            detonation_tick = Some(tick);
            let detonation = detonations[0];
            let directive = proto.encode_explode(detonation.centre, detonation.radius);
            let lodestone_server::ServerDirective::Send { packet_id, payload } = directive else {
                panic!("encode_explode must return Send, got {directive:?}");
            };
            assert_eq!(packet_id, play::clientbound::EXPLODE);

            let directives = adapter
                .handle_packet(&mut decode_world, ConnectionState::Play, packet_id, &payload)
                .expect("the real client adapter must accept our own EXPLODE payload");
            explode_directives = Some(directives);
        }
    }

    // -- swell metadata: matches `tests/live_creeper_explosion.rs`'s own
    // live-captured assertions exactly (real vanilla server bytes, decoded
    // through the same adapter this test uses).
    assert_eq!(
        ignited_first_seen,
        Some(true),
        "an ignited creeper's metadata must report DATA_IS_IGNITED true"
    );
    assert_eq!(
        swell_dir_first_seen,
        Some(1),
        "an ignited creeper's tick() sets swellDir to 1 on its very first tick \
         (vanilla's own creeper's own java) — our own server must encode that same value"
    );

    // -- detonation tick: predicted exactly, not merely "eventually".
    assert_eq!(
        detonation_tick,
        Some(MAX_SWELL),
        "the fuse must complete at exactly tick {MAX_SWELL}, matching \
         crates/lodestone-server/tests/mob_sim.rs's own \
         ignited_creeper_climbs_by_exactly_one_per_tick_and_detonates_at_tick_30"
    );
    assert!(
        sim.get(creeper_id).is_none(),
        "MobSim::tick must discard the creeper on the tick its fuse completes"
    );

    // -- explode: same two directives, same field values,
    // `tests/live_creeper_explosion.rs` asserts against real captured bytes.
    let directives = explode_directives.expect("a detonation must have produced an EXPLODE packet");
    assert_eq!(
        directives.len(),
        2,
        "one Particles directive, then one Sound directive"
    );
    assert!(
        matches!(&directives[0], Directive::Emit(ClientEvent::Particles { .. })),
        "expected a Particles directive first, got {:?}",
        directives[0]
    );
    let Directive::Emit(ClientEvent::Sound {
        sound, category, volume, pitch, ..
    }) = &directives[1]
    else {
        panic!("expected a Sound directive second, got {:?}", directives[1]);
    };
    assert_eq!(
        sound.to_string(),
        "minecraft:entity.generic.explode",
        "an un-powered creeper's detonation must use the plain generic-explode sound"
    );
    assert_eq!(*category, SoundCategory::Block);
    assert_eq!(*volume, 4.0);
    assert!((0.56..=0.84).contains(pitch), "pitch {pitch} outside vanilla's rolled band");
}

/// Negative control: a creeper that is never ignited and never given an
/// attack target must never detonate over a long run — proving the
/// detonation gate above (`take_detonations` yielding something, and
/// `encode_explode` being called at all) comes from the fuse actually
/// completing, not from `MobSim::tick`/`take_detonations` firing
/// unconditionally for every creeper. Mirrors
/// `crates/lodestone-server/tests/mob_sim.rs`'s own
/// `creeper_with_no_target_and_never_ignited_never_primes_or_detonates`,
/// extended to also drain (not just read) `take_detonations` every tick —
/// this test's own gate depends on draining being safe to call when there
/// is nothing to drain.
#[test]
fn an_inert_creeper_never_detonates_or_is_discarded() {
    let world = ChunkWorld::new(-4, 24);
    let mut sim = MobSim::new(&world);

    let creeper_id = {
        let creeper = sim.spawn_species(rk("minecraft:creeper"), Vec3::new(0.0, 0.0, 0.0));
        creeper.id()
    };

    for _ in 0..300 {
        sim.tick();
        assert!(sim.take_detonations().is_empty(), "an inert creeper must never detonate");
    }
    assert!(sim.get(creeper_id).is_some(), "an inert creeper must survive 300 ticks untouched");

    // Sanity: this creeper's own metadata is still present (swell_dir at
    // its `-1` default) and still encodes to a real packet — the control
    // above is "never detonates", not "never has metadata to encode at
    // all", which would be trivially true for the wrong reason.
    let proto = V770ServerProtocol;
    let snapshot = sim
        .snapshots()
        .into_iter()
        .find(|s| s.id == creeper_id)
        .expect("the surviving creeper must still be in the snapshot list");
    assert_eq!(snapshot.metadata, vec![MetadataField::CreeperSwellDir(-1)]);
    assert!(
        matches!(
            proto.encode_set_entity_data(creeper_id, &snapshot.metadata),
            lodestone_server::ServerDirective::Send { .. }
        ),
        "a non-empty field list must still encode to a real packet even at the default value"
    );
}
