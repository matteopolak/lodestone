/// A [`ChunkSource`] that records every coordinate it is asked to generate.
///
/// Exists for [`generation_is_anchored_at_the_player_not_at_the_origin`]: the
/// comparison is between enumerating from `(0, 0)` outward and enumerating from
/// the player, which would make each recenter do more work as the player moves.
/// That is a claim about
/// *which coordinates are generated*, and nothing that counts columns can answer
/// it — only something that records the coordinates themselves.
struct RecordingSource {
    seen: std::sync::Arc<std::sync::Mutex<Vec<(i32, i32)>>>,
}

impl ChunkSource for RecordingSource {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        self.seen.lock().expect("recording lock").push((cx, cz));
        let mut column = ChunkColumn::new(0, 16);
        for lx in 0..16 {
            for lz in 0..16 {
                column.set_block(lx, 8, lz, "minecraft:stone");
            }
        }
        column
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        // Deliberately does NOT record: only whole-column generation is the
        // subject, and a spawn-position probe reading single blocks near the
        // origin would otherwise show up as origin-anchored generation and make
        // this test lie in the alarming direction.
        let _ = (x, y, z);
        if y == 8 { "minecraft:stone".to_string() } else { "minecraft:air".to_string() }
    }

    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        "minecraft:plains".to_string()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
        // No storage; edits are discarded by design.
    }
}

/// **Generation is anchored at the player, not at `(0, 0)`** — instrumented on
/// the real `serve_connection` path at a coordinate 80,000 blocks from spawn.
///
/// This distinguishes player-anchored generation from origin-anchored
/// generation at a coordinate 80,000 blocks from spawn.
/// [`player_moved_streams_view_across_several_chunk_boundaries`] already gates
/// the exact-diff shape, but it does so at chunk 10–11 (160 blocks), which is
/// close enough to the origin that an origin-anchored enumeration and a
/// player-anchored one would produce sets of similar size. At chunk 5,000 the two
/// hypotheses differ by five orders of magnitude in column count, so the
/// measurement is unambiguous.
///
/// The assertion is on the recorded coordinate *set*, and it has two halves,
/// because either alone is satisfiable by a wrong implementation:
///
/// * every column generated for the far recenter lies within the view radius of
///   the player — an origin-anchored spiral would generate columns near `(0, 0)`;
/// * the *count* is exactly the window size — an implementation that generated
///   the whole rectangle between the origin and the player would satisfy the
///   first half for its final columns while doing 5,000× the work.
///
/// The count half is a magnitude assertion, not
/// a direction: the expected value is derived from the view radius (`(2r+1)²`)
/// rather than read off a measurement.
#[tokio::test]
async fn generation_is_anchored_at_the_player_not_at_the_origin() {
    let view_radius = 1; // 3x3 = 9 columns
    let (client_end, server_end) = memory_pair();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let source = RecordingSource {
        seen: std::sync::Arc::clone(&seen),
    };

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(
            &mut conn,
            &FakeProtocol,
            &source,
            &NoEntities,
            view_radius,
            &BlockEntityHandle::default(),
            &MobHandle::default(),
        )
        .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "FarWalker", 9).await;
    send_chunk_batch_received(&mut client, 10.0).await;

    // Everything generated for the join is not this test's subject. Clear it so
    // the far recenter's own coordinate set is read in isolation.
    seen.lock().expect("recording lock").clear();

    // Chunk (5000, 0) — 80,000 blocks from the origin, where the two strategies
    // have sharply different generation costs.
    const FAR_CHUNK: i32 = 5000;
    send_player_moved(&mut client, f64::from(FAR_CHUNK) * 16.0, 64.0, 0.0).await;
    let far = drain_available(&mut client).await;

    let (center, _forgotten, added) = split_view_directives(&far);
    assert_eq!(
        center,
        Some((FAR_CHUNK, 0)),
        "the cache centre must follow the player"
    );
    assert_eq!(
        added,
        square(FAR_CHUNK, 0, view_radius),
        "exactly the player's own window must be sent"
    );

    let recorded = seen.lock().expect("recording lock").clone();

    // Half one: nothing outside the player's window was generated at all.
    let window = square(FAR_CHUNK, 0, view_radius);
    let strays: Vec<(i32, i32)> = recorded
        .iter()
        .copied()
        .filter(|pos| !window.contains(pos))
        .collect();
    assert!(
        strays.is_empty(),
        "generation is not anchored at the player: {} of {} generated columns lie outside \
         the player's window, e.g. {:?}",
        strays.len(),
        recorded.len(),
        &strays[..strays.len().min(8)]
    );

    // Half two, the magnitude: the expected count comes from the view radius,
    // not from a measurement. An origin-anchored enumeration would be ~5,000x
    // this; a rectangle-fill between origin and player, ~10,000x.
    let expected = ((2 * view_radius + 1) * (2 * view_radius + 1)) as usize;
    assert_eq!(
        recorded.len(),
        expected,
        "a recenter 80,000 blocks out generated {} columns; a player-anchored window \
         is exactly {expected}",
        recorded.len()
    );

    drop(client);
    let _ = server.await.expect("server task panicked");
}
/// A [`ChunkSource`] of solid `minecraft:stone` — the block-drop tests' subject
/// world, chosen because `assets/loot_table/blocks/stone.json` is one of the five
/// bundled block tables and its alternative-entry shape is the non-trivial one
/// (a bare hand must fall through the tool-specific branch to cobblestone, so a
/// fixture of stone proves the fall-through actually happens
/// rather than that "an item dropped").
struct StoneSource;

impl ChunkSource for StoneSource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        let mut col = ChunkColumn::new(0, 16);
        for x in 0..16 {
            for z in 0..16 {
                for y in 0..16 {
                    col.set_block(x, y, z, "minecraft:stone");
                }
            }
        }
        col
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).block_state(lx, y, lz).to_string()
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).biome_state_at(lx, y, lz).to_string()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
        // No storage; edits are discarded by design. Harmless here: the drop is
        // rolled from the state read *before* `set_block`, which is exactly the
        // ordering `apply_block_action` guarantees.
    }
}

/// The block every block-drop test below breaks. Inside the served view, and
/// inside `StoneSource`'s 0..16 stone band.
const BREAK_POS: BlockPos = BlockPos::new(4, 9, 4);

/// How long a bare-handed dig is held for, between `StartDestroy` and
/// `StopDestroy`, in tests that break without a tool.
///
/// Comfortably over the slowest dig any of them performs: bare-handed stone is
/// `1.0 / 1.5 / 100` progress per server tick, so with
/// `block_breaking::UNTRACKED_SPEED_HEADROOM` it needs 14 ticks (700 ms) to reach
/// `STOP_DESTROY_PROGRESS`. Kept under `TIME_SYNC_INTERVAL`'s 1 s so the wait does
/// not also inject a time broadcast into the drained stream.
const BARE_HANDED_DIG: std::time::Duration = std::time::Duration::from_millis(800);

/// Puts a plain diamond pickaxe in the selected hand, breaks `pos`, and then
/// **empties the hand again**.
///
/// The pickaxe is required because stone requires the correct tool for drops,
/// so a bare hand produces no drop (asserted by
/// `bare_handed_stone_drops_nothing_while_bare_handed_dirt_still_drops`). It is
/// deliberately *unenchanted*, so the expected drop is `cobblestone`.
///
/// Emptying the hand afterwards is not tidiness: inventory insertion searches
/// selected → off-hand → `0..36` and scans the item slots in order, so a
/// pickaxe left in native slot 0 would send the collected cobblestone to native
/// slot 1 (menu slot 10) and change *which slot the pickup announces*. Clearing
/// it keeps the pickup gates asserting menu slot 36, the property they exist to
/// pin.
async fn break_with_a_pickaxe(client: &mut Connection<DuplexStream>, ordinal: u8, pos: BlockPos) {
    send_creative_slot(
        client,
        36,
        Some(&ItemStack::new(
            "minecraft:diamond_pickaxe".parse().expect("valid key"),
            1,
        )),
    )
    .await;
    send_block_action(client, 0, pos).await;
    send_block_action(client, ordinal, pos).await;
    send_creative_slot(client, 36, None).await;
}

/// **The first block-drop gate: a broken block drops.**
///
/// `apply_block_action`'s `StopDestroy` arm must set the block to air and emit
/// exactly one drop. This must drive the *real*
/// `serve_connection` path (not `drop_block_loot` directly, which would pass
/// whether or not the server ever called it) and asserts the exact drop.
///
/// Three separate predictions, each of which a plausible-but-wrong
/// implementation fails:
///
/// 1. **exactly one** item entity — not "at least one", which a table rolling
///    its pool twice would also satisfy;
/// 2. the item is **`minecraft:cobblestone`**, which is stone's table falling
///    through its silk-touch `alternatives` branch under the empty loot context.
///    A port that took the *first* alternative would produce `minecraft:stone`
///    here, and "a stone-ish item dropped" reads as success;
/// 3. the entity streams with entity type **`minecraft:item`**. The item key is
///    metadata, not the entity type; checking both fields prevents a correctly
///    positioned but unrenderable entity from passing this gate.
#[tokio::test(start_paused = true)]
async fn breaking_stone_drops_exactly_one_cobblestone_item_entity() {
    let (client_end, server_end) = memory_pair();
    let mobs = MobHandle::default();
    let mobs_for_server = mobs.clone();
    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(
            &mut conn,
            &FakeProtocol,
            &StoneSource,
            &NoEntities,
            0,
            &BlockEntityHandle::default(),
            &mobs_for_server,
        )
        .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Driller", 1).await;
    assert_eq!(
        mobs.with(|sim| sim.item_count()),
        0,
        "precondition: nothing has dropped anything yet"
    );
    break_with_a_pickaxe(&mut client, 2, BREAK_POS).await;
    let _ = drain_available(&mut client).await;

    let snapshots = mobs.with(|sim| sim.snapshots());
    assert_eq!(
        mobs.with(|sim| sim.item_count()),
        1,
        "one break of stone rolls one pool once, so exactly one item entity"
    );
    assert_eq!(
        snapshots.len(),
        1,
        "the drop must be the only entity in the sim, so the assertions below \
         cannot be reading some other entity: {snapshots:?}"
    );
    assert_eq!(
        snapshots[0].entity_type.to_string(),
        "minecraft:item",
        "a dropped item is entity type `minecraft:item`; the item's own key here \
         means `entity_type_id` misses and the client draws `minecraft:acacia_boat`"
    );
    // The *stack* travels as metadata, which is what decides
    // whether the drop draws at all. `entity_type` alone gets a correctly
    // positioned, correctly typed, completely **invisible** item entity onto
    // the client — an item entity with the wrong type cannot be rendered.
    // The exact metadata list is asserted
    // as the exact field list, not `!is_empty()`: a `MetadataField::Item`
    // carrying the wrong key (`minecraft:stone`, had the silk-touch branch
    // won) or the wrong count would satisfy a non-emptiness check.
    assert_eq!(
        snapshots[0].metadata,
        vec![MetadataField::Item {
            item: "minecraft:cobblestone".parse().expect("valid key"),
            count: 1,
        }],
        "a dropped item's whole visible identity is ItemEntity.DATA_ITEM"
    );

    drop(client);
    let _ = server.await.expect("server task panicked");
}

/// **The wire assertion every other drop gate in this file omits.**
///
/// Every other drop/pickup test passes `entities: &NoEntities`, so those tests
/// verify loot, inventory, and simulation state without proving that a connected
/// client receives the spawned entity. This gate supplies the same `MobHandle`
/// as both `entities` and `mobs`, allowing the next streaming pass to expose the
/// item on the wire.
///
/// This gate passes the **same** `MobHandle` as both `entities` and `mobs`, as
/// [`lodestone_server::IntegratedServer::open_in_memory_with_items`] does
/// for the browser build. `MobHandle` is a legitimate `EntitySource`
/// on its own (see that impl's own doc comment) for a caller that mutates the
/// sim directly and needs no ticked republish — which is exactly
/// `destroy_block`'s access pattern, no tick loop involved.
#[tokio::test(start_paused = true)]
async fn breaking_stone_streams_add_entity_when_the_mob_handle_is_its_own_source() {
    let (client_end, server_end) = memory_pair();
    let mobs = MobHandle::default();
    let mobs_for_server = mobs.clone();
    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(
            &mut conn,
            &FakeProtocol,
            &StoneSource,
            // The entity source is the same handle as `mobs` below, not
            // `&NoEntities`.
            &mobs_for_server,
            0,
            &BlockEntityHandle::default(),
            &mobs_for_server,
        )
        .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Streamer", 1).await;
    break_with_a_pickaxe(&mut client, 2, BREAK_POS).await;
    let packets = drain_available(&mut client).await;

    assert_eq!(
        mobs.with(|sim| sim.item_count()),
        1,
        "precondition: the break must actually roll a drop"
    );
    assert!(
        packets.iter().any(|(id, _)| *id == ADD_ENTITY_S2C),
        "the dropped item's MobHandle doubles as its own EntitySource, so an \
         ADD_ENTITY must reach the client on the very next streaming pass; got \
         packet ids {:?}",
        packets.iter().map(|(id, _)| *id).collect::<Vec<_>>()
    );

    drop(client);
    let _ = server.await.expect("server task panicked");
}

/// A [`ChunkSource`] like [`StoneSource`] but with **one persisted cell**: every
/// other block is stone (discarded on write, exactly as [`StoneSource`]), while
/// [`BREAK_POS`] alone remembers whatever it was last set to. This
/// fixture needs a real write to survive the break so the test can read back
/// *what* replaced the broken block, not merely that something did.
#[derive(Clone)]
struct SingleBlockSource {
    at_break_pos: std::sync::Arc<std::sync::Mutex<String>>,
}

impl SingleBlockSource {
    fn new(initial: &str) -> Self {
        Self {
            at_break_pos: std::sync::Arc::new(std::sync::Mutex::new(initial.to_string())),
        }
    }

    fn current(&self) -> String {
        self.at_break_pos.lock().expect("lock").clone()
    }
}

impl ChunkSource for SingleBlockSource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        let mut col = ChunkColumn::new(0, 16);
        for x in 0..16 {
            for z in 0..16 {
                for y in 0..16 {
                    col.set_block(x, y, z, "minecraft:stone");
                }
            }
        }
        col.set_block(BREAK_POS.x, BREAK_POS.y, BREAK_POS.z, &self.current());
        col
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        if (x, y, z) == (BREAK_POS.x, BREAK_POS.y, BREAK_POS.z) {
            return self.current();
        }
        "minecraft:stone".to_string()
    }

    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        "minecraft:plains".to_string()
    }

    fn set_block(&self, x: i32, y: i32, z: i32, name: &str) {
        if (x, y, z) == (BREAK_POS.x, BREAK_POS.y, BREAK_POS.z) {
            *self.at_break_pos.lock().expect("lock") = name.to_string();
        }
        // Everything else discarded, matching `StoneSource`.
    }
}

/// **The discriminating pair, through the real `serve_connection`
/// path**: breaking a waterlogged block leaves its water source behind, and
/// breaking the identical block *without* `waterlogged` leaves air.
///
/// Either arm alone passes under a wrong rule — the dry arm passes under
/// "always keep the fluid" too (a dry block's fluid state is `None`, so both
/// rules answer air), and the wet arm alone would pass under an implementation
/// that always wrote a water source. Only running both against the same break sequence
/// separates the fluid-preservation rule (retain the block's fluid state) from
/// "write air unconditionally".
///
/// `oak_slab` rather than a plain waterlogged block because a slab drops loot
/// (exercising the same `destroy_block` path the cobblestone gate above does)
/// and is a real vanilla `waterlogged`-carrying block, not a synthetic one.
#[tokio::test(start_paused = true)]
async fn breaking_a_waterlogged_block_leaves_water_and_a_dry_one_leaves_air() {
    async fn break_and_read(initial_state: &str) -> String {
        let source = SingleBlockSource::new(initial_state);
        let (client_end, server_end) = memory_pair();
        let mobs = MobHandle::default();
        let mobs_for_server = mobs.clone();
        let source_for_server = source.clone();
        let server = tokio::spawn(async move {
            let mut conn = Connection::new(server_end);
            serve_connection(
                &mut conn,
                &FakeProtocol,
                &source_for_server,
                &NoEntities,
                0,
                &BlockEntityHandle::default(),
                &mobs_for_server,
            )
            .await
        });

        let mut client = Connection::new(client_end);
        drive_login_and_join(&mut client, "WaterlogBreaker", 1).await;
        // Creative rather than `break_with_a_pickaxe`: a pickaxe is the
        // *wrong* tool for a wood slab (an axe is correct), so a survival dig
        // needs `divider = 100` and ~140 ticks to clear `STOP_DESTROY_PROGRESS`
        // — far more than `drain_available`'s single 50ms idle window lets
        // through. Creative's `StartDestroy` breaks synchronously
        // (`apply_block_action`'s `creative` branch), and this test is about
        // *what replaces the cell*, not about drops or dig timing, so
        // creative exercises the exact same `destroy_block` write path with
        // none of that noise.
        send_game_mode(&mut client, 1).await;
        send_block_action(&mut client, 0, BREAK_POS).await;
        let _ = drain_available(&mut client).await;

        drop(client);
        let _ = server.await.expect("server task panicked");
        source.current()
    }

    let dry = break_and_read("minecraft:oak_slab[type=bottom,waterlogged=false]").await;
    assert_eq!(dry, "minecraft:air", "a dry block's cell must become air");

    let wet = break_and_read("minecraft:oak_slab[type=bottom,waterlogged=true]").await;
    assert_eq!(
        wet, "minecraft:water[level=0]",
        "a waterlogged block's cell must keep its water source, not go to air \
         — `level=0` is `FlowingFluid.getLegacyLevel`'s own encoding for a \
         source, matching `fluidState.createLegacyBlock()`"
    );
}

/// Stone everywhere except one column of dirt at [`DIRT_POS`], for the
/// correct-tool gate.
///
/// The *world*-species guard for that gate: a correct-tool requirement applies
/// to stone but not dirt, and a fixture containing both exercises both sides of
/// that distinction.
/// Every block in [`StoneSource`] requires a correct tool, so a gate
/// mis-implemented as "you need a tool" would pass every stone assertion and
/// fail only here.
struct StoneWithDirtSource;

/// The dirt column in [`StoneWithDirtSource`]: same chunk and height band as
/// [`BREAK_POS`], different x.
const DIRT_POS: BlockPos = BlockPos::new(6, 9, 4);

impl ChunkSource for StoneWithDirtSource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        let mut col = ChunkColumn::new(0, 16);
        for x in 0..16 {
            for z in 0..16 {
                for y in 0..16 {
                    col.set_block(x, y, z, "minecraft:stone");
                }
            }
        }
        col.set_block(DIRT_POS.x, DIRT_POS.y, DIRT_POS.z, "minecraft:dirt");
        col
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).block_state(lx, y, lz).to_string()
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).biome_state_at(lx, y, lz).to_string()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
        // As `StoneSource`: no storage, and the drop is rolled from the state
        // read before `set_block`.
    }
}

/// **The correct-tool gate, through the real `serve_connection` path:
/// stone broken bare-handed drops nothing, and dirt still does.**
///
/// The server checks whether the held tool is valid before rolling drops. Stone
/// requires a correct tool, so a bare hand breaks it and yields nothing — an
/// incorrect implementation could drop cobblestone.
///
/// The stone half is an **absence**, so it is only worth what the evidence that
/// the detector fires is worth. Three things supply that here rather than a
/// description of it:
///
/// 1. the *same* packets with a pickaxe in slot 36 do produce a drop — that is
///    `breaking_stone_drops_exactly_one_cobblestone_item_entity` above, which
///    provides the corresponding positive drop case;
/// 2. **dirt, bare-handed, in the same session, still drops dirt.** A gate that
///    swallowed the whole `StopDestroy` arm, or that read "you need a tool",
///    would report "no drop" for stone too and fail here;
/// 3. the two assertions run against one connection in one order, so neither can
///    be explained by the session never having reached the break path.
#[tokio::test(start_paused = true)]
async fn bare_handed_stone_drops_nothing_while_bare_handed_dirt_still_drops() {
    let (client_end, server_end) = memory_pair();
    let mobs = MobHandle::default();
    let mobs_for_server = mobs.clone();
    let source = StoneWithDirtSource;
    assert_eq!(
        source.block_state(BREAK_POS.x, BREAK_POS.y, BREAK_POS.z),
        "minecraft:stone",
        "precondition: BREAK_POS is stone, which requires a correct tool"
    );
    assert_eq!(
        source.block_state(DIRT_POS.x, DIRT_POS.y, DIRT_POS.z),
        "minecraft:dirt",
        "precondition: DIRT_POS is dirt, which requires none — without this row \
         the fixture cannot exercise the other side of vanilla's `||`"
    );
    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(
            &mut conn,
            &FakeProtocol,
            &StoneWithDirtSource,
            &NoEntities,
            0,
            &BlockEntityHandle::default(),
            &mobs_for_server,
        )
        .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "BareHands", 1).await;

    // No creative-slot write at all: the selected slot is empty, which is the
    // bare hand.
    //
    // A held dig between the two ordinals, and only in this test, because it is
    // the only one that breaks **bare-handed**: the server
    // prices the dig (`crate::block_breaking`) and refuses a `StopDestroy` that
    // arrives too early, and a bare hand on stone is the slowest dig in the
    // file. Without the advance both packets land on one server tick, the break
    // is *refused*, and this test's first assertion — an absence — would pass
    // for the wrong reason while the dirt half failed. `break_with_a_pickaxe`
    // needs none of this: a diamond pickaxe on stone clears the threshold in a
    // single tick, which is why every other break gate here still holds.
    //
    // `sleep`, **not** `tokio::time::advance`: `advance` jumps the clock before
    // yielding, so the server may not have read the `StartDestroy` and stamped it
    // with the same tick — both packets then land together and the break is
    // refused. A paused-clock `sleep` lets the runtime drain the
    // start packet first and only auto-advances once everything is idle.
    send_block_action(&mut client, 0, BREAK_POS).await;
    tokio::time::sleep(BARE_HANDED_DIG).await;
    send_block_action(&mut client, 2, BREAK_POS).await;
    let _ = drain_available(&mut client).await;
    assert_eq!(
        mobs.with(|sim| sim.item_count()),
        0,
        "a bare hand on stone is `hasCorrectToolForDrops == false`, so vanilla \
         never rolls the table; before #539 this dropped a cobblestone"
    );

    send_block_action(&mut client, 0, DIRT_POS).await;
    tokio::time::sleep(BARE_HANDED_DIG).await;
    send_block_action(&mut client, 2, DIRT_POS).await;
    let _ = drain_available(&mut client).await;
    let snapshots = mobs.with(|sim| sim.snapshots());
    assert_eq!(
        snapshots.len(),
        1,
        "dirt does not require a correct tool, so the same bare hand drops it: \
         {snapshots:?}"
    );
    assert_eq!(
        snapshots[0].metadata,
        vec![MetadataField::Item {
            item: "minecraft:dirt".parse().expect("valid key"),
            count: 1,
        }],
    );

    drop(client);
    let _ = server.await.expect("server task panicked");
}

/// Stone everywhere except one dandelion at [`FLOWER_POS`], for the one-shot
/// gate below.
struct StoneWithFlowerSource;

/// The dandelion in [`StoneWithFlowerSource`] — a zero-hardness block, so
/// `progress_per_tick` is `+inf`, so `StartDestroy` completes immediately.
const FLOWER_POS: BlockPos = BlockPos::new(8, 9, 4);

impl ChunkSource for StoneWithFlowerSource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        let mut col = ChunkColumn::new(0, 16);
        for x in 0..16 {
            for z in 0..16 {
                for y in 0..16 {
                    col.set_block(x, y, z, "minecraft:stone");
                }
            }
        }
        col.set_block(FLOWER_POS.x, FLOWER_POS.y, FLOWER_POS.z, "minecraft:dandelion");
        col
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).block_state(lx, y, lz).to_string()
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).biome_state_at(lx, y, lz).to_string()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
        // As `StoneSource`: no storage, and the drop is rolled from the state
        // read before `set_block`.
    }
}

/// **The one-shot-block gate, end to end: a `StartDestroy` with no `StopDestroy`
/// after it pops a flower.**
///
/// This requires a dedicated integration assertion; a unit assertion on
/// `progress_per_tick >= 1.0` is a closed loop that says nothing
/// about whether `apply_block_action` reaches `destroy_block` on the start
/// ordinal. A real client that knows a block is instant sends *only* the start
/// action, so this is the whole packet sequence for pulling grass.
///
/// The control is the pair below it: the same single start action on **stone**
/// drops nothing, so this cannot pass by breaking on every `StartDestroy`.
#[tokio::test(start_paused = true)]
async fn a_start_action_alone_pops_a_one_shot_flower_but_not_stone() {
    let (client_end, server_end) = memory_pair();
    let mobs = MobHandle::default();
    let mobs_for_server = mobs.clone();
    let source = StoneWithFlowerSource;
    assert_eq!(
        source.block_state(FLOWER_POS.x, FLOWER_POS.y, FLOWER_POS.z),
        "minecraft:dandelion",
        "precondition: FLOWER_POS is the zero-hardness block under test"
    );
    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(
            &mut conn,
            &FakeProtocol,
            &StoneWithFlowerSource,
            &NoEntities,
            0,
            &BlockEntityHandle::default(),
            &mobs_for_server,
        )
        .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Picker", 1).await;

    // The control first, so a failure cannot be blamed on session ordering: one
    // start action on stone, bare-handed, and *nothing* after it.
    send_block_action(&mut client, 0, BREAK_POS).await;
    let _ = drain_available(&mut client).await;
    assert_eq!(
        mobs.with(|sim| sim.item_count()),
        0,
        "a bare-handed start action on stone must not break it — if this is \
         non-zero the flower assertion below proves nothing"
    );

    send_block_action(&mut client, 0, FLOWER_POS).await;
    let _ = drain_available(&mut client).await;
    let snapshots = mobs.with(|sim| sim.snapshots());
    assert_eq!(
        snapshots.len(),
        1,
        "a start action alone must pop a zero-hardness block: {snapshots:?}"
    );
    assert_eq!(
        snapshots[0].metadata,
        vec![MetadataField::Item {
            item: "minecraft:dandelion".parse().expect("valid key"),
            count: 1,
        }],
    );

    drop(client);
    let _ = server.await.expect("server task panicked");
}

/// **The deferred-progress gate for ordinary block breaking: a `StopDestroy` that
/// arrives on the same tick as its `StartDestroy` still breaks the block, a
/// tick or two later.**
///
/// A shortfall must not be refused outright: the deferred path keeps accruing
/// progress until the block is fully earned.
/// A local integrated server reads both
/// packets off one buffer, so *every* non-instant block took the shortfall path
/// and nothing but flowers could be broken at all.
///
/// Dirt bare-handed, because it is the one block in this file that both takes a
/// real dig and drops without a tool: `1.0 / 0.5 / 100` per tick, so with
/// `UNTRACKED_SPEED_HEADROOM` the *deferred* target of a whole block needs 7
/// ticks. The wait below is deliberately longer than that and shorter than the
/// slower blocks around it.
///
/// Two controls, and the first is the one that matters: **the drop is absent
/// immediately after the pair is sent** and present only after the wait, so this
/// is a gate on the deferred continuation rather than on the pair being accepted
/// outright. The second replaces the stop with an abort, which must never break
/// however long you wait.
#[tokio::test(start_paused = true)]
async fn a_same_tick_stop_breaks_the_block_a_few_ticks_later() {
    let (client_end, server_end) = memory_pair();
    let mobs = MobHandle::default();
    let mobs_for_server = mobs.clone();
    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(
            &mut conn,
            &FakeProtocol,
            &StoneWithDirtSource,
            &NoEntities,
            0,
            &BlockEntityHandle::default(),
            &mobs_for_server,
        )
        .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Releaser", 1).await;

    // Control: start then abort, back to back, then the same wait. An abort
    // clears the dig, so no deferred continuation may exist to finish it.
    send_block_action(&mut client, 0, DIRT_POS).await;
    send_block_action(&mut client, 1, DIRT_POS).await;
    tokio::time::sleep(BARE_HANDED_DIG).await;
    let _ = drain_available(&mut client).await;
    assert_eq!(
        mobs.with(|sim| sim.item_count()),
        0,
        "an aborted dig must not be finished by the deferred-destroy tick pass"
    );

    // The subject: start then stop, back to back, with no wait between them —
    // both land on one server tick, which exercises the deferred-progress path.
    send_block_action(&mut client, 0, DIRT_POS).await;
    send_block_action(&mut client, 2, DIRT_POS).await;
    let _ = drain_available(&mut client).await;
    assert_eq!(
        mobs.with(|sim| sim.item_count()),
        0,
        "a same-tick stop has not earned the block yet — if it drops here the \
         wait below is not what this gate is measuring"
    );

    // `sleep`, **not** `tokio::time::advance`: see
    // `bare_handed_stone_drops_nothing_while_bare_handed_dirt_still_drops`'s own
    // note. Only a paused-clock `sleep` lets the server's 50ms timer arm run.
    tokio::time::sleep(BARE_HANDED_DIG).await;
    let _ = drain_available(&mut client).await;
    let snapshots = mobs.with(|sim| sim.snapshots());
    assert_eq!(
        snapshots.len(),
        1,
        "the deferred dig must finish on the server's own clock: {snapshots:?}"
    );
    assert_eq!(
        snapshots[0].metadata,
        vec![MetadataField::Item {
            item: "minecraft:dirt".parse().expect("valid key"),
            count: 1,
        }],
    );

    drop(client);
    let _ = server.await.expect("server task panicked");
}

/// **The control for the gate above, and it must fail the same assertion.**
///
/// Same world, same block, same packets — except the second phase is
/// `AbortDestroy` (ordinal `1`) instead of `StopDestroy`. Position bookkeeping
/// means an aborted dig breaks nothing,
/// so nothing may drop.
///
/// Without this, `breaking_stone_drops_exactly_one_cobblestone_item_entity` would
/// pass just as well against a server that dropped an item on *every* block
/// packet, including the `StartDestroy` that precedes every break. The two tests
/// differ in exactly one byte.
#[tokio::test(start_paused = true)]
async fn an_aborted_dig_drops_nothing() {
    let (client_end, server_end) = memory_pair();
    let mobs = MobHandle::default();
    let mobs_for_server = mobs.clone();
    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(
            &mut conn,
            &FakeProtocol,
            &StoneSource,
            &NoEntities,
            0,
            &BlockEntityHandle::default(),
            &mobs_for_server,
        )
        .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Aborter", 1).await;
    // A *correct* tool, so the only difference from the positive gate is the
    // second ordinal — the correct-tool gate cannot be what makes this pass.
    break_with_a_pickaxe(&mut client, 1, BREAK_POS).await;
    let _ = drain_available(&mut client).await;

    assert_eq!(
        mobs.with(|sim| sim.item_count()),
        0,
        "an aborted dig breaks no block, so it must drop nothing — if this is \
         non-zero the drop is firing on the wrong packet and the positive gate \
         proves nothing"
    );

    drop(client);
    let _ = server.await.expect("server task panicked");
}

/// Creative mode's authoritative positive path: a hard block is removed by
/// the start action alone, with no timing wait, follow-up stop, or item drop.
/// The source is mutable so this observes the real `set_block` write rather
/// than merely counting a response packet.
#[tokio::test(start_paused = true)]
async fn creative_start_breaks_a_hard_block_without_a_stop_or_drop() {
    let source = SingleBlockSource::new("minecraft:stone");
    let (client_end, server_end) = memory_pair();
    let mobs = MobHandle::default();
    let mobs_for_server = mobs.clone();
    let source_for_server = source.clone();
    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(
            &mut conn,
            &FakeProtocol,
            &source_for_server,
            &NoEntities,
            0,
            &BlockEntityHandle::default(),
            &mobs_for_server,
        )
        .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "CreativeBreaker", 1).await;
    send_game_mode(&mut client, 1).await;
    send_block_action(&mut client, 0, BREAK_POS).await;
    let _ = drain_available(&mut client).await;

    assert_eq!(source.current(), "minecraft:air");
    assert_eq!(
        mobs.with(|sim| sim.item_count()),
        0,
        "creative destruction must not roll a block drop"
    );

    drop(client);
    let _ = server.await.expect("server task panicked");
}

/// Protected-state negative control: creative bypasses the hardness clock,
/// not the block's unbreakable protection. Bedrock must survive a start-only
/// creative action, proving this detector fires against the positive test's
/// same authoritative path.
#[tokio::test(start_paused = true)]
async fn creative_start_does_not_break_an_unbreakable_block() {
    let source = SingleBlockSource::new("minecraft:bedrock");
    let (client_end, server_end) = memory_pair();
    let mobs = MobHandle::default();
    let mobs_for_server = mobs.clone();
    let source_for_server = source.clone();
    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(
            &mut conn,
            &FakeProtocol,
            &source_for_server,
            &NoEntities,
            0,
            &BlockEntityHandle::default(),
            &mobs_for_server,
        )
        .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "CreativeBedrock", 1).await;
    send_game_mode(&mut client, 1).await;
    send_block_action(&mut client, 0, BREAK_POS).await;
    let _ = drain_available(&mut client).await;

    assert_eq!(source.current(), "minecraft:bedrock");
    assert_eq!(mobs.with(|sim| sim.item_count()), 0);

    drop(client);
    let _ = server.await.expect("server task panicked");
}

/// Invalid-state negative control: an unknown registry state is not an
/// eligible creative target. This is intentionally a distinct source fixture
/// from the protected block, so a hard-coded bedrock check cannot satisfy it.
#[tokio::test(start_paused = true)]
async fn creative_start_does_not_break_an_unknown_block_state() {
    let source = SingleBlockSource::new("minecraft:not_a_real_block");
    let (client_end, server_end) = memory_pair();
    let mobs = MobHandle::default();
    let mobs_for_server = mobs.clone();
    let source_for_server = source.clone();
    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(
            &mut conn,
            &FakeProtocol,
            &source_for_server,
            &NoEntities,
            0,
            &BlockEntityHandle::default(),
            &mobs_for_server,
        )
        .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "CreativeUnknown", 1).await;
    send_game_mode(&mut client, 1).await;
    send_block_action(&mut client, 0, BREAK_POS).await;
    let _ = drain_available(&mut client).await;

    assert_eq!(source.current(), "minecraft:not_a_real_block");
    assert_eq!(mobs.with(|sim| sim.item_count()), 0);

    drop(client);
    let _ = server.await.expect("server task panicked");
}

/// Reach negative control: even creative mode cannot break a target outside
/// the interaction range. The movement packet establishes a real player
/// position before the action; without that packet the server deliberately
/// has no range evidence and cannot safely reject the request.
#[tokio::test(start_paused = true)]
async fn creative_start_does_not_break_a_target_out_of_reach() {
    let source = SingleBlockSource::new("minecraft:stone");
    let (client_end, server_end) = memory_pair();
    let mobs = MobHandle::default();
    let mobs_for_server = mobs.clone();
    let source_for_server = source.clone();
    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(
            &mut conn,
            &FakeProtocol,
            &source_for_server,
            &NoEntities,
            0,
            &BlockEntityHandle::default(),
            &mobs_for_server,
        )
        .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "CreativeFarAway", 1).await;
    send_game_mode(&mut client, 1).await;
    send_player_moved(&mut client, 100.0, 100.0, 100.0).await;
    send_block_action(&mut client, 0, BREAK_POS).await;
    let _ = drain_available(&mut client).await;

    assert_eq!(source.current(), "minecraft:stone");
    assert_eq!(mobs.with(|sim| sim.item_count()), 0);

    drop(client);
    let _ = server.await.expect("server task panicked");
}
