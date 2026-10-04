//! World/chunk packets: chunk and light streaming, block updates, particles,
//! sound, world border, time, and the debug/gametest overlay packets. Split
//! out of the former monolithic `adapter.rs`.
use super::*;
use super::player::game_mode;
use super::inventory::{StackCodecContext, read_item_stack_template_with};
use crate::dialect::FixedRegistryKind;
use lodestone_data::particle_types::ParticleTypeId;
use lodestone_data::sound_events::SoundEventId;

#[cfg(test)]
#[path = "chunk/release_controls.rs"]
mod release_controls;

impl V770Adapter {
    /// Clientbound play-state packets in the chunk domain, split out of the
    /// former monolithic `handle_play` (see `adapter::mod` for the coordinator).
    pub(super) fn handle_play_chunk(&self, world: &mut dyn WorldSink, packet_id: i32, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        if packet_id == play::clientbound::LOGIN {
            let mut reader = Reader::new(payload);
            let body = GameLogin::decode(&mut reader, Ctx { version: self.dialect.protocol_version() })
                .map_err(dec_err)?;
            // The two server-switch paths are distinguished here, and this is
            // the one to read first: `login_ordinal > 1` is a **second login on
            // one socket**, which is what a Velocity/BungeeCord backend switch
            // looks like — the client never reconnects and never sees a
            // `minecraft:transfer` packet at all. `TRANSFER` is the other path
            // (a reconnect to a new address, which starts a fresh adapter whose
            // ordinal is `1` again), logged by `handle_play_connection`. See
            // the `xfer` module's doc.
            let login_ordinal = self.note_login();
            tracing::debug!(
                target: "transfer",
                seq = super::xfer::next_seq(),
                login_ordinal,
                entity_id = body.entity_id,
                dimension = %body.dimension,
                path = if login_ordinal > 1 { "backend-swap" } else { "fresh-join" },
                "xfer: LOGIN (join game) received"
            );
            // `dimension_type` is the registry holder id; `dimension` is the
            // level name. The id wins where the registry resolved it, and
            // `enter_dimension` falls back to the name match where it did not.
            let dimension_type =
                self.enter_dimension_from_wire(body.dimension_type, &body.dimension);
            let dimension = body.dimension.parse().map_err(|_| {
                AdapterError::Decode(format!("invalid dimension {}", body.dimension))
            })?;
            // The biome registry's sky colours, indexed by holder id — the
            // integer a chunk section's biome palette stores.
            // Emitted here rather than off `registry_data` itself for the same
            // reason `DimensionTypeChanged` is: `Login` is the point at which
            // the Configuration set is known complete, and re-entering
            // Configuration resends the registries and is followed by a fresh
            // `Login`, so this can never be stale.
            let biome_sky_colors = self
                .registries
                .lock()
                .ok()
                .map(|registries| registries.biome_sky_colors().to_vec())
                .unwrap_or_default();
            // The same registry generation's climate table (the shared biome
            // lane the `chunks_biomes` seam also uses), emitted at the same
            // point and for the same reason as `biome_sky_colors` just above
            // — see `BiomeClimates`'s
            // own doc for why this is a second variant rather than two more
            // fields on `BiomeVisuals`.
            let (biome_temperatures, biome_downfall, biome_has_precipitation) = self
                .registries
                .lock()
                .ok()
                .map(|registries| {
                    let climates = registries.biome_climates();
                    (
                        climates.iter().map(|c| c.map(|c| c.temperature)).collect(),
                        climates.iter().map(|c| c.map(|c| c.downfall)).collect(),
                        climates
                            .iter()
                            .map(|c| c.map(|c| c.has_precipitation))
                            .collect(),
                    )
                })
                .unwrap_or_default();
            // The same registry generation's entry *names*, indexed by holder
            // id exactly like the two tables above (a follow-up to the biome
            // sky-colour and climate lanes above, `eb423ac`) — see
            // `ClientEvent::BiomeRegistryNames`'s own doc for
            // why the mesher's `FALLBACK_BIOME_NAMES` fallback is otherwise
            // wrong against a third-party server. `entry_names` already
            // decodes this correctly; nothing before this
            // change carried it past this crate.
            let biome_names = self
                .registries
                .lock()
                .ok()
                .and_then(|registries| {
                    registries
                        .entry_names(ClientRegistries::BIOME)
                        .map(<[String]>::to_vec)
                })
                .unwrap_or_default();
            // The same story one registry over, and the same fix. The
            // `minecraft:enchantment` order was **already decoded** by
            // `entry_names` and never handed past this crate, so
            // `Sim::riptide_level` resolved `minecraft:riptide` through a
            // hardcoded holder id of 32 — `riptide` being the 33rd of 26.2's 43
            // built-in enchantments in resource-location-sorted order. Right
            // against vanilla, silently wrong against any data pack that reorders,
            // because the id stays valid and still names *an* enchantment.
            let enchantment_names = self
                .registries
                .lock()
                .ok()
                .and_then(|registries| {
                    registries
                        .entry_names("minecraft:enchantment")
                        .map(<[String]>::to_vec)
                })
                .unwrap_or_default();
            return Ok(vec![
                // Before `Login`, deliberately: a consumer folding both sees the
                // dimension's geometry before the level name that depends on it.
                Directive::Emit(ClientEvent::DimensionTypeChanged {
                    holder_id: body.dimension_type,
                    dimension_type,
                    is_flat: body.is_flat,
                }),
                Directive::Emit(ClientEvent::BiomeVisuals {
                    sky_colors: biome_sky_colors,
                }),
                Directive::Emit(ClientEvent::BiomeClimates {
                    temperatures: biome_temperatures,
                    downfall: biome_downfall,
                    has_precipitation: biome_has_precipitation,
                }),
                Directive::Emit(ClientEvent::BiomeRegistryNames { names: biome_names }),
                Directive::Emit(ClientEvent::EnchantmentRegistryNames {
                    names: enchantment_names,
                }),
                Directive::Emit(ClientEvent::Login {
                    entity_id: body.entity_id,
                    game_mode: game_mode(body.game_type)?,
                    dimension,
                }),
                Directive::Emit(ClientEvent::ChunkCacheRadiusChanged { radius: body.view_distance }),
                Directive::Emit(ClientEvent::SimulationDistanceChanged { distance: body.simulation_distance }),
            ]);
        }
        if packet_id == play::clientbound::CHUNK_BATCH_START {
            // Empty packet; it only marks the start of a batch for rate timing.
            Reader::new(payload).ensure_empty().map_err(dec_err)?;
            self.begin_chunk_batch();
            return Ok(vec![]);
        }
        if packet_id == play::clientbound::CHUNK_BATCH_FINISHED {
            // Acknowledge the batch — the server halts chunk delivery after ten
            // unacknowledged batches — reporting the estimated desired rate.
            let body: ChunkBatchFinished = decode_body(payload)?;
            let desired_chunks_per_tick = self.finish_chunk_batch(body.batch_size);
            return Ok(vec![send(
                play::serverbound::CHUNK_BATCH_RECEIVED,
                &ChunkBatchReceived {
                    desired_chunks_per_tick,
                },
            )?]);
        }
        if packet_id == play::clientbound::CHUNKS_BIOMES {
            // The clientbound chunks-biomes packet (id 13): a VarInt-prefixed
            // list of `(chunk position, byte[])` entries. Vanilla sends this
            // to *resend* biomes for chunks a player already has loaded —
            // the chunk map's own biome-resend routine, whose only caller is
            // `/fillbiome` — never at initial load, which is why the
            // per-section biome container already rides `level_chunk_with_light`
            // and this packet only ever *updates* it.
            //
            // Each entry's byte array is, per vanilla's own chunk-biome-data
            // extraction routine, every section's biome-container encoder
            // output back to back with **no other framing at all** — no
            // non-air/fluid counts (it has no blocks to count), no
            // block-state container, just `section_count` biome containers
            // in ascending section order. That makes this the one
            // chunk-shaped packet whose per-section loop is *shorter* than
            // `level_chunk_with_light`'s, not a variant of it.
            let shape = self.current_shape();
            let mut reader = Reader::new(payload);
            let count = reader.var_i32().map_err(dec_err)?;
            let count = usize::try_from(count)
                .map_err(|_| AdapterError::Decode(format!("negative chunk-biomes count {count}")))?;
            let mut directives = Vec::with_capacity(count.min(1024));
            for _ in 0..count {
                // Vanilla's packed chunk-position encoding: x in the low 32
                // bits, z in the high 32 — the same layout `forget_level_chunk`
                // already unpacks.
                let packed = reader.i64().map_err(dec_err)?;
                let (x, z) = (packed as i32, (packed >> 32) as i32);
                let bytes = reader.var_bytes(2_097_152).map_err(dec_err)?;
                let mut blob = Reader::new(bytes);
                let mut patch = BiomePatch::new();
                for section_index in 0..shape.section_count {
                    let biomes = PalettedContainer::decode(shape.biome_kind, &mut blob)
                        .map_err(|err| AdapterError::Decode(err.to_string()))?;
                    patch.set_section(section_index, biomes);
                }
                // Zero trailing bytes in this chunk's own sub-blob is the
                // strongest per-chunk alignment check, exactly as
                // `level_chunk_with_light`'s section blob uses `ensure_empty` on
                // its own bounded sub-reader.
                blob.ensure_empty().map_err(dec_err)?;
                world.merge_biomes(WorldChunkPos::new(x, z), patch);
                // Reused rather than a new event: `ChunkLoaded` already means "the
                // column at pos is dirty, re-read or re-mesh it" (see
                // `light_update`'s arm above), which is exactly what a live biome
                // change needs — surface material and (once wired) tint both
                // read the world directly, not the event payload.
                directives.push(Directive::Emit(ClientEvent::ChunkLoaded {
                    pos: ChunkPos::new(x, z),
                }));
            }
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(directives);
        }
        if packet_id == play::clientbound::LEVEL_CHUNK_WITH_LIGHT {
            let deferred = self.decode_level_chunk_with_light(payload)?;
            world.load(deferred.position, deferred.chunk);
            return Ok(deferred.directives);
        }
        if packet_id == play::clientbound::LIGHT_UPDATE {
            // A standalone, light-only update carrying the same six-field light
            // payload embedded in `level_chunk_with_light`, but applied as a
            // *merge*: a section named by a full mask is replaced, one named by
            // an empty mask becomes explicit zero, and one named by neither is
            // left unchanged. All three-state semantics live in
            // `LightPatch::from_light_masks`; this arm only reads wire
            // primitives, in wire order. Note the wire order is NOT the
            // constructor's argument order — the four bitsets arrive
            // sky/block/empty-sky/empty-block, then the two array lists.
            let mut reader = Reader::new(payload);
            let x = reader.var_i32().map_err(dec_err)?;
            let z = reader.var_i32().map_err(dec_err)?;
            let wire = crate::packets::chunk::bit_set_wire(self.dialect.game_data_version());
            let sky_mask = read_wire_bitset(&mut reader, wire)?;
            let block_mask = read_wire_bitset(&mut reader, wire)?;
            let empty_sky_mask = read_wire_bitset(&mut reader, wire)?;
            let empty_block_mask = read_wire_bitset(&mut reader, wire)?;
            let sky_arrays = read_light_arrays(&mut reader)?;
            let block_arrays = read_light_arrays(&mut reader)?;
            // Zero trailing bytes is the highest-value detector here: a wrong
            // 2048 array length or an off-by-one bitset word-count leaves the
            // buffer misaligned, which shows up only as leftover bytes.
            reader.ensure_empty().map_err(dec_err)?;
            let patch = LightPatch::from_light_masks(
                &sky_mask,
                &empty_sky_mask,
                sky_arrays,
                &block_mask,
                &empty_block_mask,
                block_arrays,
            );
            let sections = world.merge_light_changes(WorldChunkPos::new(x, z), patch);
            return Ok(if sections.is_empty() {
                Vec::new()
            } else {
                vec![Directive::Emit(ClientEvent::ChunkLightChangedPrecise {
                    pos: ChunkPos::new(x, z),
                    sections,
                })]
            });
        }
        if packet_id == play::clientbound::FORGET_LEVEL_CHUNK {
            // A single packed long: x in the low 32 bits, z in the high 32
            // (vanilla's own packed chunk-position encoding, verified against
            // 26.2 source).
            let mut reader = Reader::new(payload);
            let packed = reader
                .i64()
                .map_err(|err| AdapterError::Decode(err.to_string()))?;
            reader
                .ensure_empty()
                .map_err(|err| AdapterError::Decode(err.to_string()))?;
            let (x, z) = (packed as i32, (packed >> 32) as i32);
            world.unload(WorldChunkPos::new(x, z));
            return Ok(vec![Directive::Emit(ClientEvent::ChunkUnloaded {
                pos: ChunkPos::new(x, z),
            })]);
        }
        if packet_id == play::clientbound::BLOCK_UPDATE {
            // A single block change: a packed `BlockPos` long and the new block
            // state's registry id. It mutates exactly the one loaded section that
            // owns the position — a no-op if that chunk is not held — so the
            // world stays live after break/place rather than frozen at load.
            let mut reader = Reader::new(payload);
            let packed = reader.i64().map_err(dec_err)?;
            let state = reader.var_i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            let pos = unpack_block_pos(packed);
            let state = u32::try_from(state)
                .map_err(|_| AdapterError::Decode(format!("negative block state id {state}")))?;
            let state = self.dialect.game_data_version().state_from_wire(state)
                .ok_or_else(|| AdapterError::Decode(format!("unknown block state id {state}")))?
                .raw();
            world.set_block(pos.x, pos.y, pos.z, state);
            // Writing a block state is what creates (or destroys) a block
            // entity: vanilla does it inside the chunk's own block-state setter,
            // with no packet involved. Skipping this leaves
            // a placed chest with a state, no record, and zero pixels,
            // which still *opened* because interaction reads the state.
            // `World::sync_block_entity` documents the create/keep/replace/remove
            // rule; the `Option` is the version-specific half.
            world.sync_block_entity(
                pos.x,
                pos.y,
                pos.z,
                lodestone_data::block_states::StateId::new(state)
                    .and_then(block_entity_type)
                    .map(|kind| kind.raw()),
            );
            // Dirty exactly the section that owns the block. Without this a
            // break/place the *server* sends is applied to the world but never
            // drawn until some other event happens to dirty the column — the
            // silent desync behind "the chunk only renders properly when I
            // break something". A section-scoped signal (rather than reusing
            // `ChunkLoaded`) lets the consumer re-derive one section, and only
            // the neighbours a boundary cell actually touches.
            return Ok(vec![Directive::Emit(ClientEvent::SectionBlocksChanged {
                section: SectionPos::new(pos.x >> 4, pos.y >> 4, pos.z >> 4),
                blocks: vec![[
                    pos.x.rem_euclid(16) as u8,
                    pos.y.rem_euclid(16) as u8,
                    pos.z.rem_euclid(16) as u8,
                ]],
            })]);
        }
        if packet_id == play::clientbound::SECTION_BLOCKS_UPDATE {
            // Many block changes within one section: a packed `SectionPos` long,
            // a count, then that many VarLongs each carrying `state << 12 | local`
            // where `local` packs the section-relative `x<<8 | z<<4 | y`. All
            // writes land in the one section, forking its storage at most once.
            let mut reader = Reader::new(payload);
            let node = reader.i64().map_err(dec_err)?;
            let (section_x, section_y, section_z) = unpack_section_pos(node);
            let count = reader.var_i32().map_err(dec_err)?;
            let count = usize::try_from(count).map_err(|_| {
                AdapterError::Decode(format!("negative section update count {count}"))
            })?;
            // A section holds at most 4096 blocks; cap the pre-allocation so a
            // hostile count cannot force a large speculative allocation before
            // the truncated body is rejected by the per-entry reads.
            let mut blocks = Vec::with_capacity(count.min(4096));
            for _ in 0..count {
                let entry = reader.var_i64().map_err(dec_err)?;
                let local = (entry & 0xFFF) as u16;
                let state = u32::try_from((entry as u64) >> 12).map_err(|_| {
                    AdapterError::Decode("section block state id out of range".to_owned())
                })?;
                let state = self.dialect.game_data_version().state_from_wire(state)
                    .ok_or_else(|| AdapterError::Decode(format!("unknown block state id {state}")))?
                    .raw();
                let rel_x = ((local >> 8) & 0xF) as u8;
                let rel_z = ((local >> 4) & 0xF) as u8;
                let rel_y = (local & 0xF) as u8;
                blocks.push((rel_x, rel_y, rel_z, state));
            }
            reader.ensure_empty().map_err(dec_err)?;
            world.set_blocks(section_x, section_y, section_z, &blocks);
            // Every state write goes through `sync_block_entity`, one call per
            // changed cell, for the same reason `BLOCK_UPDATE` does: in vanilla
            // the chunk's own block-state setter is what creates and removes
            // block entities, no packet involved. A piston
            // or a `/fill` arrives here rather than as N `BLOCK_UPDATE`s, so
            // skipping it would leave exactly the same missing-block-entity bug for bulk edits.
            // Section-relative coordinates back to absolute — `set_blocks` does
            // the same conversion internally, but this seam takes absolute
            // coordinates because a block entity is keyed by world position.
            for &(rel_x, rel_y, rel_z, state) in &blocks {
                world.sync_block_entity(
                    (section_x << 4) | i32::from(rel_x),
                    (section_y << 4) | i32::from(rel_y),
                    (section_z << 4) | i32::from(rel_z),
                    lodestone_data::block_states::StateId::new(state)
                        .and_then(block_entity_type)
                        .map(|kind| kind.raw()),
                );
            }
            // Dirty the owning column so a server-authoritative multi-block
            // change (e.g. a falling tree, a piston, another player's edits) is
            // re-meshed rather than silently applied-but-invisible. An empty
            // change set touched nothing, so it needs no re-mesh. The relative
            // coordinates ride along so the consumer can distinguish an
            // interior edit from one on the section boundary.
            if blocks.is_empty() {
                return Ok(Vec::new());
            }
            return Ok(vec![Directive::Emit(ClientEvent::SectionBlocksChanged {
                section: SectionPos::new(section_x, section_y, section_z),
                blocks: blocks.iter().map(|&(x, y, z, _)| [x, y, z]).collect(),
            })]);
        }
        if packet_id == play::clientbound::BLOCK_ENTITY_DATA {
            // A packed BlockPos long, a `registry(BLOCK_ENTITY_TYPE)` VarInt, then
            // the block entity's nameless network NBT compound (its "update tag",
            // not necessarily the full save tag). Mutates the world directly,
            // mirroring BLOCK_UPDATE/SECTION_BLOCKS_UPDATE: a no-op if the owning
            // chunk is not currently loaded.
            //
            // This is what it is in vanilla — *data for an entity that
            // already exists*, created by the chunk packet's block-entity list or
            // by a state write through `sync_block_entity`. It nonetheless still
            // **creates** on a miss (`set_block_entity` is an upsert), which is a
            // deliberate divergence: vanilla's own client-side handler drops
            // the payload when the block entity is not already present at
            // that position and type (confirmed against the decompiled 26.2
            // client and world sources) because it has a pending-block-entities
            // list to promote from later, and we do not.
            // The two failure modes are not symmetric: an orphan record whose
            // state is not a chest resolves to no material and draws nothing (see
            // `lodestone-shell`'s `block_entities`), so creating is inert, whereas
            // dropping would lose server data we cannot ask for again.
            let mut reader = Reader::new(payload);
            let packed = reader.i64().map_err(dec_err)?;
            let type_id = reader.var_i32().map_err(dec_err)?;
            let type_id = u32::try_from(type_id).map_err(|_| {
                AdapterError::Decode(format!("negative block entity type id {type_id}"))
            })?;
            let type_id = self.dialect.canonical_fixed_id(FixedRegistryKind::BlockEntity, type_id as i32)? as u32;
            let nbt = read_network_nbt(&mut reader).map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            let pos = unpack_block_pos(packed);
            world.set_block_entity(pos.x, pos.y, pos.z, type_id, nbt);
            return Ok(Vec::new());
        }
        if packet_id == play::clientbound::BLOCK_EVENT {
            // A packed BlockPos long, two opaque parameter bytes, then a
            // `registry(BLOCK)` VarInt naming the block type the parameters apply
            // to (needed by the consumer to interpret b0/b1 — e.g. a note pitch
            // vs. a piston direction — which the adapter itself does not).
            let mut reader = Reader::new(payload);
            let packed = reader.i64().map_err(dec_err)?;
            let b0 = reader.u8().map_err(dec_err)?;
            let b1 = reader.u8().map_err(dec_err)?;
            let block_id = reader.var_i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            let block_id = u32::try_from(block_id)
                .map_err(|_| AdapterError::Decode(format!("negative block id {block_id}")))?;
            let block = self.dialect.block_from_wire(block_id)
                .ok_or_else(|| AdapterError::Decode(format!("unknown block id {block_id}")))?;
            return Ok(vec![Directive::Emit(ClientEvent::BlockEvent {
                pos: unpack_block_pos(packed),
                b0,
                b1,
                block: parse_key(block.name(), "block")?,
            })]);
        }
        if packet_id == play::clientbound::BLOCK_DESTRUCTION {
            // A VarInt breaker entity id, a packed BlockPos long, then the raw
            // break-stage byte. The stage's exact visual meaning beyond the wire
            // (which values clear the overlay) is a rendering concern, not
            // decoded here.
            let mut reader = Reader::new(payload);
            let entity_id = reader.var_i32().map_err(dec_err)?;
            let packed = reader.i64().map_err(dec_err)?;
            let progress = reader.u8().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::BlockDestruction {
                entity_id,
                pos: unpack_block_pos(packed),
                progress,
            })]);
        }
        if packet_id == play::clientbound::BLOCK_CHANGED_ACK {
            let mut reader = Reader::new(payload);
            let sequence = reader.var_i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::BlockChangedAck {
                sequence: lodestone_model::PredictionSequence::from_wire(sequence),
            })]);
        }
        if packet_id == play::clientbound::SET_CHUNK_CACHE_CENTER {
            let mut reader = Reader::new(payload);
            let x = reader.var_i32().map_err(dec_err)?;
            let z = reader.var_i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(
                ClientEvent::ChunkCacheCenterChanged { x, z },
            )]);
        }
        if packet_id == play::clientbound::SET_CHUNK_CACHE_RADIUS {
            let mut reader = Reader::new(payload);
            let radius = reader.var_i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(
                ClientEvent::ChunkCacheRadiusChanged { radius },
            )]);
        }
        if packet_id == play::clientbound::SET_SIMULATION_DISTANCE {
            let mut reader = Reader::new(payload);
            let distance = reader.var_i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(
                ClientEvent::SimulationDistanceChanged { distance },
            )]);
        }
        if packet_id == play::clientbound::SET_TIME {
            // 26.2 reshaped set_time: a monotonic world age followed by a map of
            // per-world-clock updates (see `packets::time`). Decode it fully so
            // the trailing zero-length check guards the variable-length map.
            let mut reader = Reader::new(payload);
            let time = SetTime::decode(&mut reader, CTX)
                .map_err(|err| AdapterError::Decode(err.to_string()))?;
            reader
                .ensure_empty()
                .map_err(|err| AdapterError::Decode(err.to_string()))?;
            // The day time is *held*, not read off the packet: 19 of every 20
            // `set_time`s carry an empty clock map (the once-a-second game-time
            // sync), and treating that as "the day time is the world age" pinned
            // `sky_darken` to a session constant. Re-anchor only on a real clock
            // update; otherwise extrapolate the held anchor at the server's own
            // rate. See `DayClock` and `SetTime::day_clock`.
            // Which clock is "the" day clock is a *registry* question, and it used
            // to be answered by "the lowest holder id present", which is
            // the overworld clock in every dimension because vanilla registers
            // it first. In the End the right clock is `minecraft:the_end`
            // (holder 1) — see `ClientRegistries::world_clock_id`.
            //
            // `None` here (no `registry_data`, or a dimension with no clock of
            // its own — the Nether has fixed time and no `default_clock`) keeps
            // the lowest-id fallback. That is deliberate rather than reporting
            // "no time": `time_of_day`'s only consumer is a sky curve that does
            // not yet gate on `has_fixed_time`, so a Nether trip reporting the
            // overworld's clock is exactly as good as before and no worse.
            let time_of_day = {
                let clock_holder = self.current_clock_holder();
                let mut clock = self.clock.lock().expect("day clock poisoned");
                if let Some(update) = time.clock_for(clock_holder) {
                    *clock = DayClock {
                        total_ticks: update.total_ticks,
                        rate: update.rate,
                        at_game_time: time.game_time,
                        synced: true,
                    };
                } else if !clock.synced {
                    // No clock update has ever arrived (we are ahead of the
                    // join-time full sync). Seed from the world age, which is
                    // exactly what this arm used to report unconditionally, so
                    // this window is no worse than before and closes on the
                    // first real update.
                    *clock = DayClock {
                        total_ticks: time.game_time,
                        rate: 1.0,
                        at_game_time: time.game_time,
                        synced: false,
                    };
                }
                clock.time_of_day(time.game_time)
            };
            return Ok(vec![Directive::Emit(ClientEvent::TimeChanged {
                world_age: time.game_time,
                time_of_day,
            })]);
        }
        if packet_id == play::clientbound::GAME_EVENT {
            // A small keyed world-state change. Only the aspects the model can
            // represent are surfaced; the rest (demo, arrow-hit, etc.) decode
            // fully — so the trailing check still guards alignment — but
            // produce no directive.
            let event: GameEvent = decode_full(payload)?;
            let directives = match event.event {
                1 => vec![Directive::Emit(ClientEvent::WeatherChanged {
                    raining: Some(true),
                    rain_level: None,
                    thunder_level: None,
                })],
                2 => vec![Directive::Emit(ClientEvent::WeatherChanged {
                    raining: Some(false),
                    rain_level: None,
                    thunder_level: None,
                })],
                3 => game_mode_from_ordinal(event.param as i32)
                    .map(|game_mode| {
                        vec![Directive::Emit(ClientEvent::GameModeChanged { game_mode })]
                    })
                    .unwrap_or_default(),
                // WIN_GAME: exiting the End through the exit
                // portal after the dragon fight. Vanilla's own game-event
                // handler ignores `param` for this event and always opens
                // the credits/win screen with the poem shown, so nothing from
                // the wire needs to ride along — see `ClientEvent::WinGame`'s
                // own doc.
                4 => vec![Directive::Emit(ClientEvent::WinGame)],
                7 => vec![Directive::Emit(ClientEvent::WeatherChanged {
                    raining: None,
                    rain_level: Some(event.param),
                    thunder_level: None,
                })],
                8 => vec![Directive::Emit(ClientEvent::WeatherChanged {
                    raining: None,
                    rain_level: None,
                    thunder_level: Some(event.param),
                })],
                _ => Vec::new(),
            };
            return Ok(directives);
        }
        if packet_id == play::clientbound::SET_DEFAULT_SPAWN_POSITION {
            // Reshaped in 26.2 to carry a full RespawnData: a dimension-qualified
            // position plus yaw and pitch. The model now models all of these.
            let spawn: SetDefaultSpawnPosition = decode_full(payload)?;
            let dimension = spawn.location.dimension.parse().map_err(|_| {
                AdapterError::Decode(format!("invalid dimension {}", spawn.location.dimension))
            })?;
            return Ok(vec![Directive::Emit(ClientEvent::SpawnPositionChanged {
                dimension,
                pos: unpack_block_pos(spawn.location.position),
                angle: spawn.yaw,
                pitch: spawn.pitch,
            })]);
        }
        if packet_id == play::clientbound::LEVEL_EVENT {
            let level_event: LevelEvent = decode_full(payload)?;
            let data = if level_event.event == 2001 {
                let raw = u32::try_from(level_event.data).map_err(|_| {
                    AdapterError::Decode(format!("negative block state id {}", level_event.data))
                })?;
                let state = self.dialect.game_data_version().state_from_wire(raw)
                    .ok_or_else(|| AdapterError::Decode(format!("unknown block state id {raw}")))?;
                LevelEventData::BlockState(BlockStateRef::canonical(state.raw()))
            } else {
                LevelEventData::Raw(level_event.data)
            };
            return Ok(vec![Directive::Emit(ClientEvent::LevelEvent {
                event: level_event.event,
                pos: unpack_block_pos(level_event.position),
                data,
                global: level_event.global,
            })]);
        }
        if packet_id == play::clientbound::LEVEL_PARTICLES {
            let registries = self.registries.lock().expect("client registries lock poisoned");
            let context = StackCodecContext::new(self.dialect, &registries);
            if self.dialect.game_data_version() == lodestone_data::GameDataVersion::V26_3 {
                return decode_latest_particles(payload, &context);
            }
            let particles: LevelParticles = decode_full(payload)?;
            let canonical = self.dialect.canonical_fixed_id(FixedRegistryKind::Particle, particles.particle_id)?;
            let particle_id = ParticleTypeId::new(canonical).ok_or_else(|| {
                AdapterError::Decode(format!("unknown particle id {}", particles.particle_id))
            })?;
            let name = particle_type_name(particle_id);
            let mut options_reader = Reader::new(&particles.options);
            let options = read_particle_options(name, &mut options_reader, &context)?;
            options_reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::Particles {
                particle: parse_key(name, "particle")?,
                long_distance: particles.override_limiter,
                always_show: particles.always_show,
                pos: Vec3 {
                    x: particles.x,
                    y: particles.y,
                    z: particles.z,
                },
                offset: Vec3f {
                    x: particles.x_dist,
                    y: particles.y_dist,
                    z: particles.z_dist,
                },
                speed: [particles.max_speed; 3],
                distribution: lodestone_model::ParticleDistribution::Default,
                count: particles.count,
                options,
            })]);
        }
        if packet_id == play::clientbound::EXPLODE {
            let registries = self.registries.lock().expect("client registries lock poisoned");
            return decode_explode(payload, &StackCodecContext::new(self.dialect, &registries));
        }
        if packet_id == play::clientbound::SOUND {
            return decode_sound(payload, self.dialect);
        }
        if packet_id == play::clientbound::SOUND_ENTITY {
            return decode_sound_entity(payload, self.dialect);
        }
        if packet_id == play::clientbound::STOP_SOUND {
            // A flags byte: bit 0 = a source category follows, bit 1 = a sound
            // identifier follows. Either, both, or neither may be present.
            let mut reader = Reader::new(payload);
            let flags = reader.u8().map_err(dec_err)?;
            let category = if flags & 0x1 != 0 {
                Some(read_sound_category(&mut reader)?)
            } else {
                None
            };
            let sound = if flags & 0x2 != 0 {
                let name = reader.string(32767).map_err(dec_err)?;
                Some(parse_key(&name, "sound")?)
            } else {
                None
            };
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::SoundStopped {
                sound,
                category,
            })]);
        }
        if packet_id == play::clientbound::SET_BORDER_CENTER {
            let mut reader = Reader::new(payload);
            let x = reader.f64().map_err(dec_err)?;
            let z = reader.f64().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(
                ClientEvent::WorldBorderCenterChanged { x, z },
            )]);
        }
        if packet_id == play::clientbound::SET_BORDER_LERP_SIZE {
            let mut reader = Reader::new(payload);
            let old_size = reader.f64().map_err(dec_err)?;
            let new_size = reader.f64().map_err(dec_err)?;
            let lerp_time_ms = reader.var_i64().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::WorldBorderSizeLerping {
                old_size,
                new_size,
                lerp_time_ms,
            })]);
        }
        if packet_id == play::clientbound::SET_BORDER_SIZE {
            let mut reader = Reader::new(payload);
            let size = reader.f64().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::WorldBorderSizeChanged {
                size,
            })]);
        }
        if packet_id == play::clientbound::SET_BORDER_WARNING_DELAY {
            let mut reader = Reader::new(payload);
            let warning_time = reader.var_i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(
                ClientEvent::WorldBorderWarningDelayChanged { warning_time },
            )]);
        }
        if packet_id == play::clientbound::SET_BORDER_WARNING_DISTANCE {
            let mut reader = Reader::new(payload);
            let warning_blocks = reader.var_i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(
                ClientEvent::WorldBorderWarningDistanceChanged { warning_blocks },
            )]);
        }
        if packet_id == play::clientbound::INITIALIZE_BORDER {
            let mut reader = Reader::new(payload);
            let x = reader.f64().map_err(dec_err)?;
            let z = reader.f64().map_err(dec_err)?;
            let old_size = reader.f64().map_err(dec_err)?;
            let new_size = reader.f64().map_err(dec_err)?;
            let lerp_time_ms = reader.var_i64().map_err(dec_err)?;
            let absolute_max_size = reader.var_i32().map_err(dec_err)?;
            let warning_blocks = reader.var_i32().map_err(dec_err)?;
            let warning_time = reader.var_i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::WorldBorderInitialized {
                x,
                z,
                old_size,
                new_size,
                lerp_time_ms,
                absolute_max_size,
                warning_blocks,
                warning_time,
            })]);
        }
        if packet_id == play::clientbound::GAME_RULE_VALUES {
            let mut reader = Reader::new(payload);
            let count = reader.var_i32().map_err(dec_err)?;
            let count = usize::try_from(count)
                .map_err(|_| AdapterError::Decode(format!("invalid game rule count {count}")))?;
            // Same cap as every other list decode here: `count` comes off the
            // wire and each rule costs at least one byte, so `remaining()` is
            // a sound ceiling on how many can exist. Reserving `count`
            // outright lets a tiny payload demand an unbounded allocation.
            let mut values = Vec::with_capacity(count.min(reader.remaining()));
            for _ in 0..count {
                let key = reader.string(32767).map_err(dec_err)?;
                let key = parse_key(&key, "game rule")?;
                let value = reader.string(32767).map_err(dec_err)?;
                values.push((key, value));
            }
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::GameRulesChanged {
                values,
            })]);
        }
        if packet_id == play::clientbound::DEBUG_BLOCK_VALUE {
            let mut reader = Reader::new(payload);
            let pos = unpack_block_pos(reader.i64().map_err(dec_err)?);
            let (subscription, value) = read_debug_update(&mut reader, self.dialect)?;
            return Ok(vec![Directive::Emit(ClientEvent::DebugBlockValue {
                pos,
                subscription,
                value,
            })]);
        }
        if packet_id == play::clientbound::DEBUG_CHUNK_VALUE {
            let mut reader = Reader::new(payload);
            // Vanilla's chunk-position stream codec is one packed long: low 32
            // bits x, high 32 bits z. Not two VarInts.
            let packed = reader.i64().map_err(dec_err)?;
            #[allow(clippy::cast_possible_truncation)]
            let chunk = ChunkPos {
                x: packed as i32,
                z: (packed >> 32) as i32,
            };
            let (subscription, value) = read_debug_update(&mut reader, self.dialect)?;
            return Ok(vec![Directive::Emit(ClientEvent::DebugChunkValue {
                chunk,
                subscription,
                value,
            })]);
        }
        if packet_id == play::clientbound::DEBUG_ENTITY_VALUE {
            let mut reader = Reader::new(payload);
            let entity_id = reader.var_i32().map_err(dec_err)?;
            let (subscription, value) = read_debug_update(&mut reader, self.dialect)?;
            return Ok(vec![Directive::Emit(ClientEvent::DebugEntityValue {
                entity_id,
                subscription,
                value,
            })]);
        }
        if packet_id == play::clientbound::DEBUG_EVENT {
            // Vanilla's debug-subscription event dispatches the same way its
            // update variant does but **without** the optional-value wrapper
            // — an event always has a value. Reusing `read_debug_update`
            // here would eat the first payload byte as a present-flag.
            let mut reader = Reader::new(payload);
            let subscription = read_debug_subscription_key(&mut reader, self.dialect)?;
            let value = reader.remaining_bytes().to_vec();
            return Ok(vec![Directive::Emit(ClientEvent::DebugEvent {
                subscription,
                value,
            })]);
        }
        if packet_id == play::clientbound::DEBUG_SAMPLE {
            let mut reader = Reader::new(payload);
            let count = reader.var_i32().map_err(dec_err)?;
            let count = usize::try_from(count)
                .map_err(|_| AdapterError::Decode(format!("invalid sample count {count}")))?;
            let mut sample = Vec::with_capacity(count.min(4096));
            for _ in 0..count {
                sample.push(reader.i64().map_err(dec_err)?);
            }
            let kind = match reader.var_i32().map_err(dec_err)? {
                0 => DebugSampleKind::TickTime,
                other => {
                    return Err(AdapterError::Decode(format!(
                        "unknown debug sample type {other}"
                    )));
                }
            };
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::DebugSample {
                sample,
                kind,
            })]);
        }
        if packet_id == play::clientbound::GAME_TEST_HIGHLIGHT_POS {
            let mut reader = Reader::new(payload);
            let absolute = unpack_block_pos(reader.i64().map_err(dec_err)?);
            let relative = unpack_block_pos(reader.i64().map_err(dec_err)?);
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::GameTestHighlightPos {
                absolute,
                relative,
            })]);
        }
        if packet_id == play::clientbound::WAYPOINT {
            return decode_waypoint(payload);
        }
        if packet_id == play::clientbound::TAG_QUERY {
            let mut reader = Reader::new(payload);
            let transaction_id = reader.var_i32().map_err(dec_err)?;
            // `writeNbt` writes a bare `TAG_End` byte (0) for null, so the tail
            // is either that one byte or a whole compound. Carried as raw bytes
            // rather than a parsed `Nbt` because a queried block entity's tag is
            // arbitrary server/datapack data with no schema this crate models.
            let tail = reader.remaining_bytes();
            let tag = if tail == [0u8] {
                None
            } else {
                Some(tail.to_vec())
            };
            return Ok(vec![Directive::Emit(ClientEvent::TagQueryResponse {
                transaction_id,
                tag,
            })]);
        }
        if packet_id == play::clientbound::TICKING_STATE {
            let mut reader = Reader::new(payload);
            let tick_rate = reader.f32().map_err(dec_err)?;
            let frozen = reader.bool().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::TickingStateChanged {
                tick_rate,
                frozen,
            })]);
        }
        if packet_id == play::clientbound::TICKING_STEP {
            let mut reader = Reader::new(payload);
            let tick_steps = reader.var_i32().map_err(dec_err)?;
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(ClientEvent::TickingStepped {
                tick_steps,
            })]);
        }
        if packet_id == play::clientbound::TEST_INSTANCE_BLOCK_STATUS {
            let mut reader = Reader::new(payload);
            let status = read_network_nbt(&mut reader).map_err(dec_err)?;
            let size = if reader.bool().map_err(dec_err)? {
                Some((
                    reader.var_i32().map_err(dec_err)?,
                    reader.var_i32().map_err(dec_err)?,
                    reader.var_i32().map_err(dec_err)?,
                ))
            } else {
                None
            };
            reader.ensure_empty().map_err(dec_err)?;
            return Ok(vec![Directive::Emit(
                ClientEvent::TestInstanceBlockStatus {
                    status: Text::from_nbt(&status),
                    size,
                },
            )]);
        }
        Ok(Vec::new())
    }

    /// Shared decoder for the normal sink path and deferred-load hook.
    pub(super) fn decode_level_chunk_with_light(
        &self,
        payload: &[u8],
    ) -> Result<DeferredChunkLoad, AdapterError> {
        // The framing depends on the current dimension's build-height window,
        // which is installed at login and is not carried by the packet.
        let shape = self.current_shape();
        let mut reader = Reader::new(payload);
        let mut chunk = LevelChunkWithLight::decode_for(&mut reader, &shape, self.dialect.game_data_version())
            .map_err(|err| AdapterError::Decode(err.to_string()))?;
        // Reject trailing bytes: a subtly wrong layout otherwise tends to
        // produce a plausible but truncated column.
        reader
            .ensure_empty()
            .map_err(|err| AdapterError::Decode(err.to_string()))?;
        for entity in &mut chunk.block_entities {
            entity.type_id = self.dialect.canonical_fixed_id(FixedRegistryKind::BlockEntity, entity.type_id as i32)? as u32;
        }

        let pos = ChunkPos::new(chunk.x, chunk.z);
        Ok(DeferredChunkLoad {
            position: WorldChunkPos::new(chunk.x, chunk.z),
            chunk: LoadedChunk::new(
                chunk.column,
                chunk.light,
                chunk.heightmaps,
                chunk.block_entities,
            ),
            directives: vec![Directive::Emit(ClientEvent::ChunkLoaded { pos })],
        })
    }
}

/// Unpacks vanilla's packed section-position long value into section-grid
/// coordinates.
///
/// The packing places `x` in bits 42–63 (22 bits), `z` in bits 20–41 (22 bits),
/// and `y` in bits 0–19 (20 bits), each a two's-complement signed field.
fn unpack_section_pos(packed: i64) -> (i32, i32, i32) {
    let x = (packed >> 42) as i32;
    let y = ((packed << 44) >> 44) as i32;
    let z = ((packed << 22) >> 42) as i32;
    (x, y, z)
}

/// Maps a vanilla game-mode ordinal to the canonical [`GameMode`], if valid.
///
/// `pub(crate)` because `server_protocol` decodes the *serverbound*
/// `change_game_mode` with it — the same id table, the other direction.
pub(crate) fn game_mode_from_ordinal(ordinal: i32) -> Option<GameMode> {
    match ordinal {
        0 => Some(GameMode::Survival),
        1 => Some(GameMode::Creative),
        2 => Some(GameMode::Adventure),
        3 => Some(GameMode::Spectator),
        _ => None,
    }
}

/// The fixed-point scale for `sound` packet positions: coordinates are sent as
/// `(int)(block * 8)`, so each unit is `1/8` of a block (`LOCATION_ACCURACY`).
const SOUND_POSITION_SCALE: f64 = 8.0;
/// Decodes a `Holder<SoundEvent>`, returning the sound's identifier and its
/// optional fixed audible range.
///
/// The holder is a VarInt: `0` introduces an inline definition (an identifier
/// then an optional `f32` range), and any positive value references the
/// `minecraft:sound_event` registry at index `value - 1`, whose range is a
/// property of the registry entry rather than the wire.
fn read_sound_holder(reader: &mut Reader<'_>, dialect: ProtocolDialect) -> Result<(String, Option<f32>), AdapterError> {
    let holder_id = reader.var_i32().map_err(dec_err)?;
    if holder_id == 0 {
        let name = reader.string(32767).map_err(dec_err)?;
        let range = if reader.bool().map_err(dec_err)? {
            Some(reader.f32().map_err(dec_err)?)
        } else {
            None
        };
        Ok((name, range))
    } else {
        let index = holder_id.checked_sub(1).filter(|_| holder_id > 0)
            .ok_or_else(|| AdapterError::Decode(format!("negative sound holder {holder_id}")))?;
        let index = dialect.canonical_fixed_id(FixedRegistryKind::Sound, index)?;
        SoundEventId::new(index)
            .map(super::sound_event)
            .map(|(name, range)| (name.to_owned(), range))
            .ok_or_else(|| AdapterError::Decode(format!("unknown sound event id {index}")))
    }
}

/// Reads vanilla's sound-source enum ordinal (a VarInt) as the canonical
/// [`SoundCategory`].
fn read_sound_category(reader: &mut Reader<'_>) -> Result<SoundCategory, AdapterError> {
    let ordinal = reader.var_i32().map_err(dec_err)?;
    u8::try_from(ordinal)
        .ok()
        .and_then(SoundCategory::from_ordinal)
        .ok_or_else(|| AdapterError::Decode(format!("invalid sound source ordinal {ordinal}")))
}

/// Decodes `sound`: a sound holder, a source category, a fixed-point position,
/// volume, pitch, and the server-rolled variant seed (forwarded untouched — the
/// variant is resolved client-side from the same seed so all clients agree).
fn decode_sound(payload: &[u8], dialect: ProtocolDialect) -> Result<Vec<Directive>, AdapterError> {
    let mut reader = Reader::new(payload);
    let (name, fixed_range) = read_sound_holder(&mut reader, dialect)?;
    let category = read_sound_category(&mut reader)?;
    let x = f64::from(reader.i32().map_err(dec_err)?) / SOUND_POSITION_SCALE;
    let y = f64::from(reader.i32().map_err(dec_err)?) / SOUND_POSITION_SCALE;
    let z = f64::from(reader.i32().map_err(dec_err)?) / SOUND_POSITION_SCALE;
    let volume = reader.f32().map_err(dec_err)?;
    let pitch = reader.f32().map_err(dec_err)?;
    let seed = reader.i64().map_err(dec_err)?;
    reader.ensure_empty().map_err(dec_err)?;
    Ok(vec![Directive::Emit(ClientEvent::Sound {
        sound: parse_key(&name, "sound")?,
        category,
        pos: Vec3 { x, y, z },
        volume,
        pitch,
        fixed_range,
        seed,
    })])
}

/// Decodes `sound_entity`: a sound holder, a source category, the entity id the
/// sound follows, volume, pitch, and the server-rolled variant seed.
fn decode_sound_entity(payload: &[u8], dialect: ProtocolDialect) -> Result<Vec<Directive>, AdapterError> {
    let mut reader = Reader::new(payload);
    let (name, fixed_range) = read_sound_holder(&mut reader, dialect)?;
    let category = read_sound_category(&mut reader)?;
    let entity_id = reader.var_i32().map_err(dec_err)?;
    let volume = reader.f32().map_err(dec_err)?;
    let pitch = reader.f32().map_err(dec_err)?;
    let seed = reader.i64().map_err(dec_err)?;
    reader.ensure_empty().map_err(dec_err)?;
    Ok(vec![Directive::Emit(ClientEvent::EntitySound {
        sound: parse_key(&name, "sound")?,
        category,
        entity_id,
        volume,
        pitch,
        fixed_range,
        seed,
    })])
}

/// Decodes the complete explosion body, including debris alignment and the
/// release-specific sound flag. Debris parameters are consumed but not drawn.
fn decode_explode(payload: &[u8], context: &StackCodecContext<'_>) -> Result<Vec<Directive>, AdapterError> {
    let mut reader = Reader::new(payload);
    let x = reader.f64().map_err(dec_err)?;
    let y = reader.f64().map_err(dec_err)?;
    let z = reader.f64().map_err(dec_err)?;
    let radius = reader.f32().map_err(dec_err)?;
    let _block_count = reader.i32().map_err(dec_err)?;
    let knockback = if reader.bool().map_err(dec_err)? {
        Some(Vec3::new(
            reader.f64().map_err(dec_err)?,
            reader.f64().map_err(dec_err)?,
            reader.f64().map_err(dec_err)?,
        ))
    } else {
        None
    };
    let (particle, options) = read_particle(&mut reader, context)?;
    let (name, fixed_range) = read_sound_holder(&mut reader, context.dialect)?;
    let count = reader.var_i32().map_err(dec_err)?;
    let count = usize::try_from(count)
        .map_err(|_| AdapterError::Decode(format!("negative explosion particle count {count}")))?;
    if count > reader.remaining() / 10 {
        return Err(AdapterError::Decode("explosion particle count exceeds readable entries".to_owned()));
    }
    for _ in 0..count {
        read_particle(&mut reader, context)?;
        reader.f32().map_err(dec_err)?;
        reader.f32().map_err(dec_err)?;
        reader.var_i32().map_err(dec_err)?;
    }
    let play_sound = if context.dialect.game_data_version() == lodestone_data::GameDataVersion::V26_3 {
        reader.bool().map_err(dec_err)?
    } else { true };
    reader.ensure_empty().map_err(dec_err)?;
    let mut directives = vec![
        Directive::Emit(ClientEvent::Explosion {
            pos: Vec3::new(x, y, z),
            radius,
            affected_blocks: Vec::new(),
            knockback,
        }),
        Directive::Emit(ClientEvent::Particles {
            particle: parse_key(particle, "particle")?,
            long_distance: false,
            always_show: false,
            pos: Vec3::new(x, y, z),
            offset: Vec3f::new(0.0, 0.0, 0.0),
            speed: [0.0; 3],
            distribution: lodestone_model::ParticleDistribution::Default,
            options,
            count: 1,
        }),
    ];
    if play_sound {
        directives.push(Directive::Emit(ClientEvent::Sound {
            sound: parse_key(&name, "sound")?,
            category: SoundCategory::Block,
            pos: Vec3::new(x, y, z),
            volume: 4.0,
            pitch: (1.0 + (rand::random::<f32>() - rand::random::<f32>()) * 0.2) * 0.7,
            fixed_range,
            seed: rand::random(),
        }));
    }
    Ok(directives)
}

/// Consumes exactly one type-specific payload before the following packet fields.
fn read_particle_options(name: &str, reader: &mut Reader<'_>, context: &StackCodecContext<'_>) -> Result<ParticleOptions, AdapterError> {
    fn rgb24(reader: &mut Reader<'_>) -> Result<[f32; 3], AdapterError> {
        let packed = reader.i32().map_err(dec_err)?;
        Ok([
            f32::from(((packed >> 16) & 0xff) as u8) / 255.0,
            f32::from(((packed >> 8) & 0xff) as u8) / 255.0,
            f32::from((packed & 0xff) as u8) / 255.0,
        ])
    }
    /// Vanilla's own ARGB component-unpack over one packed word — the same
    /// three low bytes [`rgb24`] reads, plus the top byte as alpha.
    fn argb(reader: &mut Reader<'_>) -> Result<[f32; 4], AdapterError> {
        let packed = reader.i32().map_err(dec_err)?;
        Ok([
            f32::from(((packed >> 16) & 0xff) as u8) / 255.0,
            f32::from(((packed >> 8) & 0xff) as u8) / 255.0,
            f32::from((packed & 0xff) as u8) / 255.0,
            f32::from(((packed >> 24) & 0xff) as u8) / 255.0,
        ])
    }
    // `name` is the fully-namespaced registry id (`PARTICLE_TYPE_NAMES`'
    // own entries, e.g. `"minecraft:dust"`), not the namespace-stripped path
    // `net.rs`'s `NetUpdate::Particles::kind` uses -- matching the bare path
    // here would silently fall through to `None` for every particle.
    match name {
        "minecraft:dust" => {
            let color = rgb24(reader)?;
            let scale = reader.f32().map_err(dec_err)?;
            Ok(ParticleOptions::Dust { color, scale })
        }
        "minecraft:dust_color_transition" => {
            let from_color = rgb24(reader)?;
            let to_color = rgb24(reader)?;
            let scale = reader.f32().map_err(dec_err)?;
            Ok(ParticleOptions::DustColorTransition { from_color, to_color, scale })
        }
        "minecraft:effect" | "minecraft:instant_effect" => {
            let color = rgb24(reader)?;
            let power = reader.f32().map_err(dec_err)?;
            Ok(ParticleOptions::Spell { color, power })
        }
        "minecraft:entity_effect" | "minecraft:tinted_leaves" | "minecraft:flash" => {
            let color = argb(reader)?;
            Ok(ParticleOptions::Color { color })
        }
        "minecraft:dragon_breath" => {
            let power = reader.f32().map_err(dec_err)?;
            Ok(ParticleOptions::Power { power })
        }
        "minecraft:sculk_charge" => {
            let roll = reader.f32().map_err(dec_err)?;
            Ok(ParticleOptions::SculkCharge { roll })
        }
        // The whole block-particle-option family, in one arm because the wire
        // payload really is one type — a registry-indexed mapper over the
        // block-state registry, i.e. a single **VarInt** block-state id, not
        // the fixed-width `INT` every arm above reads. The five differ only in
        // which particle class the provider builds from it.
        "minecraft:block"
        | "minecraft:block_marker"
        | "minecraft:falling_dust"
        | "minecraft:dust_pillar"
        | "minecraft:block_crumble" => {
            let raw = reader.var_i32().map_err(dec_err)?;
            let state = u32::try_from(raw).map_err(|_| {
                AdapterError::Decode(format!(
                    "{name}: block-state id {raw} is negative — \
                     vanilla's own block-state registry ids are non-negative"
                ))
            })?;
            let state = context.dialect.game_data_version().state_from_wire(state)
                .ok_or_else(|| AdapterError::Decode(format!("unknown particle block state id {state}")))?;
            Ok(ParticleOptions::BlockState {
                state: BlockStateRef::canonical(state.raw()),
            })
        }
        "minecraft:geyser" | "minecraft:geyser_plume" => {
            reader.i32().map_err(dec_err)?;
            Ok(ParticleOptions::None)
        }
        "minecraft:geyser_base" | "minecraft:geyser_poof" => {
            reader.i32().map_err(dec_err)?;
            reader.f32().map_err(dec_err)?;
            Ok(ParticleOptions::None)
        }
        "minecraft:item" => {
            read_item_stack_template_with(reader, context)?;
            Ok(ParticleOptions::None)
        }
        "minecraft:vibration" => {
            let raw = reader.var_i32().map_err(dec_err)?;
            match context.dialect.canonical_fixed_id(FixedRegistryKind::PositionSource, raw)? {
                0 => { reader.i64().map_err(dec_err)?; }
                1 => {
                    reader.var_i32().map_err(dec_err)?;
                    reader.f32().map_err(dec_err)?;
                }
                other => return Err(AdapterError::Decode(format!("unmodeled position source {other}"))),
            }
            reader.var_i32().map_err(dec_err)?;
            Ok(ParticleOptions::None)
        }
        "minecraft:trail" => {
            for _ in 0..3 { reader.f64().map_err(dec_err)?; }
            reader.i32().map_err(dec_err)?;
            reader.var_i32().map_err(dec_err)?;
            Ok(ParticleOptions::None)
        }
        "minecraft:shriek" => {
            reader.var_i32().map_err(dec_err)?;
            Ok(ParticleOptions::None)
        }
        _ => Ok(ParticleOptions::None),
    }
}

pub(crate) fn read_particle(
    reader: &mut Reader<'_>, context: &StackCodecContext<'_>,
) -> Result<(&'static str, ParticleOptions), AdapterError> {
    let raw = reader.var_i32().map_err(dec_err)?;
    let canonical = context.dialect.canonical_fixed_id(FixedRegistryKind::Particle, raw)?;
    let particle = ParticleTypeId::new(canonical)
        .ok_or_else(|| AdapterError::Decode(format!("unknown particle id {raw}")))?;
    let name = particle_type_name(particle);
    let options = read_particle_options(name, reader, context)?;
    Ok((name, options))
}

fn decode_latest_particles(
    payload: &[u8], context: &StackCodecContext<'_>,
) -> Result<Vec<Directive>, AdapterError> {
    let mut reader = Reader::new(payload);
    let (name, options) = read_particle(&mut reader, context)?;
    let body = crate::packets::release_layout::ParticleSpawn::decode(&mut reader, CTX).map_err(dec_err)?;
    reader.ensure_empty().map_err(dec_err)?;
    use crate::packets::release_layout::ParticleDistribution as WireDistribution;
    let distribution = match body.distribution {
        WireDistribution::Default => lodestone_model::ParticleDistribution::Default,
        WireDistribution::Alternative => lodestone_model::ParticleDistribution::Alternative,
        WireDistribution::AlternativeWithSpeed => lodestone_model::ParticleDistribution::AlternativeWithSpeed,
    };
    Ok(vec![Directive::Emit(ClientEvent::Particles {
        particle: parse_key(name, "particle")?, long_distance: body.override_limiter,
        always_show: body.always_show,
        pos: Vec3::new(body.pos[0], body.pos[1], body.pos[2]),
        offset: Vec3f::new(body.offset[0], body.offset[1], body.offset[2]),
        speed: body.speed, distribution, count: body.count, options,
    })])
}

/// Reads a wire `BitSet` in the release's framing (see [`BitSetWire`]) and
/// returns it as LSB-first 64-bit words for [`LightPatch::from_light_masks`]
/// to index. The count is bounded by the readable units so a garbled length
/// cannot pre-allocate an enormous vector.
fn read_wire_bitset(r: &mut Reader<'_>, wire: BitSetWire) -> Result<Vec<u64>, AdapterError> {
    let count = r.var_i32().map_err(dec_err)?;
    let count = usize::try_from(count)
        .map_err(|_| AdapterError::Decode(format!("negative bitset length {count}")))?;
    let unit_bytes = match wire {
        BitSetWire::Longs => 8,
        BitSetWire::Bytes => 1,
    };
    if count > r.remaining() / unit_bytes {
        return Err(AdapterError::Decode(format!(
            "bitset length {count} exceeds {} readable units",
            r.remaining() / unit_bytes
        )));
    }
    let mut words = vec![0u64; (count * unit_bytes).div_ceil(8)];
    for index in 0..count {
        match wire {
            BitSetWire::Longs => words[index] = r.u64().map_err(dec_err)?,
            BitSetWire::Bytes => {
                words[index / 8] |= u64::from(r.u8().map_err(dec_err)?) << (8 * (index % 8));
            }
        }
    }
    Ok(words)
}

/// Reads a `light_update` nibble-array list: a varint element count, then each
/// element as a varint byte-length plus that many bytes, validated to be
/// exactly 2048 by [`NibbleArray::from_bytes`]. The count is bounded by the
/// readable bytes (each element is at least one byte) to cap pre-allocation.
fn read_light_arrays(r: &mut Reader<'_>) -> Result<Vec<NibbleArray>, AdapterError> {
    let count = r.var_i32().map_err(dec_err)?;
    let count = usize::try_from(count)
        .map_err(|_| AdapterError::Decode(format!("negative light-array count {count}")))?;
    if count > r.remaining() {
        return Err(AdapterError::Decode(format!(
            "light-array count {count} exceeds {} readable bytes",
            r.remaining()
        )));
    }
    let mut arrays = Vec::with_capacity(count);
    for _ in 0..count {
        let len = r.var_i32().map_err(dec_err)?;
        let len = usize::try_from(len)
            .map_err(|_| AdapterError::Decode(format!("negative light-array length {len}")))?;
        let bytes = r.bytes(len).map_err(dec_err)?;
        arrays.push(NibbleArray::from_bytes(bytes).map_err(dec_err)?);
    }
    Ok(arrays)
}

/// Reads vanilla's debug-subscription update dispatch head: the
/// subscription's registry id resolved to its identifier, then the
/// optional-value wrapper's present-flag, then the rest of the payload as
/// opaque bytes.
///
/// The payload is opaque because the value codec is chosen per registry entry and
/// the seventeen registered ones share no shape — one (`dedicated_server_tick_time`)
/// has a `null` value codec and throws if it is ever sent this way. See
/// `lodestone_game::debug_feeds`' module doc.
fn read_debug_update(
    reader: &mut Reader<'_>,
    dialect: ProtocolDialect,
) -> Result<(ResourceKey, Option<Vec<u8>>), AdapterError> {
    let subscription = read_debug_subscription_key(reader, dialect)?;
    let present = reader.bool().map_err(dec_err)?;
    let value = if present {
        Some(reader.remaining_bytes().to_vec())
    } else {
        None
    };
    Ok((subscription, value))
}

/// Reads a `minecraft:debug_subscription` registry id and resolves it.
///
/// An unknown id is a decode **error** rather than a synthetic key: the id is the
/// dispatch discriminant, so not knowing it means the bytes after it cannot be
/// attributed, and inventing `lodestone:unknown_7` would let two different feeds
/// collide in the store.
fn read_debug_subscription_key(reader: &mut Reader<'_>, dialect: ProtocolDialect) -> Result<ResourceKey, AdapterError> {
    let id = reader.var_i32().map_err(dec_err)?;
    let canonical = dialect.canonical_fixed_id(FixedRegistryKind::DebugSubscription, id)?;
    let name = dialect.fixed_registry_name(FixedRegistryKind::DebugSubscription, canonical)
        .or_else(|| crate::stat_debug_registries::debug_subscription_name(canonical)).ok_or_else(|| {
        AdapterError::Decode(format!("unknown debug_subscription registry id {id}"))
    })?;
    parse_key(name, "debug subscription")
}

/// Decodes the clientbound tracked-waypoint packet and its hand-written
/// waypoint-position writer.
///
/// The position is a four-way tagged union, not an optional: `EMPTY` carries
/// nothing, `VEC3I` three VarInts, `CHUNK` two, and `AZIMUTH` one f32 bearing.
/// Vanilla degrades to the coarser forms with distance, so a decoder that treated
/// anything but `VEC3I` as "no position" would blank the locator bar at range.
fn decode_waypoint(payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
    let mut reader = Reader::new(payload);
    let operation = match reader.var_i32().map_err(dec_err)? {
        0 => WaypointOperation::Track,
        1 => WaypointOperation::Untrack,
        2 => WaypointOperation::Update,
        other => {
            return Err(AdapterError::Decode(format!(
                "unknown waypoint operation {other}"
            )));
        }
    };
    let id = if reader.bool().map_err(dec_err)? {
        WaypointId::Entity(reader.uuid().map_err(dec_err)?)
    } else {
        WaypointId::Named(reader.string(32767).map_err(dec_err)?)
    };
    let style = parse_key(&reader.string(32767).map_err(dec_err)?, "waypoint style")?;
    let color = if reader.bool().map_err(dec_err)? {
        // `vanilla's own byte buf codecs's own rgb color` is a plain big-endian int.
        #[allow(clippy::cast_sign_loss)]
        Some(reader.i32().map_err(dec_err)? as u32)
    } else {
        None
    };
    let position = match reader.var_i32().map_err(dec_err)? {
        0 => WaypointPosition::Empty,
        1 => WaypointPosition::Exact(BlockPos {
            x: reader.var_i32().map_err(dec_err)?,
            y: reader.var_i32().map_err(dec_err)?,
            z: reader.var_i32().map_err(dec_err)?,
        }),
        2 => WaypointPosition::Chunk(ChunkPos {
            x: reader.var_i32().map_err(dec_err)?,
            z: reader.var_i32().map_err(dec_err)?,
        }),
        3 => WaypointPosition::Azimuth(reader.f32().map_err(dec_err)?),
        other => {
            return Err(AdapterError::Decode(format!(
                "unknown waypoint position type {other}"
            )));
        }
    };
    reader.ensure_empty().map_err(dec_err)?;
    Ok(vec![Directive::Emit(ClientEvent::WaypointUpdated {
        operation,
        waypoint: TrackedWaypoint {
            id,
            style,
            color,
            position,
        },
    })])
}
