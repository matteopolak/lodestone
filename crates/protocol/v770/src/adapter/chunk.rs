//! World/chunk packets: chunk and light streaming, block updates, particles,
//! sound, world border, time, and the debug/gametest overlay packets. Split
//! out of the former monolithic `adapter.rs`.
use super::*;
use super::player::game_mode;

impl V770Adapter {
    /// Clientbound play-state packets in the chunk domain, split out of the
    /// former monolithic `handle_play` (see `adapter::mod` for the coordinator).
    pub(super) fn handle_play_chunk(&self, world: &mut dyn WorldSink, packet_id: i32, payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
        if packet_id == play::clientbound::LOGIN {
            let body: GameLogin = decode_body(payload)?;
            // `dimension_type` is the registry holder id; `dimension` is the
            // level name. The id wins where the registry resolved it, and
            // `enter_dimension` falls back to the name match where it did not.
            let dimension_type = self.enter_dimension(body.dimension_type, &body.dimension);
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
            // `ClientboundChunksBiomesPacket` (id 13): a VarInt-prefixed list of
            // `(ChunkPos, byte[])` entries. Vanilla sends this to *resend* biomes
            // for chunks a player already has loaded — `ChunkMap.
            // resendBiomesForChunks`, whose only caller is `/fillbiome`
            // (`FillBiomeCommand.java`) — never at initial load, which is why the
            // per-section biome container already rides `level_chunk_with_light`
            // and this packet only ever *updates* it.
            //
            // Each entry's byte array is, per `ChunkBiomeData.extractChunkData`,
            // every section's `PalettedContainer<Holder<Biome>>.write` back to
            // back with **no other framing at all** — no non-air/fluid counts (it
            // has no blocks to count), no block-state container, just
            // `section_count` biome containers in ascending section order. That
            // makes this the one chunk-shaped packet whose per-section loop is
            // *shorter* than `level_chunk_with_light`'s, not a variant of it.
            let shape = self.current_shape();
            let mut reader = Reader::new(payload);
            let count = reader.var_i32().map_err(dec_err)?;
            let count = usize::try_from(count)
                .map_err(|_| AdapterError::Decode(format!("negative chunk-biomes count {count}")))?;
            let mut directives = Vec::with_capacity(count.min(1024));
            for _ in 0..count {
                // `ChunkPos.pack`/`unpack`: x in the low 32 bits, z in the high 32
                // — the same layout `forget_level_chunk` already unpacks.
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
            // The chunk framing depends on the current dimension's build-height
            // window (set at login), which is not carried in the packet itself.
            let shape = self.current_shape();
            let mut reader = Reader::new(payload);
            let chunk = LevelChunkWithLight::decode(&mut reader, &shape)
                .map_err(|err| AdapterError::Decode(err.to_string()))?;
            // Zero trailing bytes across the whole packet is the single best
            // detector of a subtly wrong layout: a misparse almost always
            // leaves the buffer misaligned, so reject rather than apply a
            // silently truncated chunk.
            reader
                .ensure_empty()
                .map_err(|err| AdapterError::Decode(err.to_string()))?;
            // Apply the fully decoded chunk (blocks, biomes, light, heightmaps,
            // block entities) straight into the client-owned world, moving each
            // part with no clone. The event then carries only the position.
            let pos = ChunkPos::new(chunk.x, chunk.z);
            world.load(
                WorldChunkPos::new(chunk.x, chunk.z),
                LoadedChunk::new(
                    chunk.column,
                    chunk.light,
                    chunk.heightmaps,
                    chunk.block_entities,
                ),
            );
            return Ok(vec![Directive::Emit(ClientEvent::ChunkLoaded { pos })]);
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
            let sky_mask = read_wire_bitset(&mut reader)?;
            let block_mask = read_wire_bitset(&mut reader)?;
            let empty_sky_mask = read_wire_bitset(&mut reader)?;
            let empty_block_mask = read_wire_bitset(&mut reader)?;
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
            world.merge_light(WorldChunkPos::new(x, z), patch);
            // `ChunkLoaded` doubles as "the region at pos is dirty; re-read or
            // re-mesh it" (its own docs) — exactly what a light change needs.
            return Ok(vec![Directive::Emit(ClientEvent::ChunkLoaded {
                pos: ChunkPos::new(x, z),
            })]);
        }
        if packet_id == play::clientbound::FORGET_LEVEL_CHUNK {
            // A single packed long: x in the low 32 bits, z in the high 32
            // (`ChunkPos.pack`, verified against 26.2 source).
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
            world.set_block(pos.x, pos.y, pos.z, state);
            // Writing a block state is what creates (or destroys) a block
            // entity: vanilla does it inside `LevelChunk.setBlockState`, with no
            // packet involved. Skipping this leaves
            // a placed chest with a state, no record, and zero pixels,
            // which still *opened* because interaction reads the state.
            // `World::sync_block_entity` documents the create/keep/replace/remove
            // rule; the `Option` is the version-specific half.
            world.sync_block_entity(pos.x, pos.y, pos.z, block_entity_type(state));
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
                let rel_x = ((local >> 8) & 0xF) as u8;
                let rel_z = ((local >> 4) & 0xF) as u8;
                let rel_y = (local & 0xF) as u8;
                blocks.push((rel_x, rel_y, rel_z, state));
            }
            reader.ensure_empty().map_err(dec_err)?;
            world.set_blocks(section_x, section_y, section_z, &blocks);
            // Every state write goes through `sync_block_entity`, one call per
            // changed cell, for the same reason `BLOCK_UPDATE` does: in vanilla
            // `LevelChunk.setBlockState` is what creates and removes block
            // entities, no packet involved. A piston
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
                    block_entity_type(state),
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
            // deliberate divergence: vanilla's `handleBlockEntityData` drops the
            // payload when `getBlockEntity(pos, type)` is empty
            // (`ClientPacketListener.java`, `BlockGetter.java`) because
            // it has `pendingBlockEntities` to promote from later, and we do not.
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
            let name = block_type_name(block_id)
                .ok_or_else(|| AdapterError::Decode(format!("unknown block id {block_id}")))?;
            return Ok(vec![Directive::Emit(ClientEvent::BlockEvent {
                pos: unpack_block_pos(packed),
                b0,
                b1,
                block: parse_key(name, "block")?,
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
                sequence,
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
                // portal after the dragon fight. Vanilla's own handler
                // (`ClientPacketListener.handleGameEvent`'s `WIN_GAME` arm)
                // ignores `param` for this event and always opens
                // `WinScreen` with `poem = true`, so nothing from
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
            return Ok(vec![Directive::Emit(ClientEvent::LevelEvent {
                event: level_event.event,
                pos: unpack_block_pos(level_event.position),
                data: level_event.data,
                global: level_event.global,
            })]);
        }
        if packet_id == play::clientbound::LEVEL_PARTICLES {
            // The particle type is the final field: a registry id followed by
            // per-type option bytes the model does not carry. The prefix decodes
            // to fixed widths (so a misparse is caught before the id) and the
            // options are swallowed by `remaining`.
            let particles: LevelParticles = decode_full(payload)?;
            let name = particle_type_name(particles.particle_id).ok_or_else(|| {
                AdapterError::Decode(format!("unknown particle id {}", particles.particle_id))
            })?;
            return Ok(vec![Directive::Emit(ClientEvent::Particles {
                particle: parse_key(name, "particle")?,
                long_distance: particles.override_limiter,
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
                max_speed: particles.max_speed,
                count: particles.count,
            })]);
        }
        if packet_id == play::clientbound::EXPLODE {
            return decode_explode(payload);
        }
        if packet_id == play::clientbound::SOUND {
            return decode_sound(payload);
        }
        if packet_id == play::clientbound::SOUND_ENTITY {
            return decode_sound_entity(payload);
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
            let (subscription, value) = read_debug_update(&mut reader)?;
            return Ok(vec![Directive::Emit(ClientEvent::DebugBlockValue {
                pos,
                subscription,
                value,
            })]);
        }
        if packet_id == play::clientbound::DEBUG_CHUNK_VALUE {
            let mut reader = Reader::new(payload);
            // `ChunkPos.STREAM_CODEC` is one packed long: low 32 bits x, high 32
            // bits z (`ChunkPos.unpack`). Not two VarInts.
            let packed = reader.i64().map_err(dec_err)?;
            #[allow(clippy::cast_possible_truncation)]
            let chunk = ChunkPos {
                x: packed as i32,
                z: (packed >> 32) as i32,
            };
            let (subscription, value) = read_debug_update(&mut reader)?;
            return Ok(vec![Directive::Emit(ClientEvent::DebugChunkValue {
                chunk,
                subscription,
                value,
            })]);
        }
        if packet_id == play::clientbound::DEBUG_ENTITY_VALUE {
            let mut reader = Reader::new(payload);
            let entity_id = reader.var_i32().map_err(dec_err)?;
            let (subscription, value) = read_debug_update(&mut reader)?;
            return Ok(vec![Directive::Emit(ClientEvent::DebugEntityValue {
                entity_id,
                subscription,
                value,
            })]);
        }
        if packet_id == play::clientbound::DEBUG_EVENT {
            // `DebugSubscription.Event` dispatches the same way `Update` does but
            // **without** the `ByteBufCodecs.optional` wrapper — an event always
            // has a value. Reusing `read_debug_update` here would eat the first
            // payload byte as a present-flag.
            let mut reader = Reader::new(payload);
            let subscription = read_debug_subscription_key(&mut reader)?;
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
}

/// Unpacks a vanilla `SectionPos.asLong` value into section-grid coordinates.
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
fn read_sound_holder(reader: &mut Reader<'_>) -> Result<(String, Option<f32>), AdapterError> {
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
        let index = holder_id - 1;
        sound_event(index)
            .map(|(name, range)| (name.to_owned(), range))
            .ok_or_else(|| AdapterError::Decode(format!("unknown sound event id {index}")))
    }
}

/// Reads a `SoundSource` enum ordinal (a VarInt) as the canonical
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
fn decode_sound(payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
    let mut reader = Reader::new(payload);
    let (name, fixed_range) = read_sound_holder(&mut reader)?;
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
fn decode_sound_entity(payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
    let mut reader = Reader::new(payload);
    let (name, fixed_range) = read_sound_holder(&mut reader)?;
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

/// The `explosion_emitter`/`explosion` particle registry ids — the two
/// "simple" (argument-less) particle types every `Level.explode` call site
/// passes as `explosionParticle` (`Level.java`, all
/// `ParticleTypes.EXPLOSION_EMITTER`; `ServerExplosion`'s small/large split
/// can also choose `ParticleTypes.EXPLOSION`).
/// `ParticleTypes.STREAM_CODEC` dispatches on a registry id
/// (`ByteBufCodecs.registry(Registries.PARTICLE_TYPE)`, a plain 0-based
/// VarInt — **not** the `id + 1` "holder" scheme [`read_sound_holder`] and the
/// villager-data field use), and a `SimpleParticleType`'s own stream codec
/// reads no further bytes. Recognising just these two ids and rejecting
/// everything else is therefore sufficient to stay byte-aligned through this
/// field without modelling the full particle-options codec (dust colour,
/// block state, item stack, …) that `metadata.rs`'s `SER_PARTICLE`/
/// `SER_PARTICLES` already reject for the identical reason.
const PARTICLE_ID_EXPLOSION_EMITTER: i32 = 29;
const PARTICLE_ID_EXPLOSION: i32 = 30;
/// Decodes `explode` (protocol id 36): a creeper/TNT/bed/respawn-anchor
/// detonation, `ClientboundExplodePacket`.
///
/// # Server-sent, not client-predicted
///
/// Unlike a player's own block break (`e2544b9`: no level event is ever sent
/// at all, and the sound is predicted), an explosion's sound rides explicitly
/// on this packet's `explosionSound` field, and
/// `ClientPacketListener.handleExplosion` (`ClientPacketListener.java`)
/// does nothing but play exactly what the server sent, at a
/// **client-rolled** pitch:
///
/// ```text
/// playLocalSound(center, packet.explosionSound(), SoundSource.BLOCKS, 4.0F,
///     (1.0F + (random.nextFloat() - random.nextFloat()) * 0.2F) * 0.7F, false)
/// ```
///
/// `volume` (`4.0`) is a client-side constant, never on the wire. `pitch` is
/// rolled by vanilla's own client from local randomness and is not on the
/// wire either — so this decoder rolls the identical die rather than
/// inventing a fixed pitch. A real client's explosion pitch already varies
/// run to run; a constant here would be *less* faithful, not more.
///
/// # What this does not decode
///
/// `radius`, `blockCount` and `playerKnockback` are consumed for wire
/// alignment only — no consumer today. `explosionParticle` is consumed via
/// the narrow allowlist above. `blockParticles` (the flying-debris
/// `WeightedList<ExplosionParticleInfo>`) is **not** decoded at all:
/// `explosionSound` is the second-to-last field the packet carries, so once
/// it is read there is nothing left this seam needs, and modelling
/// `ExplosionParticleInfo`'s own nested particle-options codec would cost
/// real complexity for zero consumers. This is therefore one of the packets
/// that does not run the trailing-bytes misparse check — like `metadata.rs`'s
/// partial item-stack decode, deliberately, not an oversight.
///
/// The flying block-debris particles (`blockParticles`) remain unimplemented
/// for the reason above. The shockwave/smoke visual itself is implemented:
/// this decoder now also emits a `ClientEvent::Particles` directive for
/// `explosion_emitter` (`ParticleTypes.EXPLOSION_EMITTER`, the id this
/// packet actually carries — `HugeExplosionSeedParticle` is what schedules
/// the follow-up `HugeExplosionParticle`s vanilla-side, per
/// `docs/particle-catalogue.md`'s "Built" entry), alongside the
/// existing `Sound` directive. `net.rs`/`sim.rs` need no new arm: this
/// crate's `ClientEvent::Particles` already forwards generically into
/// `Particles::spawn_particles`.
fn decode_explode(payload: &[u8]) -> Result<Vec<Directive>, AdapterError> {
    let mut reader = Reader::new(payload);
    let x = reader.f64().map_err(dec_err)?;
    let y = reader.f64().map_err(dec_err)?;
    let z = reader.f64().map_err(dec_err)?;
    let _radius = reader.f32().map_err(dec_err)?;
    let _block_count = reader.i32().map_err(dec_err)?;
    if reader.bool().map_err(dec_err)? {
        // `playerKnockback: Optional<Vec3>` — consumed, not applied yet.
        reader.f64().map_err(dec_err)?;
        reader.f64().map_err(dec_err)?;
        reader.f64().map_err(dec_err)?;
    }
    let particle_id = reader.var_i32().map_err(dec_err)?;
    if particle_id != PARTICLE_ID_EXPLOSION_EMITTER && particle_id != PARTICLE_ID_EXPLOSION {
        return Err(AdapterError::Decode(format!(
            "explode: unmodeled explosionParticle registry id {particle_id} (only \
             explosion_emitter/explosion are simple enough to skip byte-accurately)"
        )));
    }
    let (name, fixed_range) = read_sound_holder(&mut reader)?;
    // `blockParticles` follows and is deliberately not decoded — see the
    // function doc above. No `reader.ensure_empty()` call here on purpose.
    //
    // The shockwave/smoke visual, alongside the sound below.
    // Always `explosion_emitter` regardless of which of the two ids this
    // packet carried — `HugeExplosionSeedParticle` is what schedules the
    // follow-up `HugeExplosionParticle`s client-side (see
    // `Particle::tick_huge_explosion_seed`), so the seed is the one real
    // vanilla explosions actually spawn from this packet.
    Ok(vec![
        Directive::Emit(ClientEvent::Particles {
            particle: parse_key("explosion_emitter", "particle")?,
            long_distance: false,
            pos: Vec3::new(x, y, z),
            offset: Vec3f::new(0.0, 0.0, 0.0),
            max_speed: 0.0,
            count: 1,
        }),
        Directive::Emit(ClientEvent::Sound {
            sound: parse_key(&name, "sound")?,
            category: SoundCategory::Block,
            pos: Vec3::new(x, y, z),
            volume: 4.0,
            pitch: (1.0 + (rand::random::<f32>() - rand::random::<f32>()) * 0.2) * 0.7,
            fixed_range,
            seed: rand::random(),
        }),
    ])
}

/// Reads a wire `BitSet` — a varint `long`-count followed by that many
/// big-endian 64-bit words (`BitSet.toLongArray()`, LSB-first bit order) —
/// returning the words for [`LightPatch::from_light_masks`] to index. The count
/// is bounded by the readable words so a garbled length cannot pre-allocate an
/// enormous vector.
fn read_wire_bitset(r: &mut Reader<'_>) -> Result<Vec<u64>, AdapterError> {
    let count = r.var_i32().map_err(dec_err)?;
    let count = usize::try_from(count)
        .map_err(|_| AdapterError::Decode(format!("negative bitset long-count {count}")))?;
    if count > r.remaining() / 8 {
        return Err(AdapterError::Decode(format!(
            "bitset long-count {count} exceeds {} readable words",
            r.remaining() / 8
        )));
    }
    let mut words = Vec::with_capacity(count);
    for _ in 0..count {
        words.push(r.u64().map_err(dec_err)?);
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

/// Reads a `DebugSubscription.Update`'s dispatch head: the subscription's
/// registry id resolved to its identifier, then `ByteBufCodecs.optional`'s
/// present-flag, then the rest of the payload as opaque bytes.
///
/// The payload is opaque because the value codec is chosen per registry entry and
/// the seventeen registered ones share no shape — one (`dedicated_server_tick_time`)
/// has a `null` value codec and throws if it is ever sent this way. See
/// `lodestone_game::debug_feeds`' module doc.
fn read_debug_update(
    reader: &mut Reader<'_>,
) -> Result<(ResourceKey, Option<Vec<u8>>), AdapterError> {
    let subscription = read_debug_subscription_key(reader)?;
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
fn read_debug_subscription_key(reader: &mut Reader<'_>) -> Result<ResourceKey, AdapterError> {
    let id = reader.var_i32().map_err(dec_err)?;
    let name = crate::stat_debug_registries::debug_subscription_name(id).ok_or_else(|| {
        AdapterError::Decode(format!("unknown debug_subscription registry id {id}"))
    })?;
    parse_key(name, "debug subscription")
}

/// Decodes `ClientboundTrackedWaypointPacket` and its hand-written
/// `TrackedWaypoint.write`.
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
        // `ByteBufCodecs.RGB_COLOR` is a plain big-endian int.
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

