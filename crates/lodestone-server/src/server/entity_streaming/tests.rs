//! Tests for the entity streamer: spawn, update, removal, metadata and leash streaming.

use super::*;
use crate::chunk::ChunkColumn;
use crate::mob_effects::ActiveEffects;
use crate::protocol::MetadataField;
use lodestone_model::{Rotation, Vec3};
use std::sync::atomic::{AtomicUsize, Ordering};
use uuid::Uuid;

/// A protocol double whose entity encoders tag each directive with a
/// distinct packet id and the entity id(s) involved, so a test can read the
/// streamer's diff *decisions* straight off the returned directives. It does
/// not implement the chunk/login half — the streamer never calls those.
struct TagProto;

#[test]
fn equal_dimension_revisions_need_a_full_stream_reset() {
    let world = crate::world_state::WorldStateHandle::new();
    let overworld = world.ensure_dimension_runtime(crate::dimension::Dimension::Overworld);
    let nether = world.ensure_dimension_runtime(crate::dimension::Dimension::Nether);
    for (runtime, species) in [(&overworld, "minecraft:cow"), (&nether, "minecraft:zombified_piglin")] {
        assert_eq!(runtime.mobs().with(|sim| sim.spawn_species(species.parse().unwrap(), Vec3::new(1.0, 61.0, 2.0)).id()), 1000);
        runtime.publish_entities();
    }
    let home = ActiveEntities::new(&world, &NoEntities, crate::dimension::Dimension::Overworld);
    let destination = ActiveEntities::new(&world, &NoEntities, crate::dimension::Dimension::Nether);
    let mut streamer = EntityStreamer::default();
    let mut roster = PlayerListStreamer::default();
    assert!(stream_pass(&TagProto, &home, &mut streamer, &mut roster, None).iter()
        .any(|directive| matches!(directive, ServerDirective::Send { packet_id: ADD, .. })));
    let control = stream_pass(&TagProto, &destination, &mut streamer, &mut roster, None);
    assert!(!control.iter().any(|directive| matches!(directive, ServerDirective::Send { packet_id: ADD, .. })),
        "the unchanged revision control actually suppresses the destination addition");
    assert_eq!(streamer.last_sent[&1000].entity_type.to_string(), "minecraft:cow");
    let _ = streamer.reset_dimension(&TagProto);
    let corrected = stream_pass(&TagProto, &destination, &mut streamer, &mut roster, None);
    assert!(corrected.iter().any(|directive| matches!(directive, ServerDirective::Send { packet_id: ADD, .. })));
    assert_eq!(streamer.last_sent[&1000].entity_type.to_string(), "minecraft:zombified_piglin");
}

const ADD: i32 = 1;

const UPDATE: i32 = 2;

const REMOVE: i32 = 3;

const METADATA: i32 = 4;

const LINK: i32 = 5;

const ATTRIBUTES: i32 = 6;

const HEALTH: i32 = 7;

const PARTICLES: i32 = 8;

const ROSTER_ADD: i32 = 9;

const ROSTER_REMOVE: i32 = 10;

const BOSS_ADD: i32 = 11;

const BOSS_PROGRESS: i32 = 12;

const BOSS_REMOVE: i32 = 13;

impl ServerProtocol for TagProto {
    fn decode(&self, _s: State, _id: i32, _p: &[u8]) -> ServerBound {
        unimplemented!("streamer never decodes")
    }
    fn login_success(&self, _u: &str, _uuid: Uuid) -> Vec<ServerDirective> {
        unimplemented!()
    }
    fn begin_configuration(&self) -> Vec<ServerDirective> {
        unimplemented!()
    }
    fn begin_play(&self, _join: &crate::protocol::JoinGame) -> Vec<ServerDirective> {
        unimplemented!()
    }
    fn begin_chunk_batch(&self) -> ServerDirective {
        unimplemented!()
    }
    fn encode_chunk(&self, _cx: i32, _cz: i32, _c: &ChunkColumn) -> ServerDirective {
        unimplemented!()
    }
    fn end_chunk_batch(&self, _n: i32) -> ServerDirective {
        unimplemented!()
    }

    fn encode_add_entity(&self, entity: &EntitySnapshot) -> ServerDirective {
        ServerDirective::Send {
            packet_id: ADD,
            payload: vec![entity.id as u8],
        }
    }
    fn encode_entity_update(
        &self,
        _prev: Option<&EntitySnapshot>,
        current: &EntitySnapshot,
    ) -> Vec<ServerDirective> {
        vec![ServerDirective::Send {
            packet_id: UPDATE,
            payload: vec![current.id as u8],
        }]
    }
    fn encode_remove_entity(&self, ids: &[i32]) -> ServerDirective {
        ServerDirective::Send {
            packet_id: REMOVE,
            payload: ids.iter().map(|id| *id as u8).collect(),
        }
    }

    fn encode_player_info_add(
        &self,
        players: &[crate::protocol::PlayerListing],
    ) -> Vec<ServerDirective> {
        vec![ServerDirective::Send {
            packet_id: ROSTER_ADD,
            payload: vec![players.len() as u8],
        }]
    }
    fn encode_player_info_remove(&self, uuids: &[Uuid]) -> Vec<ServerDirective> {
        vec![ServerDirective::Send {
            packet_id: ROSTER_REMOVE,
            payload: vec![uuids.len() as u8],
        }]
    }
    fn encode_boss_event_add(&self, _id: Uuid, _name: &Text, progress: f32) -> ServerDirective {
        ServerDirective::Send {
            packet_id: BOSS_ADD,
            payload: progress.to_be_bytes().to_vec(),
        }
    }
    fn encode_boss_event_update_progress(&self, _id: Uuid, progress: f32) -> ServerDirective {
        ServerDirective::Send {
            packet_id: BOSS_PROGRESS,
            payload: progress.to_be_bytes().to_vec(),
        }
    }
    fn encode_boss_event_remove(&self, _id: Uuid) -> ServerDirective {
        ServerDirective::Send {
            packet_id: BOSS_REMOVE,
            payload: Vec::new(),
        }
    }

    fn encode_set_entity_data(&self, entity_id: i32, fields: &[MetadataField]) -> ServerDirective {
        ServerDirective::Send {
            packet_id: METADATA,
            payload: std::iter::once(entity_id as u8)
                .chain(std::iter::once(fields.len() as u8))
                .collect(),
        }
    }
    fn encode_set_entity_link(&self, source_id: i32, target_id: Option<i32>) -> ServerDirective {
        ServerDirective::Send {
            packet_id: LINK,
            // `255` as the "no target" byte: every id this test file uses is
            // small and positive, so it cannot collide with a real target and
            // stays visually distinct from `0`, which is also a plausible id.
            payload: vec![source_id as u8, target_id.map_or(255, |id| id as u8)],
        }
    }
    fn encode_update_attributes(&self, attributes: &[EntityAttributeSnapshot]) -> ServerDirective {
        let max_health = attributes
            .iter()
            .find(|snapshot| snapshot.attribute.to_string() == "minecraft:max_health")
            .map(|snapshot| snapshot.base as u8)
            .unwrap_or_default();
        ServerDirective::Send {
            packet_id: ATTRIBUTES,
            payload: vec![max_health],
        }
    }
    fn encode_set_health(&self, health: f32, _food: i32, _saturation: f32) -> ServerDirective {
        ServerDirective::Send {
            packet_id: HEALTH,
            payload: vec![health as u8],
        }
    }
    fn encode_level_particles(
        &self,
        particle: &str,
        pos: Vec3,
        _offset: lodestone_model::Vec3f,
        _max_speed: f32,
        _count: i32,
        _long_distance: bool,
    ) -> ServerDirective {
        (particle == "minecraft:gust_emitter_small")
            .then(|| ServerDirective::Send {
                packet_id: PARTICLES,
                payload: [pos.x.to_be_bytes(), pos.y.to_be_bytes(), pos.z.to_be_bytes()].concat(),
            })
            .unwrap_or(ServerDirective::None)
    }
}

fn snap(id: i32, x: f64) -> EntitySnapshot {
    EntitySnapshot {
        id,
        uuid: Uuid::nil(),
        entity_type: "minecraft:zombie".parse().unwrap(),
        position: Vec3::new(x, 0.0, 0.0),
        rotation: Rotation::new(0.0, 0.0),
        head_yaw: 0.0,
        velocity: Vec3::new(0.0, 0.0, 0.0),
        on_ground: false,
        metadata: Vec::new(),
        object_data: 0,
        equipment: Vec::new(),
        leash_link: None,
    }
}

/// Extracts `(packet_id, payload)` from a `Send` directive for assertions.
fn sent(d: &ServerDirective) -> (i32, &[u8]) {
    match d {
        ServerDirective::Send { packet_id, payload } => (*packet_id, payload.as_slice()),
        other => panic!("expected Send, got {other:?}"),
    }
}

fn assert_sent(out: &[ServerDirective], expected: &[(i32, &[u8])]) {
    assert_eq!(out.iter().map(sent).collect::<Vec<_>>().as_slice(), expected);
}

#[derive(Default)]
struct CountedPublication {
    source: crate::LiveMobSource,
    snapshot_reads: std::sync::Arc<AtomicUsize>,
}

impl EntitySource for CountedPublication {
    fn snapshots(&self) -> Vec<EntitySnapshot> {
        self.snapshot_reads.fetch_add(1, Ordering::Relaxed);
        self.source.snapshots()
    }

    fn snapshots_if_changed(
        &self,
        previous_revision: Option<u64>,
    ) -> Option<(u64, Vec<EntitySnapshot>)> {
        let publication = self.source.snapshots_if_changed(previous_revision)?;
        self.snapshot_reads.fetch_add(1, Ordering::Relaxed);
        Some(publication)
    }

    fn boss_bars(&self) -> Vec<BossBarSnapshot> {
        self.source.boss_bars()
    }
}

#[test]
fn publication_stream_skips_reads_and_keeps_connection_lifecycles() {
    let source = CountedPublication::default();
    assert_eq!(
        source.source.snapshots_if_changed(None),
        Some((0, Vec::new())),
    );
    assert_eq!(source.source.snapshots_if_changed(Some(0)), None);
    source.source.publish(vec![snap(10, 1.25)]);
    let mut first = EntityStreamer::default();
    let mut first_list = PlayerListStreamer::default();
    let out = stream_pass(&TagProto, &source, &mut first, &mut first_list, None);
    assert_sent(&out, &[(ADD, &[10])]);
    for _ in 0..100 {
        assert!(stream_pass(&TagProto, &source, &mut first, &mut first_list, None).is_empty());
    }
    assert_eq!(source.snapshot_reads.load(Ordering::Relaxed), 1);

    source.source.publish(vec![snap(10, 3.75)]);
    let out = stream_pass(&TagProto, &source, &mut first, &mut first_list, None);
    assert_sent(&out, &[(UPDATE, &[10])]);
    assert_eq!(first.last_sent[&10].position.x, 3.75);

    let mut second = EntityStreamer::default();
    let mut second_list = PlayerListStreamer::default();
    let out = stream_pass(&TagProto, &source, &mut second, &mut second_list, None);
    assert_sent(&out, &[(ADD, &[10])]);
    source.source.publish(vec![snap(10, 3.75)]);
    assert!(stream_pass(&TagProto, &source, &mut first, &mut first_list, None).is_empty());
    assert_eq!(first.last_publication, Some(3));
    assert_eq!(source.snapshot_reads.load(Ordering::Relaxed), 4);

    source.source.publish(Vec::new());
    for (streamer, list) in [(&mut first, &mut first_list), (&mut second, &mut second_list)] {
        let out = stream_pass(&TagProto, &source, streamer, list, None);
        assert_sent(&out, &[(REMOVE, &[10])]);
    }
    let mut reconnected = EntityStreamer::default();
    let mut reconnected_list = PlayerListStreamer::default();
    let out = stream_pass(&TagProto, &source, &mut reconnected, &mut reconnected_list, None);
    assert!(out.is_empty());
    assert_eq!(reconnected.last_publication, Some(4));
    assert_eq!(source.snapshot_reads.load(Ordering::Relaxed), 7);
}

#[test]
fn unversioned_source_still_streams_direct_mutations() {
    struct DirectSource(std::sync::Mutex<Vec<EntitySnapshot>>);
    impl EntitySource for DirectSource {
        fn snapshots(&self) -> Vec<EntitySnapshot> {
            self.0.lock().unwrap().clone()
        }
    }
    let source = DirectSource(std::sync::Mutex::new(vec![snap(20, 1.25)]));
    let mut streamer = EntityStreamer::default();
    let mut list = PlayerListStreamer::default();
    let out = stream_pass(&TagProto, &source, &mut streamer, &mut list, None);
    assert_sent(&out, &[(ADD, &[20])]);
    *source.0.lock().unwrap() = vec![snap(20, 3.75)];
    let out = stream_pass(&TagProto, &source, &mut streamer, &mut list, None);
    assert_sent(&out, &[(UPDATE, &[20])]);
    source.0.lock().unwrap().clear();
    let out = stream_pass(&TagProto, &source, &mut streamer, &mut list, None);
    assert_sent(&out, &[(REMOVE, &[20])]);
}

#[test]
fn player_view_changes_stream_without_a_mob_publication() {
    let players = PlayerRegistry::new();
    let viewer = players.join("Viewer", Uuid::from_u128(1), Vec3::new(1.0, 70.0, 2.0));
    let counted = CountedPublication::default();
    let publication = counted.source.clone();
    let snapshot_reads = std::sync::Arc::clone(&counted.snapshot_reads);
    let source = crate::players::PlayerAwareSource::new(counted, players.clone());
    let mut item = snap(20, 2.25);
    item.entity_type = "minecraft:item".parse().unwrap();
    item.metadata = vec![MetadataField::Item {
        item: "minecraft:stone".parse().unwrap(),
        count: 3,
        components: None,
    }];
    publication.publish(vec![snap(10, 1.25), item.clone()]);
    let mut streamer = EntityStreamer::default();
    let mut list = PlayerListStreamer::default();
    let out = stream_pass(&TagProto, &source, &mut streamer, &mut list, Some(&viewer));
    assert_sent(&out, &[
        (ROSTER_ADD, &[1]),
        (ADD, &[10]),
        (ADD, &[20]),
        (METADATA, &[20, 1]),
    ]);
    for _ in 0..100 {
        let out = stream_pass(&TagProto, &source, &mut streamer, &mut list, Some(&viewer));
        assert!(out.is_empty());
    }
    assert_eq!(snapshot_reads.load(Ordering::Relaxed), 1);
    assert_eq!(streamer.last_sent.len(), 2);
    assert!(streamer.players_last_sent.is_empty());
    let peer = players.join("Peer", Uuid::from_u128(2), Vec3::new(3.0, 70.0, 4.0));
    let peer_id = peer.entity_id();
    let out = stream_pass(&TagProto, &source, &mut streamer, &mut list, Some(&viewer));
    assert_sent(&out, &[
        (ROSTER_ADD, &[1]),
        (ADD, &[peer_id as u8]),
        (METADATA, &[peer_id as u8, 1]),
    ]);
    assert!(!streamer.players_last_sent.contains_key(&viewer.entity_id()));
    players.set_position(peer_id, Vec3::new(6.25, 70.0, 4.0));
    let out = stream_pass(&TagProto, &source, &mut streamer, &mut list, Some(&viewer));
    assert_sent(&out, &[(UPDATE, &[peer_id as u8])]);
    assert_eq!(streamer.players_last_sent[&peer_id].position.x, 6.25);
    players.set_shared_flags(peer_id, 0x20);
    let out = stream_pass(&TagProto, &source, &mut streamer, &mut list, Some(&viewer));
    assert_sent(&out, &[
        (UPDATE, &[peer_id as u8]),
        (METADATA, &[peer_id as u8, 1]),
    ]);
    assert_eq!(
        streamer.players_last_sent[&peer_id].metadata,
        vec![MetadataField::SharedFlags(0x20)],
    );
    drop(peer);
    let out = stream_pass(&TagProto, &source, &mut streamer, &mut list, Some(&viewer));
    assert_sent(&out, &[(ROSTER_REMOVE, &[1]), (REMOVE, &[peer_id as u8])]);
    assert_eq!(snapshot_reads.load(Ordering::Relaxed), 1);
    assert_eq!(streamer.last_sent.len(), 2);
    assert!(streamer.players_last_sent.is_empty());

    item.metadata = vec![MetadataField::Item {
        item: "minecraft:stone".parse().unwrap(),
        count: 1,
        components: None,
    }];
    publication.publish(vec![snap(10, 3.75), item]);
    let out = stream_pass(&TagProto, &source, &mut streamer, &mut list, Some(&viewer));
    assert_sent(&out, &[
        (UPDATE, &[10]),
        (UPDATE, &[20]),
        (METADATA, &[20, 1]),
    ]);
    assert_eq!(streamer.last_sent[&10].position.x, 3.75);
    assert_eq!(snapshot_reads.load(Ordering::Relaxed), 2);

    publication.publish(Vec::new());
    let out = stream_pass(&TagProto, &source, &mut streamer, &mut list, Some(&viewer));
    assert_eq!(out.len(), 1);
    let (packet_id, payload) = sent(&out[0]);
    assert_eq!(packet_id, REMOVE);
    let mut removed = payload.to_vec();
    removed.sort_unstable();
    assert_eq!(removed, vec![10, 20]);
    assert_eq!(snapshot_reads.load(Ordering::Relaxed), 3);
    assert!(streamer.last_sent.is_empty());
    let out = stream_pass(&TagProto, &source, &mut streamer, &mut list, Some(&viewer));
    assert!(out.is_empty());
    assert_eq!(snapshot_reads.load(Ordering::Relaxed), 3);

    #[cfg(not(target_arch = "wasm32"))]
    if let Ok(value) = std::env::var("LODESTONE_ENTITY_PUBLICATION_PERF_ITERATIONS") {
        let iterations = value.parse::<usize>()
            .expect("LODESTONE_ENTITY_PUBLICATION_PERF_ITERATIONS must be an integer in 1..=128");
        assert!((1..=128).contains(&iterations),
            "LODESTONE_ENTITY_PUBLICATION_PERF_ITERATIONS must be in 1..=128");

        struct UnversionedPublication(CountedPublication);
        impl EntitySource for UnversionedPublication {
            fn snapshots(&self) -> Vec<EntitySnapshot> {
                self.0.snapshots()
            }
        }

        let publication = crate::LiveMobSource::default();
        publication.publish((0..512).map(|offset| {
            let mut entity = snap(1000 + offset, 1.25 + f64::from(offset) * 0.125);
            entity.metadata = vec![MetadataField::SharedFlags(0)];
            entity
        }).collect());
        let unversioned_counted = CountedPublication {
            source: publication.clone(),
            ..Default::default()
        };
        let versioned_counted = CountedPublication {
            source: publication,
            ..Default::default()
        };
        let reads = [
            std::sync::Arc::clone(&unversioned_counted.snapshot_reads),
            std::sync::Arc::clone(&versioned_counted.snapshot_reads),
        ];
        let unversioned = crate::players::PlayerAwareSource::new(
            UnversionedPublication(unversioned_counted),
            players.clone(),
        );
        let versioned = crate::players::PlayerAwareSource::new(versioned_counted, players.clone());
        let mut streamers: [EntityStreamer; 2] =
            std::array::from_fn(|_| EntityStreamer::default());
        let mut lists: [PlayerListStreamer; 2] =
            std::array::from_fn(|_| PlayerListStreamer::default());
        let mut pass = |arm: usize| {
            if arm == 0 {
                stream_pass(
                    &TagProto,
                    &unversioned,
                    &mut streamers[arm],
                    &mut lists[arm],
                    Some(&viewer),
                )
            } else {
                stream_pass(
                    &TagProto,
                    &versioned,
                    &mut streamers[arm],
                    &mut lists[arm],
                    Some(&viewer),
                )
            }
        };
        for arm in 0..2 {
            let warm = std::hint::black_box(pass(arm));
            assert_eq!(warm.len(), 1025);
            assert_eq!(reads[arm].load(Ordering::Relaxed), 1);
        }
        let mut elapsed = [std::time::Duration::ZERO; 2];
        let mut captures = [0_usize; 2];
        #[cfg(target_os = "macos")]
        let mut retired = [(0_u64, 0_u64); 2];
        for pair in 0..5 {
            let order = if pair % 2 == 0 { [0, 1] } else { [1, 0] };
            for arm in order {
                let reads_before = reads[arm].load(Ordering::Relaxed);
                #[cfg(target_os = "macos")]
                let counters_before = lodestone_testsupport::process_counters::ProcessCounters::read()
                    .expect("entity publication retired counters available");
                let started = Instant::now();
                for _ in 0..iterations {
                    let out = std::hint::black_box(pass(arm));
                    assert!(out.is_empty());
                }
                elapsed[arm] += started.elapsed();
                #[cfg(target_os = "macos")]
                {
                    let counters = lodestone_testsupport::process_counters::ProcessCounters::read()
                        .expect("entity publication retired counters available")
                        .since(counters_before).expect("monotonic retired counters");
                    retired[arm].0 += counters.instructions;
                    retired[arm].1 += counters.cycles;
                }
                let captures_now = reads[arm].load(Ordering::Relaxed) - reads_before;
                assert_eq!(captures_now, if arm == 0 { iterations } else { 0 });
                captures[arm] += captures_now;
            }
        }
        #[cfg(target_os = "macos")]
        let retired_report = format!(
            "unversioned_instructions={} versioned_instructions={} \
             unversioned_cycles={} versioned_cycles={}",
            retired[0].0, retired[1].0, retired[0].1, retired[1].1,
        );
        #[cfg(not(target_os = "macos"))]
        let retired_report = "retired_counters=unavailable";
        eprintln!(
            "ENTITY_PUBLICATION_PERF comparison=unversioned-v-versioned-publication \
             historical_baseline=false process_wide_counters=true entities=512 pairs=5 \
             alternating_order=true passes_per_arm={} unversioned_ns={} versioned_ns={} \
             unversioned_snapshot_reads={} versioned_snapshot_reads={} {retired_report}",
            iterations * 5, elapsed[0].as_nanos(), elapsed[1].as_nanos(),
            captures[0], captures[1],
        );
    }
}

#[test]
fn both_entity_sources_batch_removals_before_spawns() {
    let mut streamer = EntityStreamer::default();
    let _ = streamer.sync_with_players(
        &TagProto,
        Some(&[snap(10, 1.25)]),
        &[snap(30, 2.25)],
    );
    let out = streamer.sync_with_players(
        &TagProto,
        Some(&[snap(20, 3.75)]),
        &[snap(40, 4.75)],
    );
    assert_sent(&out, &[(REMOVE, &[10, 30]), (ADD, &[20]), (ADD, &[40])]);
    assert_eq!(streamer.last_sent.len(), 1);
    assert!(streamer.last_sent.contains_key(&20));
    assert_eq!(streamer.players_last_sent.len(), 1);
    assert!(streamer.players_last_sent.contains_key(&40));
}

#[test]
fn boss_bar_publications_stream_while_entity_revision_is_unchanged() {
    let source = CountedPublication::default();
    let mut streamer = EntityStreamer::default();
    let mut list = PlayerListStreamer::default();
    assert!(stream_pass(&TagProto, &source, &mut streamer, &mut list, None).is_empty());
    let mut bar = BossBarSnapshot {
        id: Uuid::from_u128(3),
        name: Text::literal("Boss"),
        progress: 0.875,
        visible: true,
    };
    source.source.publish_boss_bars(vec![bar.clone()]);
    let out = stream_pass(&TagProto, &source, &mut streamer, &mut list, None);
    assert_sent(&out, &[(BOSS_ADD, &[0x3f, 0x60, 0, 0])]);
    bar.progress = 0.625;
    source.source.publish_boss_bars(vec![bar]);
    let out = stream_pass(&TagProto, &source, &mut streamer, &mut list, None);
    assert_sent(&out, &[(BOSS_PROGRESS, &[0x3f, 0x20, 0, 0])]);
    source.source.publish_boss_bars(Vec::new());
    let out = stream_pass(&TagProto, &source, &mut streamer, &mut list, None);
    assert_sent(&out, &[(BOSS_REMOVE, &[])]);
    assert_eq!(source.snapshot_reads.load(Ordering::Relaxed), 1);
}

#[test]
fn first_sync_spawns_every_entity_in_source_order() {
    let mut s = EntityStreamer::default();
    let out = s.sync(&TagProto, &[snap(10, 0.0), snap(20, 0.0)]);
    assert_eq!(out.len(), 2);
    assert_eq!(sent(&out[0]), (ADD, [10u8].as_slice()));
    assert_eq!(sent(&out[1]), (ADD, [20u8].as_slice()));
}

#[test]
fn resync_with_no_change_emits_nothing() {
    let mut s = EntityStreamer::default();
    let world = [snap(10, 0.0), snap(20, 0.0)];
    let _ = s.sync(&TagProto, &world);
    let out = s.sync(&TagProto, &world);
    assert!(out.is_empty(), "unchanged world must not re-send: {out:?}");
}

#[test]
fn moved_entity_emits_a_single_update() {
    let mut s = EntityStreamer::default();
    let _ = s.sync(&TagProto, &[snap(10, 0.0)]);
    let out = s.sync(&TagProto, &[snap(10, 5.0)]);
    assert_eq!(out.len(), 1);
    assert_eq!(sent(&out[0]), (UPDATE, [10u8].as_slice()));
}

#[test]
fn vanished_entity_is_removed_and_removals_batch() {
    let mut s = EntityStreamer::default();
    let _ = s.sync(&TagProto, &[snap(10, 0.0), snap(20, 0.0), snap(30, 0.0)]);
    let out = s.sync(&TagProto, &[snap(10, 0.0)]);
    // Both 20 and 30 gone -> one batched REMOVE carrying both ids.
    assert_eq!(out.len(), 1);
    let (id, payload) = sent(&out[0]);
    assert_eq!(id, REMOVE);
    let mut ids: Vec<u8> = payload.to_vec();
    ids.sort_unstable();
    assert_eq!(ids, vec![20, 30]);
}

#[test]
fn readding_a_removed_id_spawns_it_again() {
    let mut s = EntityStreamer::default();
    let _ = s.sync(&TagProto, &[snap(10, 0.0), snap(20, 0.0)]);
    let _ = s.sync(&TagProto, &[snap(10, 0.0)]); // 20 removed
    let out = s.sync(&TagProto, &[snap(10, 0.0), snap(20, 0.0)]); // 20 back
    assert_eq!(out.len(), 1);
    assert_eq!(sent(&out[0]), (ADD, [20u8].as_slice()));
}

/// [`snap`] with a non-empty `metadata` — the metadata field list,
/// generic to any entity (not creeper-specific: `EntityStreamer::sync`
/// treats `metadata` uniformly, so a `CreeperSwellDir`/`CreeperIgnited`
/// pair exercises the same code path the next mob's fields will).
fn snap_with_metadata(id: i32, x: f64, metadata: Vec<MetadataField>) -> EntitySnapshot {
    EntitySnapshot { metadata, ..snap(id, x) }
}

/// A spawn whose snapshot already carries non-empty metadata must send
/// `ADD` followed by a metadata sync. The separate metadata frame carries
/// the initial non-default values, including a visible "no swelling
/// animation" transition.
#[test]
fn spawn_with_non_empty_metadata_sends_add_then_metadata() {
    let mut s = EntityStreamer::default();
    let fields = vec![MetadataField::CreeperSwellDir(1)];
    let out = s.sync(&TagProto, &[snap_with_metadata(10, 0.0, fields)]);
    assert_eq!(out.len(), 2);
    assert_eq!(sent(&out[0]), (ADD, [10u8].as_slice()));
    assert_eq!(sent(&out[1]).0, METADATA);
}

#[test]
fn effect_shared_flags_trigger_the_real_metadata_stream_path() {
    let mut streamer = EntityStreamer::default();
    let out = streamer.sync(
        &TagProto,
        &[snap_with_metadata(10, 0.0, vec![MetadataField::SharedFlags(0x20)])],
    );
    assert_eq!(sent(&out[0]), (ADD, [10u8].as_slice()));
    assert_eq!(sent(&out[1]), (METADATA, [10u8, 1].as_slice()));

    let cleared = streamer.sync(
        &TagProto,
        &[snap_with_metadata(10, 0.0, vec![MetadataField::SharedFlags(0)])],
    );
    assert_eq!(sent(&cleared[0]), (UPDATE, [10u8].as_slice()));
    assert_eq!(sent(&cleared[1]), (METADATA, [10u8, 1].as_slice()));
}

#[test]
fn live_air_supply_helper_consumes_the_active_effect_store() {
    let mut water_breathing = ActiveEffects::new();
    water_breathing.apply("minecraft:water_breathing", 200, 0);
    let mut refilling = PlayerVitals::restored(crate::vitals::MAX_HEALTH, 20);
    assert_eq!(
        tick_player_air_supply(&mut refilling, true, false, &water_breathing).air_changed,
        Some(24),
        "the production helper must carry Water Breathing through to the air ticker"
    );

    let mut ordinary = PlayerVitals::restored(crate::vitals::MAX_HEALTH, 20);
    assert_eq!(
        tick_player_air_supply(&mut ordinary, true, false, &ActiveEffects::new()).air_changed,
        Some(19),
        "the no-effect control must still deplete rather than universally refill"
    );
}

#[test]
fn live_saturation_helper_updates_the_authoritative_food_snapshot() {
    let mut vitals = PlayerVitals::restored(crate::vitals::MAX_HEALTH, 300);
    vitals.set_food(crate::food::FoodData::restored(16, 0.0, 0.0, 0));
    assert!(apply_effect_saturation(&mut vitals, 3));
    assert_eq!(vitals.food().food_level(), 19);
    assert_eq!(vitals.food().saturation(), 6.0);

    let mut full = PlayerVitals::default();
    full.set_food(crate::food::FoodData::restored(20, 20.0, 0.0, 0));
    assert!(
        !apply_effect_saturation(&mut full, 1),
        "a full food/saturation snapshot must not request a redundant packet"
    );
}

/// The production publication helper must emit both the folded attribute
/// and the current health frame when Health Boost is added and removed.
/// The removal arm is the important control: it catches an implementation
/// that grows the extra hearts correctly but leaves their client-side row
/// after expiry.
#[tokio::test]
async fn health_boost_attribute_and_health_reach_the_real_connection_path() {
    let (client_end, server_end) = lodestone_net::memory_pair();
    let mut conn = Connection::new(server_end);
    let mut peer = Connection::new(client_end);
    let mut state = State::Play;
    let mut vitals = PlayerVitals::default();
    let mut effects = ActiveEffects::new();

    effects.apply("minecraft:health_boost", 1, 0);
    assert!(
        sync_effect_max_health(&mut conn, &mut state, &TagProto, &mut vitals, &effects)
            .await
            .expect("the active effect must publish")
    );
    assert_eq!(vitals.max_health(), 24.0);
    assert_eq!(peer.read_packet().await.expect("attribute frame"), Some((ATTRIBUTES, vec![24])));
    assert_eq!(peer.read_packet().await.expect("health frame"), Some((HEALTH, vec![20])));

    // The one-tick duration expires through the same store/tick path the
    // live timer uses; direct removal would not prove the expiry arm.
    let _ = effects.tick(0, vitals.health(), vitals.max_health());
    assert!(
        sync_effect_max_health(&mut conn, &mut state, &TagProto, &mut vitals, &effects)
            .await
            .expect("expiry must publish the restored ceiling")
    );
    assert_eq!(vitals.max_health(), crate::vitals::MAX_HEALTH);
    assert_eq!(peer.read_packet().await.expect("restored attribute frame"), Some((ATTRIBUTES, vec![20])));
    assert_eq!(peer.read_packet().await.expect("restored health frame"), Some((HEALTH, vec![20])));
}

/// The death choke point must consume Wind Charged exactly where health
/// crosses zero and put the existing small-gust world effect on the real
/// outbound connection.  The empty-effect control distinguishes a death
/// packet from the effect-specific particle frame.
#[tokio::test]
async fn wind_charged_death_reaches_the_real_connection_path() {
    let (client_end, server_end) = lodestone_net::memory_pair();
    let mut conn = Connection::new(server_end);
    let mut peer = Connection::new(client_end);
    let mut state = State::Play;
    let mut vitals = PlayerVitals::default();
    vitals.kill();
    let mut effects = ActiveEffects::new();
    effects.apply("minecraft:wind_charged", 200, 0);
    let mut advancements = AdvancementManager::new(Vec::new()).expect("empty advancement tree");

    publish_health(
        &mut conn,
        &mut state,
        &TagProto,
        &vitals,
        &effects,
        Vec3::new(4.0, 70.0, 9.0),
        LOCAL_PLAYER_ENTITY_ID,
        "Player",
        crate::vitals::DeathCause::GenericKill,
        &mut advancements,
        Uuid::nil(),
        None,
    )
    .await
    .expect("death publication");

    assert_eq!(peer.read_packet().await.expect("health frame"), Some((HEALTH, vec![0])));
    assert_eq!(
        peer.read_packet().await.expect("small-gust frame"),
        Some((
            PARTICLES,
            [4.0f64.to_be_bytes(), 70.9f64.to_be_bytes(), 9.0f64.to_be_bytes()].concat(),
        )),
        "the effect must use the existing level-particle consumer at the midpoint"
    );

    let no_effect = ActiveEffects::new();
    assert_eq!(no_effect.death_trigger(), None, "control: no active trigger means no gust frame");
}

/// Control: a spawn with *empty* metadata (every existing test's `snap`)
/// must send only `ADD` — proves the metadata branch above is
/// conditional, not unconditional padding on every spawn.
#[test]
fn spawn_with_empty_metadata_sends_only_add() {
    let mut s = EntityStreamer::default();
    let out = s.sync(&TagProto, &[snap(10, 0.0)]);
    assert_eq!(out.len(), 1);
    assert_eq!(sent(&out[0]), (ADD, [10u8].as_slice()));
}

/// A metadata-only change (position/rotation/velocity all unchanged)
/// must still be caught — `EntitySnapshot`'s derived `PartialEq` covers
/// `metadata`, so `Some(prev) if prev != entity` fires exactly as it
/// would for a moved entity, and re-encodes both the (redundant, but
/// harmless) position/rotation update and the metadata sync.
#[test]
fn metadata_only_change_is_caught_even_with_no_motion() {
    let mut s = EntityStreamer::default();
    let _ = s.sync(&TagProto, &[snap_with_metadata(10, 0.0, vec![MetadataField::CreeperSwellDir(-1)])]);
    let out = s.sync(
        &TagProto,
        &[snap_with_metadata(10, 0.0, vec![MetadataField::CreeperSwellDir(1)])],
    );
    assert_eq!(out.len(), 2, "expected UPDATE then METADATA, got {out:?}");
    assert_eq!(sent(&out[0]), (UPDATE, [10u8].as_slice()));
    assert_eq!(sent(&out[1]).0, METADATA);
}

/// Negative control for the test above: re-syncing the exact same
/// metadata (no change at all) must emit nothing, proving the branch is
/// a real diff and not "always resend metadata once present."
#[test]
fn unchanged_metadata_emits_nothing_on_resync() {
    let mut s = EntityStreamer::default();
    let snapshot = snap_with_metadata(10, 0.0, vec![MetadataField::CreeperIgnited(true)]);
    let _ = s.sync(&TagProto, &[snapshot.clone()]);
    let out = s.sync(&TagProto, &[snapshot]);
    assert!(out.is_empty(), "unchanged metadata must not re-send: {out:?}");
}

/// [`snap`] with a `leash_link` already set — the spawn-time link packet.
fn snap_leashed(id: i32, x: f64, target: i32) -> EntitySnapshot {
    EntitySnapshot { leash_link: Some(target), ..snap(id, x) }
}

/// **The discriminating case.** A mob that is *already* leashed by the time
/// a client first spawns it — a fresh join, or walking back into view range
/// after the attach happened — must still get the rope: `ADD` followed by
/// `LINK`. A test that only ever leashes a mob the streamer has already sent
/// once after spawn would not cover this branch. The snapshot therefore
/// includes the link at spawn and requires both records.
#[test]
fn a_mob_already_leashed_on_spawn_sends_add_then_link() {
    let mut s = EntityStreamer::default();
    // Pairwise-distinct ids (10, 0, 77): a transposition of `source_id` and
    // `target_id` inside the encoder would otherwise be invisible — the
    // wire-shape reason this repo's own CLAUDE.md gives for two adjacent
    // same-typed fields.
    let out = s.sync(&TagProto, &[snap_leashed(10, 0.0, 77)]);
    assert_eq!(out.len(), 2, "expected ADD then LINK, got {out:?}");
    assert_eq!(sent(&out[0]), (ADD, [10u8].as_slice()));
    assert_eq!(sent(&out[1]), (LINK, [10u8, 77u8].as_slice()));
}

/// A fresh attach — `None` on the first sync, `Some` on the second — must
/// send `UPDATE` then `LINK`, with the link payload carrying the real
/// target rather than the "no holder" sentinel.
#[test]
fn attaching_a_leash_after_spawn_sends_update_then_link() {
    let mut s = EntityStreamer::default();
    let _ = s.sync(&TagProto, &[snap(10, 0.0)]);
    let out = s.sync(&TagProto, &[snap_leashed(10, 0.0, 77)]);
    assert_eq!(out.len(), 2, "expected UPDATE then LINK, got {out:?}");
    assert_eq!(sent(&out[0]), (UPDATE, [10u8].as_slice()));
    assert_eq!(sent(&out[1]), (LINK, [10u8, 77u8].as_slice()));
}

/// A detach — `Some` then `None` — must send `UPDATE` then `LINK` again,
/// this time carrying the "no holder" sentinel (`255` in this test
/// protocol's own encoding), proving the diff fires on the way down too,
/// not only on the way up.
#[test]
fn detaching_a_leash_sends_update_then_link_with_no_target() {
    let mut s = EntityStreamer::default();
    let _ = s.sync(&TagProto, &[snap_leashed(10, 0.0, 77)]);
    let out = s.sync(&TagProto, &[snap(10, 0.0)]);
    assert_eq!(out.len(), 2, "expected UPDATE then LINK, got {out:?}");
    assert_eq!(sent(&out[0]), (UPDATE, [10u8].as_slice()));
    assert_eq!(sent(&out[1]), (LINK, [10u8, 255u8].as_slice()));
}

/// Negative control: re-syncing the same `leash_link` (still `Some`, same
/// target) must emit nothing extra beyond position/rotation — proving the
/// branch is a real diff, matching the metadata family's own control.
#[test]
fn unchanged_leash_link_emits_no_extra_link_on_resync() {
    let mut s = EntityStreamer::default();
    let snapshot = snap_leashed(10, 0.0, 77);
    let _ = s.sync(&TagProto, &[snapshot.clone()]);
    let out = s.sync(&TagProto, &[snapshot]);
    assert!(out.is_empty(), "unchanged leash_link must not re-send LINK: {out:?}");
}
