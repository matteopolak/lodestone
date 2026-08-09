use std::collections::HashSet;

use super::*;
use crate::config::{Config, Mode};
use lodestone_ecs::player::SWIMMING_EYE_HEIGHT;

fn test_config() -> Config {
    Config {
        mode: Mode::Headless,
        render_distance: 2,
        ..Config::default()
    }
}

/// Fold one `ClientEvent` into this `Sim`'s `World` exactly the way the net
/// thread's `lodestone_client::state::SharedState::apply` does — enqueue,
/// run `NetIngest` once, one event per run.
///
/// # Why the loopback feed is not enough for these
///
/// `NetClient::loopback_with_feed` models the `NetUpdate` channel — the
/// *driver's* reaction path. It does not model `SharedState::apply`, which is
/// where the local player's server-reported state (vitals, xp, the entity id,
/// game mode, dimension, liveness) is folded, and there is no `SharedState` in
/// a loopback harness at all. Production runs **both** paths for one packet,
/// so a test that needs both drives both — which is closer to production than
/// the `NetUpdate::Health` these tests used to feed, because that arm was the
/// duplicate fold the collapse deleted.
fn ingest(sim: &mut Sim, event: lodestone_client::ClientEvent) {
    sim.write(|w| {
        w.resource_mut::<lodestone_ecs::ingest::IngestQueue>()
            .push(event);
        w.run_schedule(lodestone_ecs::NetIngest);
    });
}

/// A `ClientEvent::Login` for `entity_id`, creative in the overworld — the
/// event that seeds `ServerEntityId` **and** the local player's `EntityIndex`
/// entry.
fn login_event(entity_id: i32) -> lodestone_client::ClientEvent {
    lodestone_client::ClientEvent::Login {
        entity_id,
        game_mode: lodestone_client::GameMode::Creative,
        dimension: "minecraft:overworld".parse().expect("valid dimension id"),
    }
}

/// The objective name currently displayed in the sidebar slot, read straight
/// off the [`lodestone_ecs::SessionScoreboard`] component rather than through
/// `Sim::sidebar` — which also needs the objective's own `ObjectiveUpdate` and
/// a translator, neither of which this is asking about.
fn displayed_sidebar(sim: &Sim) -> Option<String> {
    sim.read(|w| {
        w.get::<lodestone_ecs::SessionScoreboard>(sim.local)?
            .0
            .displayed(lodestone_game::scoreboard::DisplaySlot::Sidebar)
            .map(str::to_owned)
    })
}

/// What a real windowed client is built from — the path that must never hold
/// an offline world. `Mode::Window` matters: `Mode::Headless` deliberately
/// delegates to the demo-world fixture (see [`Sim::new`]).
fn client_config() -> Config {
    Config {
        mode: Mode::Window,
        render_distance: 2,
        ..Config::default()
    }
}

/// Sections the GPU is holding, counted the way `app::WindowApp::redraw`
/// drives it: upload everything that has meshed, then apply the removals.
/// `TerrainMesh::uploaded_sections` is the record of exactly that set.
fn resident_sections(sim: &mut Sim) -> usize {
    let _ = sim.drain_all_meshes();
    let _ = sim.drain_removals();
    sim.terrain(|t| t.uploaded_sections.len())
}

/// Drive one loopback session to `Connected` and report what is resident.
/// The feed sends **no chunks**, so the live world's section set is empty and
/// any non-zero count is offline terrain.
fn resident_after_connect(mut sim: Sim) -> usize {
    use crate::net::NetUpdate;
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
    sim.poll_net();
    assert_eq!(sim.session_phase(), SessionPhase::Connected);
    sim.step(5.0 / 20.0);
    resident_sections(&mut sim)
}

#[test]
fn a_client_session_holds_only_the_live_world_never_offline_terrain() {
    // The two-worlds regression: the client came up with `worldgen`'s demo
    // world meshed and uploaded around the origin, then a multiplayer join
    // added the server's columns *alongside* it — the player standing at the
    // server's spawn with the wrong world drawn several hundred blocks away.
    //
    // The assertion is on the counters the report was diagnosed from: total
    // resident sections must equal the live set, not the sum. It comes first
    // in this test so that the control below — the pre-fix construction —
    // fails on *this* check rather than on a structural one.
    assert_eq!(
        resident_after_connect(Sim::new(client_config())),
        0,
        "after attaching a live session the resident set must be exactly the \
         live world's sections (none here — the loopback feed sends no chunks); \
         anything else is the offline world left behind"
    );

    // Same property, one layer earlier: nothing to tear down beats tearing
    // it down, so the offline world must never be built or scheduled at all.
    let mut sim = Sim::new(client_config());
    assert!(
        sim.chunk_world().is_empty(),
        "a client session must not generate an offline world"
    );
    assert_eq!(
        sim.pending_meshes(),
        0,
        "a client session must not schedule offline sections for meshing"
    );
    assert_eq!(
        resident_sections(&mut sim),
        0,
        "nothing may be uploaded before a session exists"
    );
}

#[test]
fn the_demo_world_fixture_is_the_control_that_fails_the_gate_above() {
    // The detector's positive control. `Sim::with_demo_world` *is* what
    // `Sim::new` used to do for every windowed run without `--live`, so this
    // reproduces the reported state exactly: offline sections meshed,
    // uploaded, and still resident after a live session attaches. If this ever
    // reports zero, the gate above has stopped being able to fail and is
    // vacuous — it is not measuring residency any more.
    let mut fixture = Sim::with_demo_world(test_config());
    assert!(
        !fixture.chunk_world().is_empty(),
        "the fixture must build a world"
    );
    assert!(
        resident_sections(&mut fixture) > 0,
        "control: the fixture must actually upload offline sections"
    );
    assert!(
        resident_after_connect(Sim::with_demo_world(test_config())) > 0,
        "control: offline sections must still be resident after a live \
         session attaches — this is the assertion the client path must not \
         be able to satisfy"
    );
}

#[test]
fn fog_reaches_full_at_the_configured_render_distance() {
    // Fog is what hides the render-distance edge, so its end must track the
    // *configured* distance. A fixed default would fog out the outer chunks
    // of a larger view, making `--render-distance 16` look worse than 8.
    for rd in [2u32, 8, 16, 32] {
        let fog = fog_for_render_distance(rd);
        assert_eq!(
            fog.end,
            rd as f32 * 16.0,
            "fog should reach full at the render distance for rd={rd}"
        );
        assert!(
            fog.start < fog.end,
            "fog range must be non-degenerate, else fog silently disables"
        );
    }
}

#[test]
fn fog_stays_well_inside_the_camera_far_plane() {
    // If fog completed at or beyond the far plane, geometry would clip
    // against a still-visible background instead of dissolving into it.
    for rd in [2u32, 8, 16, 32] {
        let far = lodestone_render::Camera::far_for_render_distance(rd, 0);
        assert!(
            fog_for_render_distance(rd).end < far,
            "fog end must precede the far plane for rd={rd}"
        );
    }
}

#[test]
fn fog_fades_into_the_same_colour_the_frame_clears_to() {
    // Terrain fades into the sky. If these two drifted apart, the horizon
    // would show a band of haze in a colour the sky never is.
    assert_eq!(fog_for_render_distance(8).color, crate::gpu::SKY_COLOR);
}

#[test]
fn sim_fog_follows_its_own_config_not_a_default() {
    // Proves the delegation, so the cheap tests above actually cover what
    // the renderer is handed.
    let sim = Sim::new(test_config());
    assert_eq!(
        sim.fog_settings(),
        fog_for_render_distance(sim.config.render_distance)
    );
    assert_ne!(
        sim.fog_settings(),
        fog_for_render_distance(8),
        "test config is not the default distance, so these must differ"
    );
}

#[test]
fn a_submerged_eye_selects_short_dense_fog_over_the_sky_fog() {
    // The whole point of threading the fluid state through: while the eye is
    // under water the fog must become the short, dense water fog, not the
    // render-distance sky fog that would leave the seabed sharp to the
    // horizon (the pre-change bug, confirmed on pixels). Guards the
    // *selection*; the colour/vanilla-likeness is a pixel concern.
    let mut sim = Sim::new(test_config());
    let rd = sim.config.render_distance;
    let sky = fog_for_render_distance(rd);

    // Dry: the render-distance sky fog.
    assert_eq!(sim.fog_settings(), sky, "a dry eye keeps the sky fog");

    // Eye in water: shorter than, and a different colour from, the sky fog.
    sim.set_fluid_state(FluidState {
        water_height: 1.0,
        eye_in_water: true,
        ..FluidState::NONE
    });
    assert!(sim.fluid_state().under_water());
    let water = sim.fog_settings();
    assert_ne!(water, sky, "a submerged eye must not keep the sky fog");
    assert!(
        water.end <= sky.end,
        "water fog cannot reach past the sky edge"
    );
    assert_eq!(water.start, 0.0, "water fog ramps from the eye");
    assert!(
        water.start < sky.start,
        "water fog is denser (starts nearer) than the sky fog"
    );

    // Eye in lava wins over water and is shorter still.
    sim.set_fluid_state(FluidState {
        water_height: 1.0,
        eye_in_water: true,
        lava_height: 1.0,
        eye_in_lava: true,
    });
    assert!(sim.fluid_state().under_lava());
    assert!(
        sim.fog_settings().end < water.end,
        "lava blinds faster than water"
    );
}

/// Real census entries as the version's table reports them (v770's
/// `hardness.rs`, dumped from a headless 26.2 server). Spelled out here so
/// the shell's unit tests assert against real numbers while still naming no
/// version crate; the `live`-gated test below proves these are the values
/// that actually arrive through the registry seam.
mod census {
    use lodestone_model::BlockHardness;

    pub const STONE: BlockHardness = BlockHardness {
        hardness: 1.5,
        requires_correct_tool: true,
    };
    pub const DIRT: BlockHardness = BlockHardness {
        hardness: 0.5,
        requires_correct_tool: false,
    };
    pub const OBSIDIAN: BlockHardness = BlockHardness {
        hardness: 50.0,
        requires_correct_tool: true,
    };
    pub const BEDROCK: BlockHardness = BlockHardness {
        hardness: -1.0,
        requires_correct_tool: false,
    };
}

/// Bare-hand inputs on flat, dry ground — the pose every timing figure below
/// is quoted at.
fn dry_ground(entry: lodestone_model::BlockHardness) -> BreakInputs {
    dig_break_inputs(entry, bare_handed_tool_mining(entry), false, true, false)
}

#[test]
fn bare_hand_correct_tool_is_the_negation_of_the_blocks_requirement() {
    // The defect this whole path exists to fix, pinned as a number. Feeding
    // `requires_correct_tool` straight into `correct_tool` is the naive
    // wiring: it reads like faithful data and flips stone from the 100
    // divider to the 30, breaking it 3.4x too fast — i.e. it reintroduces
    // "block breaking is too fast" while looking correct.
    let naive_stone = BreakInputs {
        hardness: census::STONE.hardness,
        correct_tool: census::STONE.requires_correct_tool,
        ..BreakInputs::default()
    };
    assert_eq!(
        naive_stone.ticks_to_break(),
        Some(45),
        "sanity: the naive wiring really is the fast one"
    );
    assert_eq!(
        dry_ground(census::STONE).ticks_to_break(),
        Some(151),
        "bare-hand stone must take 151 ticks (~8.0s), server-confirmed over RCON; \
         45 here means `correct_tool` was fed `requires_correct_tool` unnegated"
    );

    // Dirt moves the *other* way, so a test that only looked at stone could
    // be satisfied by a blanket `correct_tool: false`.
    assert_eq!(
        dry_ground(census::DIRT).ticks_to_break(),
        Some(15),
        "bare-hand dirt is the correct tool for its own drops: 30 divider"
    );
    let naive_dirt = BreakInputs {
        hardness: census::DIRT.hardness,
        correct_tool: census::DIRT.requires_correct_tool,
        ..BreakInputs::default()
    };
    assert_eq!(naive_dirt.ticks_to_break(), Some(51));
}

#[test]
fn a_resolved_tool_mining_speeds_up_the_dig_not_just_bare_hands() {
    // This is the actual regression the `sim.rs` wiring exists to close:
    // before it, `drive_mining` fed `BreakInputs::default()` for every tool
    // field regardless of what the version adapter resolved, so a diamond
    // pickaxe mined stone no faster than a fist. `dig_break_inputs` must
    // fold a real `ToolMining` straight through — reference numbers from
    // `docs/tool-mining.md` (also pinned externally by
    // `crates/lodestone-data/tests/tools.rs`): a diamond pickaxe (`speed:
    // 8.0`, `correct_tool: true`) on stone is 6 ticks, not the bare-hand
    // 151.
    let diamond_pickaxe = lodestone_model::ToolMining {
        speed: 8.0,
        correct_tool: true,
        damage_per_block: 1,
    };
    let tooled = dig_break_inputs(census::STONE, diamond_pickaxe, false, true, false);
    assert_eq!(tooled.tool_speed, 8.0);
    assert!(tooled.correct_tool);
    assert_eq!(
        tooled.ticks_to_break(),
        Some(6),
        "a diamond pickaxe on stone must be 6 ticks, matching the v770 tool oracle"
    );
    assert_eq!(
        dry_ground(census::STONE).ticks_to_break(),
        Some(151),
        "bare hand on the same block must be unaffected by the tooled case above"
    );
}

#[test]
fn tool_mining_item_lifts_the_hotbar_stacks_id_and_count_with_no_tool_override() {
    // `tool_mining_item` is what `drive_mining` feeds `VersionAdapter::tool_mining`
    // for the selected hotbar slot. It must carry the real item id and count
    // across, and leave `tool` at `Inherited` when the wire said nothing, so
    // `tool_mining` resolves the item's *built-in* tool from the version's
    // generated prototype table rather than silently treating every held item
    // as toolless. This is the control for
    // `an_explicit_wire_tool_override_survives_the_lift_to_the_version_seam`.
    let item_id: lodestone_model::Identifier =
        "minecraft:diamond_pickaxe".parse().expect("valid id");
    let held = lodestone_game::item::ItemStack::new(item_id.clone(), 1);
    let lifted = tool_mining_item(&held);
    assert_eq!(lifted.item, item_id);
    assert_eq!(lifted.count, 1);
    assert_eq!(
        lifted.components.tool,
        lodestone_model::ToolPatch::Inherited,
        "no wire override means Inherited — the item id alone must resolve the tool"
    );
}

/// An explicit `minecraft:tool` from the wire (`/give
/// …[minecraft:tool={…}]`, or a datapack item) must survive the lift into the
/// version seam.
///
/// It did not before: `tool_mining_item` built a fresh
/// `ItemComponents::default()`, i.e. `ToolPatch::Inherited`, so an overridden
/// tool resolved as if the *item default* applied — a custom-speed pickaxe
/// dug at its vanilla rate, and `[!minecraft:tool]` dug like a real pickaxe
/// instead of a bare hand. The canonical stack has carried the patch since
/// `67ff7c3`; this reads it back.
///
/// Both directions are checked, because `Removed` is the one that fails
/// *unsafely*: an item that should mine like a bare hand mining at tool speed
/// makes the client predict a break the server will not grant.
#[test]
fn an_explicit_wire_tool_override_survives_the_lift_to_the_version_seam() {
    use lodestone_game::item::{ComponentValue, ItemComponents, TOOL_COMPONENT};

    let item_id: lodestone_model::Identifier =
        "minecraft:diamond_pickaxe".parse().expect("valid id");
    let key: lodestone_model::Identifier = TOOL_COMPONENT.parse().expect("valid id");

    for patch in [
        lodestone_model::ToolPatch::Removed,
        // A rule-less tool with a distinctly non-vanilla speed: if the patch
        // were dropped, `tool_mining` would answer with the diamond
        // pickaxe's real table instead and the equality below would fail.
        lodestone_model::ToolPatch::Set(lodestone_model::ItemTool::new(
            Vec::new(),
            12.5,
            3,
            true,
        )),
    ] {
        let mut components = ItemComponents::new();
        components.insert(key.clone(), ComponentValue::Tool(patch.clone()));
        let held =
            lodestone_game::item::ItemStack::with_components(item_id.clone(), 1, components);
        assert_eq!(
            tool_mining_item(&held).components.tool,
            patch,
            "an explicit wire tool patch must reach `VersionAdapter::tool_mining`"
        );
    }
}

#[test]
fn submerged_reads_eye_in_water_not_the_fogs_under_water() {
    // Vanilla's `getDestroySpeed` gates the 5x underwater penalty on
    // `isEyeInFluid(WATER)` alone; `FluidState::under_water()` additionally
    // requires `in_water()` and is what the *fog* selects on. The two
    // disagree exactly here — an eye in water whose box is not — so reading
    // the fog's predicate would silently drop the penalty in that pose.
    let eye_only = FluidState {
        eye_in_water: true,
        ..FluidState::NONE
    };
    assert!(eye_only.eye_in_water);
    assert!(
        !eye_only.under_water(),
        "the two predicates must actually differ here, or this proves nothing"
    );

    let dry = dry_ground(census::STONE);
    let wet = dig_break_inputs(
        census::STONE,
        bare_handed_tool_mining(census::STONE),
        false,
        true,
        eye_only.eye_in_water,
    );
    // Compare the *rate*, not the tick count: `ticks_to_break` replays
    // vanilla's f32 accumulate-and-compare loop, so a 5x slower rate lands
    // near — not exactly on — 5x the ticks (the same rounding that makes
    // bare-hand stone 151 rather than the textbook 150).
    assert_eq!(
        wet.dig_speed(),
        dry.dig_speed() * 0.2,
        "submerged mining is 5x slower (the 0.2 submerged_mining_speed factor)"
    );
    assert!(
        wet.ticks_to_break().unwrap() > dry.ticks_to_break().unwrap() * 4,
        "and it shows up in the break time"
    );
}

#[test]
fn off_ground_mining_is_five_times_slower() {
    // `on_ground` was already wired before the hardness seam; keep it pinned
    // so a rewrite of the input builder cannot quietly drop it.
    let grounded = dry_ground(census::STONE);
    let airborne = dig_break_inputs(
        census::STONE,
        bare_handed_tool_mining(census::STONE),
        false,
        false,
        false,
    );
    assert_eq!(airborne.dig_speed(), grounded.dig_speed() / 5.0);
    assert!(
        airborne.ticks_to_break().unwrap() > grounded.ticks_to_break().unwrap() * 4,
        "off-ground mining must be materially slower"
    );
}

#[test]
fn tool_inputs_stay_at_bare_hand_defaults() {
    // `dry_ground` builds its inputs from `bare_handed_tool_mining`
    // specifically (an empty main hand), so `tool_speed` must stay at the
    // bare-hand `1.0` here — a live dig instead resolves a real
    // `ToolMining` through `VersionAdapter::tool_mining` in `drive_mining`.
    // Mining efficiency, haste and fatigue have no modeled source at all
    // yet (no enchantment/potion/attribute inputs), so those stay at
    // `BreakInputs::default` regardless of what is held.
    let inputs = dry_ground(census::STONE);
    assert_eq!(inputs.tool_speed, 1.0);
    assert_eq!(inputs.mining_efficiency, 0.0);
    assert_eq!(inputs.haste_amplifier, None);
    assert_eq!(inputs.mining_fatigue, None);
    assert_eq!(inputs.block_break_speed, 1.0);
}

/// Replay a held dig for `ticks` and report the crack stage the shell would
/// draw, mirroring `crack_target`'s read of `Mining::destroy_stage`.
fn stage_after(entry: lodestone_model::BlockHardness, ticks: u32) -> i32 {
    let pos = BlockPos::new(0, 64, 0);
    let inputs = dry_ground(entry);
    let mut machine = Mining::new();
    machine.start(pos, BlockFace::Up, &inputs, None);
    for _ in 0..ticks {
        machine.continue_(pos, BlockFace::Up, &inputs, None);
    }
    machine.destroy_stage()
}

#[test]
fn unbreakable_blocks_draw_no_crack_at_all() {
    // `hardness == -1.0` makes `progress_per_tick` return 0.0, so progress
    // never leaves 0.0 and `destroy_stage()` stays -1 — which is what
    // `crack_target` turns into `None`. Under the old fixed hardness bedrock
    // cracked like anything else.
    assert_eq!(dry_ground(census::BEDROCK).progress_per_tick(), 0.0);
    assert_eq!(dry_ground(census::BEDROCK).ticks_to_break(), None);
    for ticks in [0u32, 1, 10, 200] {
        assert_eq!(
            stage_after(census::BEDROCK, ticks),
            -1,
            "bedrock must never show a crack stage (t={ticks})"
        );
    }
}

#[test]
fn crack_stages_advance_at_per_block_rates() {
    // The visible half of the defect: under one fixed hardness every block
    // pulsed through all ten stages at the same speed. Obsidian is 100x
    // stone's hardness and must crawl where dirt races.
    let t = 8;
    let dirt = stage_after(census::DIRT, t);
    let stone = stage_after(census::STONE, t);
    let obsidian = stage_after(census::OBSIDIAN, t);
    assert!(
        dirt > stone && stone >= obsidian,
        "stages must order dirt > stone >= obsidian at t={t}, got {dirt}/{stone}/{obsidian}"
    );
    assert!(
        dirt >= 5,
        "dirt is half-broken in 8 ticks, got stage {dirt}"
    );
    assert_eq!(
        obsidian, 0,
        "obsidian (5000 ticks) must still be on stage 0 after 8 ticks"
    );
    // ... and it really does eventually crack, so `0` above is slowness and
    // not an unbreakable-style dead stop.
    assert!(stage_after(census::OBSIDIAN, 600) > 0);
}

#[cfg(feature = "live")]
#[test]
fn the_registry_seam_feeds_the_same_numbers_the_unit_tests_assume() {
    // Closes the loop: everything above asserts against hand-written census
    // constants, which would keep passing if `Sim` resolved no adapter at all
    // or the seam regressed to the trait's `None` default. This asserts the
    // shell's *own* lookup, for the protocol its config names.
    let sim = Sim::new(test_config());
    // Stage 5 deleted the `Sim.version_data` *field*; the adapter is the
    // `VersionData` resource. This gate still read the field and so had not
    // compiled since — invisible without `--features live`.
    let world = sim.ecs().read();
    let version = world.resource::<VersionData>();
    assert!(
        version.0.is_some(),
        "the `live` feature must compile a family in for protocol {}",
        sim.config.protocol
    );

    // Air is state 0 in every version's block-state registry, so it is the
    // one id the shell can name without naming a version.
    let air = version
        .block_hardness(id::AIR)
        .expect("air must resolve through the seam");
    assert_eq!(air.hardness, 0.0);

    // Find the census entries the unit tests above assume, by value rather
    // than by id (ids renumber every data bump).
    let entries: Vec<_> = (0..40_000)
        .filter_map(|id| version.block_hardness(id))
        .collect();
    assert!(
        entries.len() > 30_000,
        "expected a full state census, got {} entries",
        entries.len()
    );
    for expected in [
        census::STONE,
        census::DIRT,
        census::OBSIDIAN,
        census::BEDROCK,
    ] {
        assert!(
            entries.contains(&expected),
            "{expected:?} is not in the version's census — the hand-written \
             constants in `census` have drifted from the real table"
        );
    }

    // An id past the census reports unknown rather than a guess, which is
    // what makes `drive_mining` refuse to dig instead of inventing a rate.
    assert_eq!(version.block_hardness(u32::MAX), None);
}

/// Live break-timing gate for the shell's own mining inputs, against the
/// survival oracle (`lodestone-survival`, game :25565, RCON :25566).
///
/// The hermetic tests above prove the *arithmetic*. What they cannot prove is
/// the thing that made retiring the old fixed hardness risky: feeding a real
/// hardness moves the client's `STOP_DESTROY` from ~5 ticks to the block's
/// true completion tick, which is a change in **protocol interaction**, not
/// just in a number. The server has two branches on `STOP` and this change
/// swaps which one runs, so it has to be measured rather than reasoned about.
///
/// Both regimes are driven back-to-back on the same connection and the same
/// block, so the comparison is not across two runs of a shared server:
///
/// * **before** — the retired `LIVE_DIG_HARDNESS` (`0.05` for every block).
///   `STOP` lands at ~5 ticks, `getDestroyProgress * (ticks + 1)` is ≈`0.04`,
///   under the server's `0.7` gate, so the server sets `hasDelayedDestroy`
///   and finishes on its own timer: the block becomes air **seconds after**
///   the `STOP`.
/// * **after** — the shell's real inputs. `STOP` lands at tick 151, the
///   product is ≈`1.05`, over the gate, so the server takes the immediate
///   `destroyAndAck` branch: air lands **right behind** the `STOP`.
///
/// The `stop → air` gap is therefore the discriminator between the branches,
/// and the `start → air` total is the regression guard on player-visible
/// break time (which must *not* move).
///
/// ```text
/// cargo test -p lodestone-shell --features live --lib \
///     sim::tests::live_bare_hand_stone -- --ignored --nocapture
/// ```
#[cfg(feature = "live")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the lodestone-survival server on 127.0.0.1:25565 (RCON :25566)"]
async fn live_bare_hand_stone_timing_survives_the_real_hardness_seam() {
    // `Instant` was missing here and this whole gate did not compile under
    // `--features live`; `--all-targets` alone cannot see it and `--lib`
    // without the feature cannot either, which is the exact blind spot
    // `CLAUDE.md`'s second health-check command exists to close. Pre-existing
    // at `84ffba2`, found by running that command.
    use std::time::{Duration, Instant};

    use lodestone_client::{ClientBuilder, ClientHandle, LoginProfile, ServerAddress};
    use lodestone_testsupport::{AsyncRconClient as Rcon, poll_until, unique_username};

    /// The hardness this path used to feed for *every* block, kept only here
    /// as the "before" leg of the measurement. It is not reachable from
    /// production code any more, and must not become so again.
    const RETIRED_FIXED_HARDNESS: f32 = 0.05;

    /// One dig, driven tick-by-tick through the real [`Mining`] machine with
    /// every emitted action lowered onto the wire. Returns
    /// `(stop_tick, start_to_stop, start_to_air)`, with air read from the
    /// *server* over RCON — never from our own optimistic prediction.
    async fn dig(
        handle: &ClientHandle,
        rcon: &mut Rcon,
        pos: BlockPos,
        inputs: &BreakInputs,
        max_ticks: u32,
    ) -> Option<(u32, Duration, Duration)> {
        let mut machine = Mining::new();
        let face = BlockFace::West;
        let t0 = Instant::now();
        for action in machine.start(pos, face, inputs, None) {
            let _ = handle.send_action(action);
        }
        let mut stop_at = None;
        let mut ticks = 0u32;
        while machine.is_destroying() && ticks < max_ticks {
            tokio::time::sleep(Duration::from_millis(50)).await;
            ticks += 1;
            for action in machine.continue_(pos, face, inputs, None) {
                if matches!(
                    action,
                    ClientAction::BlockAction {
                        action: lodestone_model::BlockActionKind::StopDestroy,
                        ..
                    }
                ) {
                    stop_at = Some((ticks, t0.elapsed()));
                }
                let _ = handle.send_action(action);
            }
        }
        let (stop_tick, to_stop) = stop_at?;
        // Poll server truth. `execute if block` reports "Test passed" only on
        // a match, so this never mistakes an error string for a break.
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let resp = rcon
                .cmd(&format!(
                    "execute if block {} {} {} minecraft:air",
                    pos.x, pos.y, pos.z
                ))
                .await;
            if resp.contains("Test passed") {
                return Some((stop_tick, to_stop, t0.elapsed()));
            }
            if Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn place(rcon: &mut Rcon, pos: BlockPos, block: &str) -> bool {
        rcon.cmd(&format!("setblock {} {} {} {block}", pos.x, pos.y, pos.z))
            .await;
        rcon.cmd(&format!(
            "execute if block {} {} {} {block}",
            pos.x, pos.y, pos.z
        ))
        .await
        .contains("Test passed")
    }

    let user = unique_username();
    let protocol = test_config().protocol;
    let adapter = lodestone_registry::adapter_for_protocol(protocol)
        .expect("the `live` feature compiles a family in for the configured protocol");
    let (handle, mut events) = ClientBuilder::new(
        ServerAddress {
            host: "127.0.0.1".into(),
            port: 25565,
        },
        LoginProfile {
            username: user.clone(),
            uuid: uuid::Uuid::new_v4(),
        },
        adapter,
    )
    .connect()
    .await
    .expect("connect to lodestone-survival on 127.0.0.1:25565");
    // Drain the event stream so the driver's bounded channel never blocks.
    let drain = tokio::spawn(async move { while events.recv().await.is_some() {} });

    assert!(
        poll_until(
            Duration::from_secs(30),
            Duration::from_millis(100),
            || async {
                handle
                    .players()
                    .into_iter()
                    .find(|p| p.name.as_deref() == Some(user.as_str()))
            }
        )
        .await
        .is_some(),
        "player {user} never reached Play on the oracle"
    );

    let mut rcon = Rcon::connect(("127.0.0.1", 25566), "lodestone")
        .await
        .expect("connect RCON on 127.0.0.1:25566");
    // Survival is required (creative insta-breaks everything, making the
    // timing vacuous); op clears spawn protection; the effects keep a stray
    // mob, fall or hunger from killing the player mid-dig, which would
    // teleport the entity and strand every later command.
    let _ = rcon.cmd(&format!("op {user}")).await;
    let _ = rcon.cmd(&format!("gamemode survival {user}")).await;
    for eff in [
        "minecraft:resistance 999999 255 true",
        "minecraft:regeneration 999999 9 true",
        "minecraft:fire_resistance 999999 0 true",
        "minecraft:saturation 999999 9 true",
    ] {
        let _ = rcon.cmd(&format!("effect give {user} {eff}")).await;
    }

    let p = poll_until(
        Duration::from_secs(15),
        Duration::from_millis(200),
        || async { handle.position() },
    )
    .await
    .expect("client never reported a position");
    // Two blocks east at feet level: clear of the player box, inside reach,
    // and never the floor being stood on.
    let target = BlockPos::new(
        p.x.floor() as i32 + 2,
        p.y.floor() as i32,
        p.z.floor() as i32,
    );
    let gate = BlockPos::new(target.x, target.y, target.z + 2);
    for q in [target, gate] {
        for dy in 0..=1 {
            let _ = rcon
                .cmd(&format!(
                    "setblock {} {} {} minecraft:air",
                    q.x,
                    q.y + dy,
                    q.z
                ))
                .await;
        }
    }

    // Clear the server's `hasClientLoaded()` gate, which drops every
    // `player_action` for ~60 ticks after join. A hardness-0 block breaks on
    // START alone, so retrying it until it vanishes both proves the
    // instant-break branch and tells us the gate is open — without it the
    // first timed dig silently measures the gate instead of the block.
    let gate_deadline = Instant::now() + Duration::from_secs(30);
    let mut gate_cleared = false;
    while Instant::now() < gate_deadline {
        assert!(place(&mut rcon, gate, "minecraft:slime_block").await);
        let mut m = Mining::new();
        let gate_entry = lodestone_model::BlockHardness {
            hardness: 0.0,
            requires_correct_tool: false,
        };
        let inputs = dig_break_inputs(
            gate_entry,
            bare_handed_tool_mining(gate_entry),
            false,
            true,
            false,
        );
        assert!(inputs.progress_per_tick() >= 1.0, "hardness 0 is instant");
        for action in m.start(gate, BlockFace::Up, &inputs, None) {
            let _ = handle.send_action(action);
        }
        assert!(!m.is_destroying(), "an instant break retains no live dig");
        tokio::time::sleep(Duration::from_millis(500)).await;
        if rcon
            .cmd(&format!(
                "execute if block {} {} {} minecraft:air",
                gate.x, gate.y, gate.z
            ))
            .await
            .contains("Test passed")
        {
            gate_cleared = true;
            break;
        }
    }
    assert!(gate_cleared, "the server's client-loaded gate never opened");
    println!("load gate clear");

    // --- BEFORE: the retired fixed hardness ---
    assert!(place(&mut rcon, target, "minecraft:stone").await);
    let before = dig(
        &handle,
        &mut rcon,
        target,
        &BreakInputs {
            hardness: RETIRED_FIXED_HARDNESS,
            on_ground: true,
            ..BreakInputs::default()
        },
        400,
    )
    .await
    .expect("the retired-constant dig never reached air");
    println!(
        "BEFORE (fixed {RETIRED_FIXED_HARDNESS}): STOP at tick {} ({:?}), air at {:?} \
         — stop→air gap {:?}",
        before.0,
        before.1,
        before.2,
        before.2 - before.1
    );

    // --- AFTER: the shell's own inputs, from the real census entry ---
    assert!(place(&mut rcon, target, "minecraft:stone").await);
    let stone = dig_break_inputs(
        census::STONE,
        bare_handed_tool_mining(census::STONE),
        false,
        true,
        false,
    );
    assert_eq!(stone.ticks_to_break(), Some(151));
    let after = dig(&handle, &mut rcon, target, &stone, 400)
        .await
        .expect("the real-hardness dig never reached air");
    println!(
        "AFTER  (census stone): STOP at tick {} ({:?}), air at {:?} — stop→air gap {:?}",
        after.0,
        after.1,
        after.2,
        after.2 - after.1
    );

    // 1. The predictor now stops at the block's true completion tick.
    assert_eq!(
        after.0, 151,
        "the real-hardness dig must emit its STOP on tick 151, not earlier"
    );
    assert!(
        before.0 < 20,
        "sanity: the retired constant really did stop early (tick {})",
        before.0
    );

    // 2. Player-visible break time is unchanged — the regression guard. Both
    //    legs land near ~8s; the driving loop sleeps 50ms per tick so real
    //    scheduling jitter accumulates over 151 ticks, hence the window.
    for (label, total) in [("before", before.2), ("after", after.2)] {
        assert!(
            total > Duration::from_millis(6_500) && total < Duration::from_millis(12_000),
            "{label}: bare-hand stone must still take ~8s, got {total:?}"
        );
    }

    // 3. The branch really did swap: the retired constant left the server to
    //    finish the block seconds after the STOP (delayed-destroy), while the
    //    real hardness has the STOP itself destroy it (immediate).
    assert!(
        before.2 - before.1 > Duration::from_secs(3),
        "before: the server should have finished on its own timer well after the \
         early STOP, got a {:?} gap",
        before.2 - before.1
    );
    assert!(
        after.2 - after.1 < Duration::from_secs(2),
        "after: the STOP should destroy the block immediately (progress*(ticks+1) \
         ≈ 1.01 clears the 0.7 gate), got a {:?} gap",
        after.2 - after.1
    );

    // Best-effort cleanup on the shared oracle.
    for q in [target, gate] {
        let _ = rcon
            .cmd(&format!("setblock {} {} {} minecraft:air", q.x, q.y, q.z))
            .await;
    }
    let _ = rcon.cmd(&format!("effect clear {user}")).await;
    let _ = rcon.cmd(&format!("deop {user}")).await;
    drain.abort();
}

#[test]
fn new_generates_world_and_schedules_meshes() {
    let sim = Sim::new(test_config());
    assert!(!sim.chunk_world().is_empty(), "world should have chunks");
    assert!(sim.pending_meshes() > 0, "sections should be scheduled");
}

#[test]
fn all_scheduled_sections_mesh() {
    let mut sim = Sim::new(test_config());
    let meshes = sim.drain_all_meshes();
    assert!(!meshes.is_empty());
    assert!(meshes.iter().any(|m| m.mesh.quad_count() > 0));
}

#[test]
fn stepping_settles_the_player_on_the_ground() {
    let mut sim = Sim::new(test_config());
    for _ in 0..60 {
        sim.step(1.0 / 20.0);
    }
    assert!(
        sim.player().on_ground,
        "player should be standing on terrain"
    );
    assert_eq!(sim.stats.position[1], sim.player().position.y);
}

#[test]
fn mouse_look_updates_view_and_clears_delta() {
    let mut sim = Sim::new(test_config());
    let yaw0 = sim.player().yaw;
    sim.input_mut(|i| i.add_mouse(50.0, 0.0));
    sim.apply_mouse();
    assert_ne!(sim.player().yaw, yaw0);
    assert_eq!(sim.input().mouse_dx, 0.0);
}

/// Issue #203: `invertMouseX` must negate the yaw delta by the *exact*
/// same magnitude `apply_look`'s curve would otherwise produce, not just
/// change its sign in some direction. A test that only asserted
/// `delta.signum() != plain.signum()` would also pass for a shader-style
/// bug that inverts and also rescales — see `CLAUDE.md`'s note on the
/// *magnitude* species of vacuous test.
#[test]
fn invert_mouse_x_negates_the_yaw_delta_exactly() {
    // A raw `after - before` is not safe here: `apply_look` wraps yaw
    // into `[-180, 180)`, so if the fixture's starting yaw happens to
    // sit near that seam, the plain and inverted runs can wrap on
    // opposite sides and a naive subtraction reports deltas 360° apart
    // even though the underlying rotation is the exact negation. This
    // computes the shortest signed angular delta instead, the same
    // normalisation `apply_look` itself applies to the absolute angle.
    fn yaw_delta(before: f32, after: f32) -> f32 {
        (after - before + 180.0).rem_euclid(360.0) - 180.0
    }

    let mut plain = Sim::new(test_config());
    let yaw0 = plain.player().yaw;
    plain.input_mut(|i| i.add_mouse(50.0, 0.0));
    plain.apply_mouse();
    let plain_delta = yaw_delta(yaw0, plain.player().yaw);
    assert_ne!(plain_delta, 0.0, "the fixture must actually turn the player");

    let mut inverted = Sim::new(test_config());
    inverted.set_mouse_invert(true, false);
    let yaw0i = inverted.player().yaw;
    inverted.input_mut(|i| i.add_mouse(50.0, 0.0));
    inverted.apply_mouse();
    let inverted_delta = yaw_delta(yaw0i, inverted.player().yaw);

    assert_eq!(
        inverted_delta, -plain_delta,
        "invert_mouse_x must negate dx before the sensitivity curve, \
         producing the exact opposite yaw delta, not merely a different one"
    );
}

/// Issue #443: a `sensitivity` change must take effect on the **next tick of
/// the same `Sim`**, with no restart.
///
/// This is the assertion the issue needs and the one a naive gate misses.
/// Persistence already worked before this fix — `afba832` made the option
/// write to disk — so a gate that asserts the *stored* value changed passes
/// against the bug and proves nothing. It is the *precondition* species of
/// vacuous test: the setup, not the assert, is what is wrong.
///
/// The defect was that [`Sim::apply_mouse`] read `self.config.sensitivity`,
/// the **argv-derived** [`Config`] value, which is fixed for the process's
/// lifetime. Dragging the slider therefore persisted correctly and changed
/// nothing until relaunch.
///
/// Both deltas are **predicted exactly** from
/// [`lodestone_controller::sensitivity_factor`] rather than merely compared to
/// each other, and the value the *unfixed* code would produce is computed
/// alongside — without that third number, "the two deltas differ" is also
/// satisfied by a fix that scales by the wrong amount (`CLAUDE.md`'s
/// *magnitude* species). At vanilla's curve `(s·0.6 + 0.2)³ · 8 · 0.15`, a
/// 50-pixel drag gives 30.72° at slider 1.0, 1.05° at 0.1, and 7.5° at the
/// fixture's own config value of 0.5 — three well-separated numbers.
#[test]
fn a_sensitivity_change_applies_to_the_same_sim_without_a_restart() {
    // `apply_look` wraps yaw into `[-180, 180)`, so a raw `after - before`
    // can report deltas 360° apart if the fixture's yaw sits near the seam.
    // Same normalisation as `invert_mouse_x_negates_the_yaw_delta_exactly`.
    fn yaw_delta(before: f32, after: f32) -> f32 {
        (after - before + 180.0).rem_euclid(360.0) - 180.0
    }

    const DRAG_PX: f32 = 50.0;
    let cfg = test_config();
    // The value the pre-fix code read, and therefore the wrong hypothesis.
    let stale = DRAG_PX * lodestone_controller::sensitivity_factor(cfg.sensitivity);

    let mut sim = Sim::new(cfg);

    // One `Sim`, two sensitivities, no reconstruction between them — that is
    // the whole point. A test that built a second `Sim` would pass even if
    // the value were only read at construction.
    let mut measure = |sim: &mut Sim, slider: f32| {
        sim.set_sensitivity(slider);
        let before = sim.player().yaw;
        sim.input_mut(|i| i.add_mouse(DRAG_PX, 0.0));
        sim.apply_mouse();
        yaw_delta(before, sim.player().yaw)
    };

    for slider in [1.0_f32, 0.1] {
        let want = DRAG_PX * lodestone_controller::sensitivity_factor(slider);
        let got = measure(&mut sim, slider);
        assert!(
            (got - want).abs() < 1e-3,
            "slider {slider} must turn the player {want}° for a {DRAG_PX}px drag, \
             got {got}° — apply_mouse is not reading the pushed sensitivity"
        );
        assert!(
            (got - stale).abs() > 1.0,
            "slider {slider} produced {got}°, within 1° of the {stale}° the \
             argv-derived config value would give — the fix is not observable, \
             so this gate would pass against the bug"
        );
    }
}

/// As [`invert_mouse_x_negates_the_yaw_delta_exactly`], for `invertMouseY`
/// and pitch.
#[test]
fn invert_mouse_y_negates_the_pitch_delta_exactly() {
    let mut plain = Sim::new(test_config());
    let pitch0 = plain.player().pitch;
    plain.input_mut(|i| i.add_mouse(0.0, 30.0));
    plain.apply_mouse();
    let plain_delta = plain.player().pitch - pitch0;
    assert_ne!(plain_delta, 0.0, "the fixture must actually tilt the player");

    let mut inverted = Sim::new(test_config());
    inverted.set_mouse_invert(false, true);
    let pitch0i = inverted.player().pitch;
    inverted.input_mut(|i| i.add_mouse(0.0, 30.0));
    inverted.apply_mouse();
    let inverted_delta = inverted.player().pitch - pitch0i;

    assert_eq!(inverted_delta, -plain_delta, "invert_mouse_y must negate dy exactly");
}

/// Issue #202, end-to-end: `Sim::set_toggle_modes` (what `app.rs` calls
/// from `nav.toggle_sneak()`/`toggle_sprint()`) has to actually reach the
/// live `InputState` a key event drives — that push happens inside
/// [`Sim::step`], not at the setter itself, so this proves the wiring
/// rather than just the setter storing a bool nobody reads.
///
/// Includes a negative control (hold mode, the default): without it, a
/// version of this test that always reported "still engaged" would pass
/// just as well against a build that never wired toggle mode at all.
#[test]
fn toggle_sneak_option_reaches_live_input_and_survives_key_release() {
    let mut toggle = Sim::new(test_config());
    toggle.set_toggle_modes(true, false, false, false);
    // `step` is what actually applies the pushed option to `InputState`;
    // see that method's doc. Without this call, `set` below would still
    // run in hold mode.
    toggle.step(1.0 / 20.0);

    toggle.input_mut(|i| i.set(lodestone_controller::Action::Sneak, true));
    assert!(
        lodestone_controller::movement_intent(&toggle.input()).sneak,
        "a fresh press must engage toggle sneak"
    );
    toggle.input_mut(|i| i.set(lodestone_controller::Action::Sneak, false));
    assert!(
        lodestone_controller::movement_intent(&toggle.input()).sneak,
        "toggle sneak must survive key release, unlike hold mode"
    );

    // -- negative control -------------------------------------------------
    let mut hold = Sim::new(test_config());
    hold.set_toggle_modes(false, false, false, false);
    hold.step(1.0 / 20.0);
    hold.input_mut(|i| i.set(lodestone_controller::Action::Sneak, true));
    assert!(lodestone_controller::movement_intent(&hold.input()).sneak);
    hold.input_mut(|i| i.set(lodestone_controller::Action::Sneak, false));
    assert!(
        !lodestone_controller::movement_intent(&hold.input()).sneak,
        "hold mode must clear sneak on release, or the toggle assertions \
         above are not really exercising the toggle"
    );
}

/// As the sneak half above, for `key.sprint`/`toggle_sprint` — a
/// different `InputState` field with its own `set` branch, not merely
/// the same code path exercised twice. Sprint needs `forward` held too
/// (`movement_intent`'s gate), so this drives that as well.
#[test]
fn toggle_sprint_option_reaches_live_input_and_survives_key_release() {
    let mut toggle = Sim::new(test_config());
    toggle.set_toggle_modes(false, true, false, false);
    toggle.step(1.0 / 20.0);

    toggle.input_mut(|i| i.set(lodestone_controller::Action::Forward, true));
    toggle.input_mut(|i| i.set(lodestone_controller::Action::Sprint, true));
    assert!(
        lodestone_controller::movement_intent(&toggle.input()).sprint,
        "a fresh press must engage toggle sprint"
    );
    toggle.input_mut(|i| i.set(lodestone_controller::Action::Sprint, false));
    assert!(
        lodestone_controller::movement_intent(&toggle.input()).sprint,
        "toggle sprint must survive key release, unlike hold mode"
    );
}

#[test]
fn connected_sim_emits_one_move_per_physics_tick() {
    use crate::net::NetUpdate;
    let (net, actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    // Before login the adapter has no Play-state Move packet, so the shell
    // must not spew movement yet: drive to Connected first.
    feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
    sim.poll_net(); // → Connected
    assert_eq!(sim.session_phase(), SessionPhase::Connected);
    sim.step(5.0 / 20.0); // ~5 ticks, all now in-world.
    // Counted by *variant*, not as a total: the tick tail also emits one
    // `EndClientTick` per tick (vanilla's `Minecraft.tick` does the same), so a
    // bare count answers "how many actions" rather than "how many moves".
    let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
    let moves = sent
        .iter()
        .filter(|a| matches!(a, ClientAction::Move { .. }))
        .count();
    assert!(moves > 0, "a connected sim should send movement packets");
    assert_eq!(
        moves as u64,
        sim.tick_count(),
        "exactly one outbound Move per physics tick"
    );
    // The tick tail rides along one-for-one, and is the *last* thing each tick
    // sends — the ordering vanilla's own send site has.
    assert_eq!(
        sent.iter()
            .filter(|a| matches!(a, ClientAction::EndClientTick))
            .count() as u64,
        sim.tick_count(),
        "exactly one EndClientTick per physics tick"
    );
    assert!(
        matches!(sent.last(), Some(ClientAction::EndClientTick)),
        "the tick tail must be last in the tick's stream, got {:?}",
        sent.last()
    );
}

#[test]
fn move_is_withheld_until_connected() {
    // A sim that is merely Connecting (attached, not yet logged in) must send
    // nothing — otherwise every pre-Play tick is a dropped-action on the wire.
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    assert_eq!(sim.session_phase(), SessionPhase::Connecting);
    sim.step(5.0 / 20.0);
    assert!(
        sim.tick_count() > 0,
        "ticks must still run while connecting"
    );
    let sent = std::iter::from_fn(|| actions.try_recv().ok()).count();
    assert_eq!(sent, 0, "no movement should be sent before login");
}

/// Issue #23 (bell, `docs/block-entity-renderers.md`'s Bell section):
/// `Sim::bell_source` is the accessor `app.rs`'s new per-frame install calls
/// (`if let Some(f) = self.sim.bell_source() { render.set_bell_source(f); }`)
/// — a plain island-detector for that one call site, not a pixel gate. A
/// full through-the-wire proof needs a real `ClientHandle` (login, a real
/// chunk with a `minecraft:bell` state *and* a recorded block-entity entry),
/// which no test double in this crate builds yet — every existing chest/
/// skull/sign/bell pixel gate installs a hand-built closure on `RenderState`
/// directly rather than going through `Sim::*_source`, so that gap predates
/// this change and is shared by all four block-entity types, not bell alone.
/// This is the part that *is* checkable without one: the accessor must
/// track connection state exactly like its skull/sign siblings (`None`
/// before any net is attached, `Some` after), and the closure it returns
/// must be safe to call before login rather than panicking on the
/// not-yet-published `ClientHandle` — the same "empty rather than a panic"
/// contract `block_entities::bell_spawns_before_login_is_empty_rather_than_a_panic`
/// already pins for the free function underneath it.
#[test]
fn bell_source_tracks_connection_state_and_is_safe_before_login() {
    let mut sim = Sim::new(test_config());
    assert!(
        sim.bell_source().is_none(),
        "no net attached at all must report no source, matching skull_source/sign_source"
    );

    let (net, _actions, _feed) = NetClient::loopback_with_feed();
    sim.attach_net(net);
    let source = sim
        .bell_source()
        .expect("a net is attached, so a source must exist even before login completes");
    assert_eq!(
        source(glam::Vec3::ZERO),
        Vec::new(),
        "no ClientHandle has been published yet, so the closure must return \
         no spawns rather than panicking on the empty OnceLock"
    );
}

/// Both [`CollisionSource`] implementors must actually be `Send + Sync +
/// 'static`, or they could not be held in a `Resource` at all.
///
/// Asserted rather than reasoned about: the Stage 1 report recorded this as
/// "likely, unverified" for [`LiveCollision`] (which holds
/// `Arc<ChunkSection>`, `Arc<BlockAtlas>` and `Option<Arc<dyn
/// VersionAdapter>>`), and it is the single fact the whole Stage-2 collision
/// seam rests on. It compiles today because `Arc<dyn CollisionSource>` is
/// used; this pins it so the reason stays visible if it ever stops holding.
#[test]
fn both_collision_sources_are_send_sync_and_static() {
    fn assert_resource_shaped<T: CollisionSource>() {}
    assert_resource_shaped::<ChunkWorldCollision>();
    assert_resource_shaped::<LiveCollisionSource>();
}

// **Issue #38's three autopilot gates lived here.** They were
// `autopilot_plugin_is_registered_and_its_systems_actually_run` (the island
// gate: one tick with a goal set must move `AutopilotStatus` off `Idle`),
// `goto_chat_command_drives_the_player_toward_the_goal_over_real_ticks` (real
// displacement down a hand-carved corridor, with a sealed-corridor control),
// and `goto_chat_command_never_reaches_the_outbound_action_queue`.
//
// **They went with the dependency, and none of them was weakened to do it.**
// `lodestone-autopilot` is a pre-implemented *external* plugin now, so
// `lodestone-shell` does not depend on it at all — not optionally, not behind a
// feature — and a test here cannot name `AutopilotStatus` any more than
// production code can. The first two gates' subject moved rather than
// disappearing: `crates/plugins/lodestone-autopilot/tests/drives_to_goal.rs`
// installs `AutopilotPlugin` in a real `App`, drives a real `GameTick`
// schedule, and asserts real arrival against **jar-derived** collision, with
// unreachable-goal controls. That is strictly stronger evidence than the two
// gates here were, because it does not depend on the shell registering
// anything. What is genuinely gone is only the claim the shell ever registered
// it — which is the decision, not a regression in the plugin.
//
// The third gate's *surviving* half is directly below, and its `#goto`-specific
// half is what issue #118 (plugin command registration) will restore.

/// The `#` client-local namespace is still reserved by [`Sim::send_chat`] even
/// though nothing fills it: a `#`-prefixed line must be consumed and refused,
/// never composed into an outbound chat action where every other player on the
/// server would read it.
///
/// This is the surviving half of issue #38's
/// `goto_chat_command_never_reaches_the_outbound_action_queue`. That test also
/// asserted `#goto 3 4` returned `true` and reached
/// `lodestone_autopilot::AutopilotGoal`; both are gone with the dependency (see
/// `send_chat`'s doc and `sim/build.rs`), so the *interception* is what is left
/// to pin — and it is worth pinning on its own, because deleting it would
/// restore no capability and would start leaking `#` lines onto the wire.
///
/// # The control is the point
///
/// `assert!(actions.try_recv().is_err())` is the load-bearing line, and on its
/// own it is the *precondition* species of vacuous test: an empty outbound
/// queue is also exactly what a `Sim` produces when nothing is wired to it at
/// all. So an ordinary `/say` line runs first on the **same** `Sim` and must
/// land in the queue. Without that, this gate would pass on a `send_chat` that
/// had been gutted to send nothing whatsoever.
#[test]
fn a_hash_prefixed_line_is_consumed_locally_and_never_reaches_the_outbound_queue() {
    let (net, actions, _feed) = crate::net::NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);

    assert!(
        sim.send_chat("/say hi"),
        "control: an ordinary command line must report that it sent"
    );
    assert!(
        actions.try_recv().is_ok(),
        "control: the outbound action queue must actually carry an ordinary \
         line -- otherwise the emptiness asserted below proves nothing"
    );

    for line in ["#goto 3 4", "#goto", "#follow 1 2", "#"] {
        assert!(
            !sim.send_chat(line),
            "`{line}` is client-local and unhandled, so send_chat must report \
             that nothing was sent"
        );
        assert!(
            actions.try_recv().is_err(),
            "`{line}` must be consumed locally, never handed to the outbound \
             action queue where other players would read it"
        );
    }
}

/// The runtime half of the boundary decision: **the shipped client does not
/// navigate itself.** A type-level absence (`cargo tree` reporting no
/// `lodestone-autopilot` edge) says the crate is not linked; it does not say
/// the *behaviour* is gone, because the shell could in principle have grown its
/// own walker. This asserts the behaviour.
///
/// Deliberately the exact mirror of the deleted
/// `goto_chat_command_drives_the_player_toward_the_goal_over_real_ticks`: the
/// same flat corridor, the same `#goto 0 5`, the same 200 driven ticks. That
/// test measured the player closing to within 1.5 blocks of (0, _, 5) from
/// about 5 blocks out. Here the player must **not move**, which is why the
/// corridor is worth building at all — it removes the "they were stuck on
/// terrain anyway" explanation for a stationary result.
///
/// This is not a test that nothing is registered; it is a test that no chat
/// line makes the player walk. Re-registering `AutopilotPlugin` alone would
/// leave it passing (nothing sets an `AutopilotGoal`), which is correct: the
/// capability under test is the `#goto`-drives-the-player pair, and that pair
/// is what was removed.
#[test]
fn no_chat_line_makes_the_shipped_client_walk_itself() {
    let mut sim = Sim::new(test_config());
    let feet_y = sim.player().position.y.floor() as i32;
    // Same corridor the deleted drive gate carved, running +Z from spawn.
    for dz in -1..=6 {
        for dx in -1..=1 {
            sim.set_block_world([dx, feet_y - 1, dz], id::STONE);
            sim.set_block_world([dx, feet_y, dz], id::AIR);
            sim.set_block_world([dx, feet_y + 1, dz], id::AIR);
            sim.set_block_world([dx, feet_y + 2, dz], id::AIR);
        }
    }
    for _ in 0..20 {
        sim.step(1.0 / 20.0);
    }

    let before = sim.player().position;
    assert!(
        !sim.send_chat("#goto 0 5"),
        "`#goto` must be refused now that no plugin claims the `#` namespace"
    );
    for _ in 0..200 {
        sim.step(1.0 / 20.0);
    }
    let after = sim.player().position;

    let moved = ((after.x - before.x).powi(2) + (after.z - before.z).powi(2)).sqrt();
    assert!(
        moved < 0.5,
        "no chat line may drive the player: moved {moved:.2} blocks \
         horizontally over 200 ticks after `#goto 0 5` \
         (from {before:?} to {after:?}). The deleted issue-#38 drive gate \
         measured ~4 blocks of travel on this same corridor, so movement here \
         means something in the shell is navigating for the player again."
    );
}

/// The authority test for the stage, at the shell level: the components are
/// the *only* store, so a write through the `World` — which is what a plugin
/// gets — changes what the server is told on the next tick.
///
/// If `Sim` still held a `PlayerState` of its own, this would pass a write
/// into a field nobody reads and the wire would report the unmodified pose.
#[test]
fn a_write_through_the_world_reaches_the_wire() {
    use crate::net::NetUpdate;
    let (net, actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
    sim.poll_net();
    while actions.try_recv().is_ok() {}

    let local = sim.local_player();
    sim.ecs()
        .write()
        .get_mut::<PhysicsState>(local)
        .expect("local player")
        .0
        .position = Vec3d::new(11.5, 200.0, -3.5);

    sim.step(lodestone_ecs::TICK_PERIOD);
    let moved: Vec<_> = std::iter::from_fn(|| actions.try_recv().ok())
        .filter_map(|a| match a {
            ClientAction::Move { pos, .. } => Some(pos),
            _ => None,
        })
        .collect();
    assert_eq!(moved.len(), 1, "one move per tick");
    // No world to collide against in this fixture beyond the demo terrain far
    // below, so the tick's only change is gravity — x and z are untouched.
    assert!((moved[0].x - 11.5).abs() < 1e-9, "got {moved:?}");
    assert!((moved[0].z + 3.5).abs() < 1e-9, "got {moved:?}");
    // …and the accessor agrees with the wire, because there is one store.
    assert!((sim.player().position.x - 11.5).abs() < 1e-9);
}

/// The other half of the authority test: `Sim`'s accessors are views onto the
/// same components, not onto a copy. A write through the accessor must be
/// visible in the `World` a plugin queries.
#[test]
fn the_accessors_and_the_world_are_the_same_store() {
    let mut sim = Sim::new(test_config());
    sim.player_mut(|p| p.yaw = 42.0);
    sim.input_mut(|i| i.set(lodestone_controller::Action::Forward, true));

    let local = sim.local_player();
    let world = sim.ecs().read();
    assert_eq!(world.get::<PhysicsState>(local).expect("local").0.yaw, 42.0);
    assert_eq!(
        lodestone_controller::movement_intent(&world.resource::<RawInput>().0).forward,
        1.0
    );
}

/// **Stage 4's authority test at the shell level.** The `ChunkWorld` resource
/// is the *only* chunk store, so a write through the handle a plugin would get
/// (`sim.chunk_world()`, or `sim.ecs().resource::<ChunkWorld>()`) is what the
/// sim collides against, raycasts into and meshes.
///
/// If `Sim` still owned a `World` field, this would write into a store nobody
/// reads and `block_at_world` would report the pre-edit block.
#[test]
fn a_write_through_the_chunk_world_resource_is_what_the_sim_reads() {
    let sim = Sim::new(test_config());
    let feet = sim.player().position;
    let (bx, bz) = (feet.x.floor() as i32 + 4, feet.z.floor() as i32 + 4);
    let above = crate::worldgen::surface_height(bx, bz) + 4;

    assert_eq!(
        sim.block_at_world([bx, above, bz]),
        id::AIR,
        "the cell starts empty"
    );

    // The write goes through the *write* resource handle, not through any `Sim`
    // method — issue #423: the read handle `sim.chunk_world()` yields has no
    // write path.
    {
        let store = sim.chunk_world_write();
        let mut world = store.write();
        let chunk = world
            .get_mut(ChunkPos {
                x: bx.div_euclid(16),
                z: bz.div_euclid(16),
            })
            .expect("the fixture holds this column");
        chunk.column.set_block(
            bx.rem_euclid(16) as usize,
            above,
            bz.rem_euclid(16) as usize,
            PLACE_BLOCK,
        );
    }

    assert_eq!(
        sim.block_at_world([bx, above, bz]),
        PLACE_BLOCK,
        "the sim reads the store a plugin writes, with no propagation step"
    );
    // And collision sees it in the same instant — there is no cached clone to
    // invalidate any more. Before Stage 4 this needed
    // `Sim::set_block_world` to clear `demo_collision` by hand, and a missed
    // clear read as "I mined the block but still cannot walk through it".
    let source = sim.chunk_collision();
    let mut solid = false;
    source.with_view(&mut |view: &dyn CollisionView| {
        let mut boxes = Vec::new();
        view.collision_boxes(bx, above, bz, &mut boxes);
        solid = !boxes.is_empty();
    });
    assert!(
        solid,
        "the collision source reads the same store, uncached — a plugin's edit \
         is collidable on the next tick"
    );
}

/// The control for the test above: the same probe against a cell nobody wrote
/// must report empty, so "solid" is a measurement rather than a constant.
#[test]
fn the_collision_source_reports_empty_where_nothing_was_written() {
    let sim: Sim = Sim::new(test_config());
    let feet = sim.player().position;
    let (bx, bz) = (feet.x.floor() as i32 + 4, feet.z.floor() as i32 + 4);
    let above = crate::worldgen::surface_height(bx, bz) + 4;

    let source = sim.chunk_collision();
    let mut solid = false;
    source.with_view(&mut |view: &dyn CollisionView| {
        let mut boxes = Vec::new();
        view.collision_boxes(bx, above, bz, &mut boxes);
        solid = !boxes.is_empty();
    });
    assert!(!solid, "control: an untouched air cell must not collide");
}

/// `heal_dirty_columns` must actually be registered in the `Update` schedule
/// `Sim::step` runs — the island check for Stage 4's one system. A dirtied
/// column that `run_schedule(Update)` does not drain is a chunk seam that
/// stays baked against air forever.
#[test]
fn the_update_schedule_drains_the_dirty_column_set() {
    let mut sim = Sim::new(test_config());
    let _ = sim.drain_all_meshes();
    let pos = *sim
        .chunk_world()
        .read()
        .iter()
        .next()
        .expect("the fixture holds a column")
        .0;
    sim.terrain_mut(|t| t.dirty_columns.insert((pos.x, pos.z)));
    assert_eq!(sim.pending_meshes(), 0, "drained to a clean slate");

    sim.ecs().write().run_schedule(lodestone_ecs::Update);

    assert!(
        sim.terrain(|t| t.dirty_columns.is_empty()),
        "the Update schedule must drain the dirty set"
    );
    assert!(
        sim.pending_meshes() > 0,
        "and draining it must submit real mesh jobs, not just empty the set"
    );
}

#[test]
fn disconnected_sim_sends_nothing() {
    // Without a net attached, stepping must not attempt to send.
    let mut sim = Sim::new(test_config());
    sim.step(5.0 / 20.0);
    assert!(sim.net.is_none());
}

#[test]
fn mob_effect_applied_for_local_player_reaches_status_effects() {
    use crate::net::NetUpdate;
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 7 }).unwrap();
    // `ServerEntityId` — the "is this effect ours" test — is folded from
    // `ClientEvent::Login` on the net thread, not from `NetUpdate::LoggedIn`.
    // Production sees both for one packet; so does this test.
    ingest(&mut sim, login_event(7));
    sim.poll_net();
    assert_eq!(
        sim.server_entity_id(),
        Some(7),
        "setup: the id must be folded"
    );
    assert!(sim.player().effects.levitation.is_none());

    feed.send(NetUpdate::EffectApplied {
        entity_id: 7,
        effect: "levitation".into(),
        amplifier: 2,
        duration_ticks: 200,
        ambient: false,
        show_icon: true,
    })
    .unwrap();
    sim.poll_net();
    assert_eq!(
        sim.player().effects.levitation,
        Some(2),
        "the wire→StatusEffects seam must fold an effect for the local entity id"
    );
    // The same event must also reach the display model with its full data.
    let chips = crate::effects::chips_from(&sim.active_effects());
    assert_eq!(chips.len(), 1, "the HUD effect model must fold it too");
    assert_eq!(chips[0].label, "levitation III"); // amplifier 2 → level III
    assert_eq!(chips[0].time, "0:10"); // 200 ticks → 10 s

    feed.send(NetUpdate::EffectRemoved {
        entity_id: 7,
        effect: "levitation".into(),
    })
    .unwrap();
    sim.poll_net();
    assert!(sim.player().effects.levitation.is_none());
    assert!(
        sim.active_effects().is_empty(),
        "removal must clear the HUD effect model as well"
    );
}

#[test]
fn mob_effect_for_a_different_entity_is_not_applied_to_the_local_player() {
    use crate::net::NetUpdate;
    // `update_mob_effect` is entity-agnostic on the wire; only the entity id
    // that matches the local player's should ever mutate `sim.player`.
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 7 }).unwrap();
    ingest(&mut sim, login_event(7));
    sim.poll_net();

    feed.send(NetUpdate::EffectApplied {
        entity_id: 1234, // some other (mob) entity, not the local player
        effect: "levitation".into(),
        amplifier: 0,
        duration_ticks: 200,
        ambient: false,
        show_icon: true,
    })
    .unwrap();
    sim.poll_net();
    assert!(
        sim.player().effects.levitation.is_none(),
        "a remote entity's effect must not leak into the local player's StatusEffects"
    );
    assert!(
        sim.active_effects().is_empty(),
        "a remote entity's effect must not reach the local HUD overlay either"
    );
}

/// Hermetic proof that `NetUpdate::Particles` actually reaches the
/// emitter: idle, `stats`/the HUD counter would also read
/// `particles=0/0+0unres`, which cannot distinguish "the route works but
/// nothing has fired" from "the route is missing" (`grep -rn
/// "ClientEvent::Particles" crates/lodestone-shell/src/` returned zero
/// hits before this change). So this feeds a live event and asserts the
/// *caused* output, not the idle baseline.
#[test]
fn net_particles_reaches_the_emitter_and_resolves() {
    use crate::net::NetUpdate;
    use lodestone_client::Vec3;
    use lodestone_particle::Sheet;

    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
    sim.poll_net();

    // A headless `Sim` has no vanilla jar, so `flame`'s sheet has no atlas
    // UVs by default — install the same kind of fixture table
    // `particles.rs`'s own hermetic tests use, so `unresolved == 0` is
    // actually reachable without fetching `client.jar`.
    let rect = [0.0f32, 0.0, 0.0625, 0.0625];
    sim.particles_mut(|p| {
        p.install_test_sheet_uv(HashMap::from([((Sheet::Flame, 0u16), rect)]));
    });

    // Keep the particle origin within vanilla's 32-block render cutoff of
    // wherever `Sim::new` spawned the player.
    let origin = sim.player().position;
    feed.send(NetUpdate::Particles {
        kind: "flame".into(),
        long_distance: false,
        pos: Vec3::new(origin.x, origin.y, origin.z),
        offset: Vec3f::new(0.1, 0.1, 0.1),
        max_speed: 0.02,
        count: 9,
    })
    .unwrap();
    sim.poll_net();

    assert_eq!(
        sim.particles_mut(|p| p.engine_mut().particles().len()),
        9,
        "count must be honoured exactly once the event reaches the emitter"
    );
    let cam = sim.camera(1.0);
    let frame = sim.particles_mut(|p| {
        p.extract(&cam, 0.0, &|_, _, _| Some(lodestone_particle::FULL_BRIGHT))
    });
    assert_eq!(frame.alive, 9);
    assert_eq!(
        frame.unresolved, 0,
        "flame is a sheet-sourced type with an installed atlas entry"
    );
    assert_eq!(frame.drawn, 9);
}

/// How many particles the two hold measurements below run over. High enough
/// that the per-particle work dominates two resource moves by orders of
/// magnitude, well under `ParticleEngine::DEFAULT_CAPACITY` (16 384) so the
/// engine does not silently drop the tail.
const HOLD_MEASUREMENT_PARTICLES: i32 = 4_000;

/// The small end of the scaling measurement, a tenth of
/// [`HOLD_MEASUREMENT_PARTICLES`]. The two are compared against each other —
/// see [`extract_particles_does_not_hold_the_world_guard_across_the_per_particle_work`]
/// for why a *ratio between two particle counts* replaced a ratio against wall
/// time.
const HOLD_MEASUREMENT_PARTICLES_SMALL: i32 = HOLD_MEASUREMENT_PARTICLES / 10;

/// How much the guarded time may grow when the particle count grows 10x.
///
/// A guard held *outside* the per-particle work is O(1) in the count — the same
/// handful of resource moves either way — so the expectation is ~1.0. A guard
/// held *across* it is O(N).
///
/// Measured over eight runs, averaged per [`HOLD_MEASUREMENT_REPEATS`]:
///
/// | shape | ratio for a 10x count | margin to this bound |
/// |---|---|---|
/// | correct (guard outside the work) | **0.40 - 0.97x** | 3.1x |
/// | pre-fix (guard across the work) | **5.60 - 10.93x** | 1.9x |
///
/// The bound is placed between the two measured populations rather than at a
/// round number. Note the O(N) shape does **not** reach the naive 10.0 reliably:
/// the pre-fix extract carries O(1) setup, which is a larger share of the small
/// measurement, so a bound of 5 would have been inside its range — an earlier
/// draft used 5 and the control failed 1 run in 5.
///
/// Averaging is what made the populations separate. Single samples ran
/// 0.6-1.6x and 4.3-8.9x, which overlap far too closely for a threshold; the
/// spread was scheduler noise on a few tens of microseconds, and summing several
/// extracts attacks it directly rather than hiding it behind a rounder number.
const HOLD_SCALING_LIMIT: u128 = 3;

/// How many extracts each side of the ratio averages over.
///
/// Both hold measurements are a few tens of microseconds, where scheduler noise
/// is a large fraction of a single sample. Summing several extracts before
/// taking the ratio shrinks that without changing what is being measured, since
/// the count — the thing the ratio is *about* — is identical in every repeat.
const HOLD_MEASUREMENT_REPEATS: usize = 5;

/// Spawns `count` live particles around the player and returns the `Sim` and a
/// camera to extract them with.
fn sim_with_particles(count: i32) -> (Sim, Camera) {
    let mut sim = Sim::new(test_config());
    let origin = sim.player().position;
    sim.particles_mut(|p| {
        p.spawn_particles(
            "smoke",
            [origin.x, origin.y, origin.z],
            [0.5, 0.5, 0.5],
            0.02,
            count,
        );
    });
    let camera = sim.camera(1.0);
    (sim, camera)
}

/// [`sim_with_particles`] at the full [`HOLD_MEASUREMENT_PARTICLES`].
fn sim_with_many_particles() -> (Sim, Camera) {
    sim_with_particles(HOLD_MEASUREMENT_PARTICLES)
}

/// Guarded nanoseconds for one `extract_particles` over `count` particles.
fn guarded_ns_for_extract(count: i32) -> (u64, usize) {
    let (mut sim, camera) = sim_with_particles(count);
    sim.reset_lock_holds();
    let mut alive = 0;
    for _ in 0..HOLD_MEASUREMENT_REPEATS {
        alive = sim.extract_particles(&camera).alive;
    }
    (sim.lock_holds().total_ns, alive)
}

/// Guarded nanoseconds for the **pre-fix shape** over `count` particles: the
/// whole extract run inside the write guard.
///
/// `light` is the offline arm (`self.net == None`), so this *understates* the
/// old hold — the live arm additionally took a chunk-store lock per particle
/// inside it.
fn guarded_ns_for_prefix_shape(count: i32) -> (u64, usize) {
    let (mut sim, camera) = sim_with_particles(count);
    sim.reset_lock_holds();
    let mut alive = 0;
    for _ in 0..HOLD_MEASUREMENT_REPEATS {
        alive = lodestone_ecs::hold_write(sim.ecs(), |w| {
            w.resource_mut::<ParticleSim>()
                .0
                .extract(&camera, 0.0, &|_, _, _| None)
        })
        .alive;
    }
    (sim.lock_holds().total_ns, alive)
}

/// **The measurement §4.1(c) could not make.**
///
/// `Sim::extract_particles` was the longest `World` guard hold in the process:
/// it took the write guard by hand and held it across the whole extract *and*
/// one chunk-store lookup per live particle for light. `docs/world-unification.md`
/// bounded that structurally — "no guard spans a frame" — and said so out loud:
/// *treat the bound as structural, not measured*. A duration claim with nothing
/// measuring the duration is the species of vacuous test `CLAUDE.md` names, so
/// this is the number.
///
/// The assertion is a ratio of **guarded time at two particle counts**, not a
/// ratio against the call's own wall time.
///
/// It used to be the latter — guarded < 25% of wall — and that instrument was
/// wrong in a way worth recording, because it looked like the careful choice.
/// The reasoning was that an absolute nanosecond ceiling is a statement about
/// one machine, whereas both sides of a ratio are measured in the same run; the
/// doc claimed an expected value of "a fraction of a percent" against a 25%
/// threshold, i.e. two orders of margin.
///
/// Measured, the real figure was **27-33%**, so the test failed 5 runs in 6
/// standing still. Worse, and this is the part that makes it a bad instrument
/// rather than a mistuned one: the guarded work is O(1) while the wall time is
/// O(N), so the ratio is not scale-free at all — and *load* inflates wall time
/// far more than it inflates four lock acquisitions. **A busier machine made
/// this test more likely to pass.** It went green inside the full suite, where
/// contention stretched the wall time, and red standalone. "Green in the batch"
/// was therefore never evidence of anything.
///
/// What the property actually says is that the guard does not span the
/// *per-particle* work — a statement about **scaling**. So: extract over
/// [`HOLD_MEASUREMENT_PARTICLES_SMALL`] and over ten times as many, and require
/// the guarded time not to grow with the count. Both measurements sit in one
/// run under the same load, and neither is compared to a wall clock.
///
/// Its negative control is
/// [`the_pre_fix_shape_of_extract_particles_fails_the_hold_bound`], which
/// reproduces the old shape and must fail this same bound.
#[test]
fn extract_particles_does_not_hold_the_world_guard_across_the_per_particle_work() {
    let (small_ns, small_alive) = guarded_ns_for_extract(HOLD_MEASUREMENT_PARTICLES_SMALL);
    let (large_ns, large_alive) = guarded_ns_for_extract(HOLD_MEASUREMENT_PARTICLES);

    // The *world*-species guard: the flaw in a vacuous duration test lives in
    // the input, not the assert. An extract over an empty engine would satisfy
    // the bound below trivially, so assert the volume first — at both ends,
    // since the ratio is meaningless if either side did no work.
    assert!(
        small_alive >= HOLD_MEASUREMENT_PARTICLES_SMALL as usize
            && large_alive >= HOLD_MEASUREMENT_PARTICLES as usize,
        "the measurement needs real volume at both ends; alive={small_alive} and {large_alive}"
    );
    // A clock that cannot see the small case cannot produce a ratio either.
    assert!(
        small_ns > 0,
        "the hold meter reported 0 ns over {small_alive} particles, so no ratio below is \
         meaningful — the meter, not the guard, is what failed"
    );
    eprintln!(
        "extract_particles guarded time: {small_ns} ns over {small_alive} particles, \
         {large_ns} ns over {large_alive} — ratio {:.2}x for a 10x count \
         (bound {HOLD_SCALING_LIMIT}x)",
        large_ns as f64 / small_ns as f64
    );
    assert!(
        u128::from(large_ns) < u128::from(small_ns) * HOLD_SCALING_LIMIT,
        "the `World` guard must not span the per-particle work: guarded time grew from \
         {small_ns} ns to {large_ns} ns for a 10x particle count, which scales with the \
         count rather than staying flat"
    );
}

/// The negative control for the bound above, and the reason it is evidence
/// rather than decoration: the *pre-fix shape* — extract run inside the write
/// guard — must fail the same assertion, measured by the same counter.
///
/// This is deliberately hand-written rather than a switch on `Sim`: a test
/// switch would have to survive in production code, and what needs proving is
/// that the detector distinguishes two shapes, not that a flag works.
///
/// Under the scaling formulation this control is *stronger* than it was against
/// wall time. Holding the guard across the extract makes the guarded time equal
/// the work, so it is O(N) and lands near the full 10x — whereas the old
/// wall-time form asked it to exceed 25% of a quantity it *was*, which is
/// nearly tautological and would have been satisfied by any hold at all.
#[test]
fn the_pre_fix_shape_of_extract_particles_fails_the_hold_bound() {
    let (small_ns, small_alive) =
        guarded_ns_for_prefix_shape(HOLD_MEASUREMENT_PARTICLES_SMALL);
    let (large_ns, large_alive) = guarded_ns_for_prefix_shape(HOLD_MEASUREMENT_PARTICLES);

    assert!(
        small_alive >= HOLD_MEASUREMENT_PARTICLES_SMALL as usize
            && large_alive >= HOLD_MEASUREMENT_PARTICLES as usize,
        "same input volume as the positive case; alive={small_alive} and {large_alive}"
    );
    assert!(small_ns > 0, "the hold meter reported 0 ns over {small_alive} particles");
    eprintln!(
        "pre-fix shape guarded time: {small_ns} ns over {small_alive} particles, \
         {large_ns} ns over {large_alive} — ratio {:.2}x for a 10x count \
         (must reach {HOLD_SCALING_LIMIT}x)",
        large_ns as f64 / small_ns as f64
    );
    assert!(
        u128::from(large_ns) >= u128::from(small_ns) * HOLD_SCALING_LIMIT,
        "the detector must fire on the shape it exists to reject: holding the guard across \
         the extract grew guarded time only from {small_ns} ns to {large_ns} ns for a 10x \
         particle count, so the bound in \
         `extract_particles_does_not_hold_the_world_guard_across_the_per_particle_work` \
         is not discriminating"
    );
}

/// The frame-level claim, also measured: `Sim::step` takes **many short
/// guards**, not one long one.
///
/// `docs/world-unification.md` said "counted from the code it takes on the
/// order of 15 short guards plus ~8 per catch-up tick". This counts them, so a
/// future refactor that coalesced the frame into one long guard — which would
/// read as a tidy-up and would stall ingest for a whole frame — fails here.
/// The control for the mechanism is `lodestone_ecs`'s
/// `the_hold_meter_reports_a_deliberately_long_hold`.
#[test]
fn a_frame_takes_many_short_world_guards_and_no_long_one() {
    let mut sim = Sim::with_demo_world(test_config());
    // One frame long enough to run at least one catch-up tick.
    sim.step(0.1);

    sim.reset_lock_holds();
    let started = std::time::Instant::now();
    sim.step(0.1);
    let wall = started.elapsed();
    let holds = sim.lock_holds();

    eprintln!(
        "Sim::step(0.1): wall {:?}, {} holds totalling {} ns, longest {} ns",
        wall, holds.holds, holds.total_ns, holds.longest_ns
    );
    assert!(
        holds.holds >= 15,
        "a frame must be many short guards rather than one long one; counted {}",
        holds.holds
    );
    // A ceiling, not a target: 25 ms is "no single guard spans a 40 fps frame".
    // Absolute rather than a ratio here because a whole `step` legitimately
    // *is* mostly its two `run_schedule` holds, so a ratio would assert
    // nothing. Loose enough to survive a preempted CI core; the control above
    // shows a 30 ms hold is visible, so this ceiling can actually be crossed.
    assert!(
        holds.longest_ns < 25_000_000,
        "no single `World` guard in a frame may approach a frame: longest was {} ns",
        holds.longest_ns
    );
}

/// Vanilla's render cutoff (`ClientLevel.doAddParticle`): a particle
/// farther than 32 blocks from the viewer is dropped unless the packet
/// sets `long_distance`. Two events at the same far-away position, one
/// with the flag and one without, must differ in whether anything
/// spawns — proving the cutoff is actually wired to the flag rather than
/// always on or always off.
#[test]
fn long_distance_flag_gates_the_far_away_cutoff() {
    use crate::net::NetUpdate;
    use lodestone_client::Vec3;
    use lodestone_particle::Sheet;

    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
    sim.poll_net();
    sim.particles_mut(|p| {
        p.install_test_sheet_uv(HashMap::from([(
            (Sheet::Flame, 0u16),
            [0.0f32, 0.0, 0.0625, 0.0625],
        )]));
    });

    // Comfortably past the 32-block (sqrt(1024)) cutoff on every axis.
    let origin = sim.player().position;
    let far = Vec3::new(origin.x + 1000.0, origin.y, origin.z);

    feed.send(NetUpdate::Particles {
        kind: "flame".into(),
        long_distance: false,
        pos: far,
        offset: Vec3f::new(0.0, 0.0, 0.0),
        max_speed: 0.0,
        count: 3,
    })
    .unwrap();
    sim.poll_net();
    assert_eq!(
        sim.particles_mut(|p| p.engine_mut().particles().len()),
        0,
        "a far-away burst without long_distance must be dropped, not spawned off-screen"
    );

    feed.send(NetUpdate::Particles {
        kind: "flame".into(),
        long_distance: true,
        pos: far,
        offset: Vec3f::new(0.0, 0.0, 0.0),
        max_speed: 0.0,
        count: 3,
    })
    .unwrap();
    sim.poll_net();
    assert_eq!(
        sim.particles_mut(|p| p.engine_mut().particles().len()),
        3,
        "the same burst with long_distance set must bypass the cutoff"
    );
}

#[test]
fn session_phase_tracks_net_updates() {
    use crate::net::NetUpdate;
    use lodestone_model::Text;

    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    // Before any connection: purely local.
    assert_eq!(sim.session_phase(), SessionPhase::LocalOnly);

    // Attaching a live connection moves us to Connecting immediately, so the
    // menu shows a loading screen rather than a lie.
    sim.attach_net(net);
    assert_eq!(sim.session_phase(), SessionPhase::Connecting);

    // LoggedIn ⇒ Connected (the menu's "session_ready").
    feed.send(NetUpdate::LoggedIn { entity_id: 42 }).unwrap();
    sim.poll_net();
    assert_eq!(sim.session_phase(), SessionPhase::Connected);

    // A mid-game disconnect ⇒ Ended with the reason preserved, which is what
    // drives the menu's Error screen. Assert the reason survives, so a
    // blank/again-Connected mapping can't pass. `"Server closed"` is a
    // synthetic, not-a-vanilla-key reason (see `NetUpdate::Disconnected`'s
    // doc comment), hence `Text::literal` rather than `Text::translate`;
    // the translation-key path is covered separately by
    // `disconnect_reason_is_translated_through_the_language_table`.
    feed.send(NetUpdate::Disconnected(Box::new(Text::literal(
        "Server closed",
    ))))
    .unwrap();
    sim.poll_net();
    match sim.session_phase() {
        SessionPhase::Ended(reason) => {
            assert!(reason.contains("Server closed"), "reason lost: {reason}");
        }
        other => panic!("expected Ended, got {other:?}"),
    }
}

/// Control for the two tests below: proves the "no raw key reaches the
/// screen" assertion can actually fail, i.e. it is discriminating rather
/// than vacuous (`CLAUDE.md`'s evidence standard). `test_config()` is
/// `Mode::Headless`, so `Sim::new` always takes the demo-palette path
/// (`BlockResources::load(false)`), which never loads a language table —
/// `sim.language` is deterministically `None` here regardless of the
/// environment. With no table, `resolve_text` still lowers the
/// `Translate` node (via `lodestone_game::text::resolve`), but with
/// nothing to translate it and no `fallback` set, it falls back to the
/// key itself — reproducing byte-for-byte the pre-#68 defect
/// (`net::forward` used to send `reason.to_plain_string()`, which hits
/// the same "no match, no fallback ⇒ render the key" path against its
/// own tiny built-in table). If this ever changed to also disappear the
/// key, the positive test below would no longer be proof of anything.
#[test]
fn disconnect_reason_without_a_language_table_falls_back_to_the_raw_key() {
    use crate::net::NetUpdate;
    use lodestone_model::Text;

    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    assert!(
        sim.language.is_none(),
        "control's premise requires no language table loaded"
    );
    sim.attach_net(net);
    feed.send(NetUpdate::Disconnected(Box::new(Text::translate(
        "multiplayer.disconnect.kicked",
        vec![],
    ))))
    .unwrap();
    sim.poll_net();
    match sim.session_phase() {
        SessionPhase::Ended(reason) => {
            assert!(
                reason.contains("multiplayer.disconnect.kicked"),
                "control failed to reproduce the raw-key defect: {reason}"
            );
        }
        other => panic!("expected Ended, got {other:?}"),
    }
}

/// The proof (issue #68): a real translation key reaches `Screen::Error`
/// as the real English vanilla ships for it, not as the raw key. The
/// expected string is not this test's own formatter's output — it is
/// copied verbatim from the real vanilla `en_us.json`
/// (`.cache/mc/26.2/src/assets/minecraft/lang/en_us.json:5773`,
/// `"multiplayer.disconnect.kicked": "Kicked by an operator"`), i.e. a
/// hand-decoded spec example per `CLAUDE.md`'s evidence standard, so
/// this can't pass by agreeing with itself. The fixture below carries
/// only that one real entry rather than the whole ~500 KiB table so the
/// test stays hermetic and has no `client.jar`/`LODESTONE_ASSETS`
/// dependency that could go missing in CI — `Language::from_json_bytes`
/// is the same parser [`crate::resources::BlockResources::try_vanilla`]
/// feeds the real file through, so this is not a bespoke lookup path.
#[test]
fn disconnect_reason_is_translated_through_the_language_table() {
    use crate::net::NetUpdate;
    use lodestone_assets::Language;
    use lodestone_model::Text;

    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    let lang = Language::from_json_bytes(
        br#"{"multiplayer.disconnect.kicked": "Kicked by an operator"}"#,
    )
    .expect("valid language JSON");
    sim.language = Some(Arc::new(lang));
    sim.attach_net(net);
    feed.send(NetUpdate::Disconnected(Box::new(Text::translate(
        "multiplayer.disconnect.kicked",
        vec![],
    ))))
    .unwrap();
    sim.poll_net();
    match sim.session_phase() {
        SessionPhase::Ended(reason) => {
            assert!(
                reason.contains("Kicked by an operator"),
                "translated English missing: {reason}"
            );
            assert!(
                !reason.contains("multiplayer.disconnect.kicked"),
                "raw key leaked through the translator: {reason}"
            );
        }
        other => panic!("expected Ended, got {other:?}"),
    }
}

#[test]
fn session_phase_reports_net_error_as_ended() {
    use crate::net::NetUpdate;
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    feed.send(NetUpdate::Error("connection refused".into()))
        .unwrap();
    sim.poll_net();
    match sim.session_phase() {
        SessionPhase::Ended(reason) => {
            assert!(reason.contains("connection refused"), "got {reason}");
        }
        other => panic!("expected Ended, got {other:?}"),
    }
}

#[test]
fn end_session_tears_down_and_a_fresh_connect_afterward_starts_clean() {
    // The real acceptance test for `Sim::end_session`: not just that it
    // clears fields, but that a *second* connect afterward behaves
    // exactly like the first, with nothing from the old session leaking
    // through.
    use crate::net::NetUpdate;
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 7 }).unwrap();
    ingest(&mut sim, login_event(7));
    sim.poll_net();
    assert_eq!(sim.session_phase(), SessionPhase::Connected);

    // Populate every read-model `end_session` is responsible for
    // clearing, so this test can actually observe the reset rather than
    // asserting on fields that were already empty. The vitals go in through
    // the *net thread's* fold (`ingest`) because that is now the only writer;
    // the chat log still arrives on the `NetUpdate` channel.
    feed.send(NetUpdate::Chat {
        text: lodestone_model::Text::literal("hello"),
        player: false,
        sender: None,
    })
    .unwrap();
    ingest(
        &mut sim,
        lodestone_client::ClientEvent::HealthChanged {
            health: 12.0,
            food: 8,
            saturation: 3.0,
        },
    );
    // A shared-fold component that is *not* a vital, to pin the other half of
    // the stale-note fix: before this change `end_session` left the previous
    // server's sidebar standing.
    ingest(
        &mut sim,
        lodestone_client::ClientEvent::DisplayObjective {
            slot: lodestone_model::event::DisplaySlot::Sidebar,
            objective: Some("kills".into()),
        },
    );
    sim.poll_net();
    assert!(
        !sim.recent_chat(10).is_empty(),
        "setup: chat must be populated before the teardown can be observed clearing it"
    );
    assert_eq!(sim.health(), Some(12.0), "setup: health must be populated");
    assert_eq!(
        sim.server_entity_id(),
        Some(7),
        "setup: entity id must be populated"
    );
    assert_eq!(
        displayed_sidebar(&sim).as_deref(),
        Some("kills"),
        "setup: the sidebar must be populated"
    );

    sim.end_session();

    assert!(sim.net().is_none(), "the connection must be dropped");
    assert_eq!(sim.session_phase(), SessionPhase::LocalOnly);
    assert!(sim.recent_chat(10).is_empty(), "chat log must clear");
    assert_eq!(sim.health(), None, "health must clear");
    assert_eq!(sim.food(), None, "food must clear");
    assert_eq!(
        sim.server_entity_id(),
        None,
        "the local entity id must clear"
    );
    assert_eq!(
        displayed_sidebar(&sim),
        None,
        "the previous server's sidebar must clear too — §4.1(c) made this \
         reachable from `Sim.local`, so the old 'it goes away with `net`' \
         reasoning no longer holds"
    );

    // The negative control this test exists for: a fresh connect
    // afterward must reach `Connected` and must not carry the old
    // session's chat forward, proving the reset actually took rather
    // than merely reporting empty because nothing polled yet.
    let (net2, _actions2, feed2) = NetClient::loopback_with_feed();
    sim.attach_net(net2);
    assert_eq!(sim.session_phase(), SessionPhase::Connecting);
    feed2.send(NetUpdate::LoggedIn { entity_id: 9 }).unwrap();
    ingest(&mut sim, login_event(9));
    sim.poll_net();
    assert_eq!(sim.session_phase(), SessionPhase::Connected);
    assert_eq!(sim.server_entity_id(), Some(9));
    assert!(
        sim.recent_chat(10).is_empty(),
        "the new session must not inherit the old one's chat"
    );
}

#[test]
fn inbound_chat_is_logged_and_typed_lines_route_to_the_action_seam() {
    use crate::net::NetUpdate;
    use lodestone_client::ClientAction;
    let (net, actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);

    // Inbound server chat must surface in the HUD log (not merely logged).
    feed.send(NetUpdate::Chat {
        text: lodestone_model::Text::literal("hello world"),
        player: false,
        sender: None,
    })
    .unwrap();
    sim.poll_net();
    let lines: Vec<String> = sim.recent_chat(10).into_iter().map(|(l, _)| l).collect();
    assert_eq!(
        lines,
        vec!["hello world".to_string()],
        "inbound chat must reach the display log"
    );

    // Typed lines route through the one outbound action seam: a leading '/'
    // is a command (slash stripped), otherwise a chat message.
    assert!(sim.send_chat("/say hi"), "a command line must send");
    assert!(sim.send_chat("plain message"), "a chat line must send");
    // Anti-vacuity: a blank line must send *nothing*, so "everything sends"
    // can't pass — and neither can "nothing sends", guarded by the two above.
    assert!(!sim.send_chat("   "), "blank input must not send");

    // Nothing is intercepted on the way out any more (#382). `/givedebug`
    // used to be rewritten into `/give @s …` *here*, with a local echo
    // pushed into the chat log and — when malformed — nothing sent at all.
    // Both halves of that are now the server's business.
    let before = sim.recent_chat(10).len();
    assert!(
        sim.send_chat("/givedebug minecraft:diamond_pickaxe 1"),
        "a /givedebug line is now an ordinary command and must reach the wire"
    );
    assert!(
        sim.send_chat("/givedebug"),
        "even the malformed form goes to the server; nothing absorbs it locally"
    );
    assert_eq!(
        sim.recent_chat(10).len(),
        before,
        "no local echo and no local error line — that was the wrapper's job"
    );

    let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
    assert_eq!(
        sent,
        vec![
            ClientAction::SendCommand {
                command: "say hi".into()
            },
            ClientAction::SendChat {
                text: "plain message".into()
            },
            // Verbatim, *not* rewritten to `give @s minecraft:diamond_pickaxe 1`
            // — which is the whole assertion.
            ClientAction::SendCommand {
                command: "givedebug minecraft:diamond_pickaxe 1".into()
            },
            ClientAction::SendCommand {
                command: "givedebug".into()
            },
        ],
        "exactly the four non-blank lines route, with the command slash stripped"
    );
}

#[test]
fn chat_lines_age_as_the_clock_advances() {
    use crate::net::NetUpdate;
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);

    feed.send(NetUpdate::Chat {
        text: lodestone_model::Text::literal("aged line"),
        player: false,
        sender: None,
    })
    .unwrap();
    sim.poll_net();
    // Freshly received: age is ~0.
    assert!(
        sim.recent_chat(1)[0].1 < 0.001,
        "a just-received line is young"
    );

    // Advancing the sim clock ages the line by real elapsed time.
    sim.step(2.5);
    let age = sim.recent_chat(1)[0].1;
    assert!(
        (2.4..=2.6).contains(&age),
        "line age must track the sim clock, got {age}"
    );
}

/// The HUD's health/food accessors must reflect the **net thread's** fold.
///
/// This used to feed `NetUpdate::Health` and assert the shell's own arm folded
/// it. That arm was the duplicate the vitals collapse deleted, so the test now
/// drives `ClientEvent::HealthChanged` through the one remaining fold — the
/// `NetIngest` schedule inside this `Sim`'s own `World`, which is exactly what
/// production does — and asserts the same accessors. Sharper, not weaker: the
/// old version could have passed with the production fold missing entirely.
#[test]
fn server_health_and_food_reach_the_hud_accessors() {
    let (net, _actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    // Off a live server there is no survival state, so the HUD draws no bars.
    assert_eq!(sim.health(), None);
    assert_eq!(sim.food(), None);

    ingest(
        &mut sim,
        lodestone_client::ClientEvent::HealthChanged {
            health: 14.0,
            food: 17,
            saturation: 2.5,
        },
    );
    // Both fields must land — a one-sided store would leave the other None.
    assert_eq!(sim.health(), Some(14.0));
    assert_eq!(sim.food(), Some(17));
}

/// The negative control for the two tests above: enqueueing without running
/// the schedule must change nothing, so "the accessor reports 14" is evidence
/// the *fold* ran and not merely that the event was constructed.
#[test]
fn queueing_health_without_running_net_ingest_folds_nothing() {
    let mut sim = Sim::new(test_config());
    let local = sim.local;
    sim.write(|w| {
        w.resource_mut::<lodestone_ecs::ingest::IngestQueue>().push(
            lodestone_client::ClientEvent::HealthChanged {
                health: 14.0,
                food: 17,
                saturation: 2.5,
            },
        );
    });
    assert_eq!(
        sim.health(),
        None,
        "pushing must not fold; only NetIngest folds"
    );
    // …and the local player really is the entity the fold would write, so the
    // assertion above is not passing because it is looking at the wrong one.
    assert!(
        sim.read(|w| w.get::<Vitals>(local).is_some()),
        "the local player must carry Vitals for this control to mean anything"
    );
}

#[test]
fn server_experience_reaches_the_hud_accessor() {
    let (net, _actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    // Off a live server (or before the first packet) there is no real XP
    // value, so the HUD must not draw a faked bar.
    assert_eq!(sim.experience(), None);

    ingest(
        &mut sim,
        lodestone_client::ClientEvent::ExperienceChanged {
            progress: 0.6,
            level: 30,
            total: 1395,
        },
    );
    assert_eq!(sim.experience(), Some((0.6, 30, 1395)));
}

#[test]
fn title_events_fold_into_the_title_overlay() {
    use crate::net::NetUpdate;
    use lodestone_model::{ClientEvent, Text};

    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    // No title yet → nothing to draw.
    assert!(sim.title_overlay().is_none());

    feed.send(NetUpdate::TitleEvent(ClientEvent::TitleText {
        text: Text::literal("Welcome"),
    }))
    .unwrap();
    feed.send(NetUpdate::TitleEvent(ClientEvent::SubtitleText {
        text: Text::literal("to the server"),
    }))
    .unwrap();
    sim.poll_net();

    let (title, subtitle, _alpha) = sim
        .title_overlay()
        .expect("a server-sent title must reach the HUD accessor");
    assert_eq!(title, "Welcome");
    assert_eq!(subtitle.as_deref(), Some("to the server"));

    // A clear packet must empty the overlay again.
    feed.send(NetUpdate::TitleEvent(ClientEvent::TitlesCleared {
        reset_times: false,
    }))
    .unwrap();
    sim.poll_net();
    assert!(sim.title_overlay().is_none());
}

#[test]
fn game_info_chat_folds_into_the_action_bar_not_the_feed() {
    use crate::net::NetUpdate;
    use lodestone_model::Text;

    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    assert!(sim.action_bar_overlay().is_none());

    feed.send(NetUpdate::ActionBar(Text::literal("Boss incoming")))
        .unwrap();
    sim.poll_net();

    let (text, alpha) = sim
        .action_bar_overlay()
        .expect("a GameInfo message must reach the action-bar accessor");
    assert_eq!(text, "Boss incoming");
    assert!(alpha > 0.0, "a fresh action-bar message is fully opaque");
    // It must not have leaked into the chat scrollback.
    assert!(
        sim.recent_chat(10).is_empty(),
        "GameInfo is the action bar, not chat — it must not enter the feed"
    );
}

/// The held-item name highlight (issue #126) end to end: selecting an
/// item's name reaches [`Sim::held_item_overlay`] — the accessor
/// `app.rs`'s `hud_frame.held_item = self.sim.held_item_overlay()` reads
/// every frame — and, the property `docs/held-item-name-tooltip.md`
/// calls out as the one non-obvious constraint, switching between two
/// hotbar slots that hold the **same** item does not retrigger it.
#[test]
fn held_item_overlay_reaches_pixels_and_keys_on_identity_not_slot() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    assert_eq!(
        sim.held_item_overlay(),
        None,
        "control: nothing selected at spawn must show no overlay"
    );

    // Identical dirt in both hotbar slot 0 (selected by default) and
    // slot 1.
    give_main_hand_item(&mut sim, "minecraft:dirt");
    let local = sim.local;
    sim.write(|w| {
        if let Some(mut menus) = w.get_mut::<lodestone_ecs::SessionMenus>(local) {
            menus.0.apply(&lodestone_model::ClientEvent::InventorySlotChanged {
                slot: 1,
                item: Some(lodestone_model::ItemStack::new(
                    "minecraft:dirt".parse().expect("valid item id"),
                    1,
                )),
            });
        }
    });

    sim.step(1.0 / 20.0);
    let (name, alpha) = sim
        .held_item_overlay()
        .expect("selecting an item must show its name — the pixel this feature draws");
    assert_eq!(name, "Dirt");
    assert_eq!(
        alpha, 1.0,
        "Hud.java:639: a freshly triggered highlight is at full opacity, no fade-in"
    );

    // Run past the hold phase into the fade so alpha is measurably below
    // 1.0 before the slot switch below — otherwise a retrigger bug could
    // hide behind "alpha was already 1.0 anyway".
    for _ in 0..35 {
        sim.step(1.0 / 20.0);
    }
    let faded_alpha = sim
        .held_item_overlay()
        .map(|(_, a)| a)
        .expect("control: must still be showing (fading, not yet expired)");
    assert!(
        (0.0..1.0).contains(&faded_alpha),
        "control: must be mid-fade before the slot switch, got {faded_alpha}"
    );

    // The subject: selecting slot 1, which holds the identical item,
    // must not restart the timer.
    sim.select_slot(1);
    sim.step(1.0 / 20.0);
    let after_switch = sim
        .held_item_overlay()
        .map(|(_, a)| a)
        .expect("still showing: the countdown continues, it does not vanish");
    assert!(
        after_switch <= faded_alpha,
        "switching between two slots holding the same item must not restart the \
         timer (Hud.java:1194-1196's item-and-hover-name identity check, not slot \
         equality) — alpha went from {faded_alpha} to {after_switch}, which only \
         happens if it retriggered"
    );
}

/// The read-through the shell now depends on: it folds nothing itself, so
/// the rows must come out of the **client's** one `SessionTabList`.
///
/// `ingest_session_event` runs the same `lodestone_ecs::session` systems the
/// real net thread runs (see `NetClient::session`); what this pins is the
/// chain `component → NetClient::tab_list → Sim::tab_list_view`, which is
/// exactly what the deleted `NetUpdate::TabListEvent` fold used to short.
#[test]
fn tab_overlay_rows_read_the_clients_one_folded_tab_list() {
    use lodestone_model::{ClientEvent, GameMode, PlayerListEntry, Text};
    use uuid::Uuid;

    let (net, _actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);

    let alice = Uuid::from_u128(1);
    let bob = Uuid::from_u128(2);
    let ingest = |sim: &Sim, event: ClientEvent| {
        sim.net().expect("net attached").ingest_session_event(event);
    };
    ingest(
        &sim,
        ClientEvent::PlayerListUpdate {
            entries: vec![
                PlayerListEntry {
                    uuid: bob,
                    name: Some("Bob".into()),
                    game_mode: Some(GameMode::Spectator),
                    latency: Some(30),
                    display_name: None,
                    listed: Some(true),
                    properties: None,
                },
                PlayerListEntry {
                    uuid: alice,
                    name: Some("Alice".into()),
                    game_mode: Some(GameMode::Survival),
                    latency: Some(12),
                    display_name: Some(Text::literal("Alice the Brave")),
                    listed: Some(true),
                    properties: None,
                },
            ],
        },
    );

    // The whole row, not just the name: the projection now carries the game mode
    // and the latency *band*, and asserting only the names would not notice
    // either being dropped on the way through — which is exactly what the
    // pre-`TabListView` flattening did.
    let rows = |sim: &Sim| -> Vec<(String, &'static str, bool)> {
        sim.tab_list_view()
            .rows
            .iter()
            .map(|row| {
                (
                    crate::overlay::spans_text(&row.name),
                    row.ping_sprite,
                    row.spectator,
                )
            })
            .collect()
    };
    assert_eq!(
        rows(&sim),
        vec![
            ("Alice the Brave".to_string(), "icon/ping_5", false),
            // Spectators sort last and draw dimmed; both facts are in the row.
            ("Bob".to_string(), "icon/ping_5", true),
        ],
        "tab overlay rows must come from the client's folded TabList state"
    );

    ingest(
        &sim,
        ClientEvent::PlayerListRemove {
            profile_ids: vec![alice],
        },
    );
    assert_eq!(rows(&sim), vec![("Bob".to_string(), "icon/ping_5", true)]);
}

/// Issue #410's missing hop: `crate::gpu::gather_crack_targets` and
/// `BlockDestructionOverlays::iter` were both proven in `gpu/outline.rs`'s
/// own gate, but the issue was closed with nothing in production calling the
/// gather — `app.rs` only ever passed `Sim::crack_target()`'s single local
/// dig through. This proves `Sim::crack_targets()` actually walks
/// `SessionBlockDestruction` for two *different* breaking entities, not just
/// the local target the pipeline gate already covers in isolation.
#[test]
fn crack_targets_reaches_every_other_players_overlay_not_just_the_local_dig() {
    use lodestone_model::ClientEvent;

    let (net, _actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);

    let ingest = |sim: &Sim, event: ClientEvent| {
        sim.net().expect("net attached").ingest_session_event(event);
    };
    ingest(
        &sim,
        ClientEvent::BlockDestruction {
            entity_id: 301,
            pos: BlockPos::new(10, 64, 20),
            progress: 3,
        },
    );
    ingest(
        &sim,
        ClientEvent::BlockDestruction {
            entity_id: 402,
            pos: BlockPos::new(-5, 70, 8),
            progress: 7,
        },
    );

    let targets = sim.crack_targets();
    assert_eq!(
        targets.len(),
        2,
        "no local dig is in progress, so this must be exactly the two \
         other-player overlays reaching pixels — not one, not zero"
    );
    assert!(
        targets
            .iter()
            .any(|t| t.block == [10, 64, 20] && t.stage == 3),
        "entity 301's overlay must reach Sim::crack_targets: {targets:?}"
    );
    assert!(
        targets
            .iter()
            .any(|t| t.block == [-5, 70, 8] && t.stage == 7),
        "entity 402's overlay must reach Sim::crack_targets: {targets:?}"
    );
}

/// The negative control for the pair above: with no connection there is no
/// session `World` to read, so both projections must be empty rather than
/// falling back to some shell-local copy — which is the assertion that
/// `Sim` really holds neither aggregate any more.
#[test]
fn without_a_connection_the_shell_has_no_session_state_of_its_own() {
    let sim = Sim::new(test_config());
    assert!(sim.tab_list_view().is_empty());
    assert!(sim.sidebar().is_none());
    assert!(sim.boss_bars().is_empty());
}

/// The scoreboard twin of the tab-list read-through above.
#[test]
fn sidebar_rows_read_the_clients_one_folded_scoreboard() {
    use lodestone_model::event::{DisplaySlot, ObjectiveMode, ObjectiveRenderType};
    use lodestone_model::{ClientEvent, Text};

    let (net, _actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);

    for event in [
        ClientEvent::ObjectiveUpdate {
            name: "kills".into(),
            mode: ObjectiveMode::Add,
            display_name: Some(Text::literal("Kills")),
            render_type: Some(ObjectiveRenderType::Integer),
            number_format: None,
        },
        ClientEvent::DisplayObjective {
            slot: DisplaySlot::Sidebar,
            objective: Some("kills".into()),
        },
        ClientEvent::ScoreUpdate {
            holder: "Alice".into(),
            objective: "kills".into(),
            value: 7,
            display: Some(Text::literal("Alice the Brave")),
            number_format: None,
        },
        ClientEvent::ScoreUpdate {
            holder: "Bob".into(),
            objective: "kills".into(),
            value: 3,
            display: None,
            number_format: None,
        },
    ] {
        sim.net().expect("net attached").ingest_session_event(event);
    }

    let sidebar = sim.sidebar().expect("sidebar objective should be visible");
    assert_eq!(crate::overlay::spans_text(&sidebar.title), "Kills");
    let rows: Vec<(String, String)> = sidebar
        .lines
        .iter()
        .map(|line| {
            (
                crate::overlay::spans_text(&line.label),
                crate::overlay::spans_text(&line.score),
            )
        })
        .collect();
    assert_eq!(
        rows,
        vec![
            ("Alice the Brave".to_string(), "7".to_string()),
            ("Bob".to_string(), "3".to_string())
        ],
        "sidebar rows must come from the client's folded Scoreboard state"
    );
}

// -----------------------------------------------------------------------
// Local placement prediction (issue #381)
// -----------------------------------------------------------------------

/// The state ids below are transcribed from
/// `.cache/mc/26.2/generated/reports/blocks.json` — Mojang's own generator
/// output, data source #1 — and **not** from this code's own resolution, so
/// they are an external oracle rather than a round trip through
/// `state_for_placement`. Each is the state whose properties vanilla's
/// `getStateForPlacement` produces for that block.
///
/// A 26.2 data bump shifts every id, and this failing is the point: it says
/// the census moved under the resolver, which is exactly when the property
/// rules deserve a re-read.
mod placement_oracle {
    /// `chest[type=single,facing=north,waterlogged=false]` — the registered
    /// default, and what `ChestBlock.getStateForPlacement` yields facing north.
    pub const CHEST_NORTH: u32 = 3988;
    /// `chest[type=single,facing=south,waterlogged=false]`.
    pub const CHEST_SOUTH: u32 = 3994;
    /// `oak_slab[type=bottom,waterlogged=false]`.
    pub const OAK_SLAB_BOTTOM: u32 = 13333;
    /// `oak_slab[type=top,waterlogged=false]`.
    pub const OAK_SLAB_TOP: u32 = 13331;
    /// `oak_log[axis=y]`.
    pub const OAK_LOG_Y: u32 = 137;
    /// `stone` — the one propertyless case.
    pub const STONE: u32 = 1;
}

/// The production seam, not a re-spelling of it — [`predicted_placement_state`]
/// is what `use_item_live` resolves through and what the pixel gate drives.
fn resolve(block: &str, placed: PlacedState) -> Option<u32> {
    predicted_placement_state(block, &placed)
}

/// The resolver must hit the block's own placement state exactly — including
/// the two properties the census cannot default (`waterlogged`, a chest's
/// `type`), because "lowest state id for this block" gets **both** wrong:
/// `BooleanProperty`'s value order is `{true, false}`, so the lowest chest id
/// is a *waterlogged* chest and the lowest slab id is a *top* slab.
#[test]
fn placement_states_resolve_to_the_jar_oracle() {
    assert_eq!(
        resolve(
            "minecraft:chest",
            PlacedState {
                facing: Some(BlockFace::North),
                ..PlacedState::default()
            }
        ),
        Some(placement_oracle::CHEST_NORTH),
        "a chest facing north must resolve to type=single, waterlogged=false"
    );
    assert_eq!(
        resolve(
            "minecraft:chest",
            PlacedState {
                facing: Some(BlockFace::South),
                ..PlacedState::default()
            }
        ),
        Some(placement_oracle::CHEST_SOUTH),
        "facing must actually reach the resolved state, not be dropped"
    );
    assert_eq!(
        resolve(
            "minecraft:oak_slab",
            PlacedState {
                half: Some(Half::Bottom),
                ..PlacedState::default()
            }
        ),
        Some(placement_oracle::OAK_SLAB_BOTTOM)
    );
    assert_eq!(
        resolve(
            "minecraft:oak_slab",
            PlacedState {
                half: Some(Half::Top),
                ..PlacedState::default()
            }
        ),
        Some(placement_oracle::OAK_SLAB_TOP),
        "the slab's half must select type=top, not the block's default"
    );
    assert_eq!(
        resolve(
            "minecraft:oak_log",
            PlacedState {
                axis: Some(Axis::Y),
                ..PlacedState::default()
            }
        ),
        Some(placement_oracle::OAK_LOG_Y)
    );
    assert_eq!(
        resolve("minecraft:stone", PlacedState::default()),
        Some(placement_oracle::STONE)
    );
}

/// The declines, and why each one is a decline rather than a guess. Without
/// these the resolver would look "complete" while writing states the server
/// immediately contradicts.
#[test]
fn unclassifiable_placements_decline_rather_than_guess() {
    for (block, why) in [
        // A 4-way `facing` the census cannot tell from a chest's, and vanilla
        // points it *toward* the player.
        ("minecraft:ladder", "FacingHorizontal is not classified"),
        // Two cells, a hinge and an upper/lower half.
        ("minecraft:oak_door", "multi-block placement"),
        // `shape` comes from the neighbouring rails.
        ("minecraft:rail", "neighbour-derived shape"),
        // `persistent` is set *true* for a player-placed leaf, so the
        // registered default would be actively wrong.
        ("minecraft:oak_leaves", "persistent is placement-derived"),
        // Not in the horizontal-facing list — and its `mode` has no
        // consistent default across the blocks that carry one either.
        ("minecraft:comparator", "unclassified 4-way facing"),
        // Not a block at all.
        ("minecraft:diamond_sword", "not a block item"),
    ] {
        assert_eq!(
            resolve(block, PlacedState::default()),
            None,
            "{block} must decline ({why}); predicting it would write a state the \
             server contradicts one round trip later"
        );
    }
}

/// A right-click on a solid cell with a chest in hand must decide `Place` into
/// the adjacent air cell, and a right-click on the chest itself must decide
/// `Interact` — the branch that keeps the prediction from dropping a ghost
/// chest beside the one you meant to open.
#[test]
fn placement_facts_drive_the_place_versus_interact_decision() {
    let clicked = BlockPos::new(4, 64, 9);
    let target = BlockPos::new(4, 65, 9);
    let solid_ground = PlacementFacts {
        clicked,
        target,
        clicked_replaceable: false,
        clicked_interactable: false,
        target_replaceable: true,
        target_obstructed: false,
    };
    let chest = PlacementFacts {
        clicked_interactable: true,
        ..solid_ground
    };
    let ctx = UseOnContext {
        hand: Hand::Main,
        clicked,
        face: BlockFace::Up,
        cursor: Vec3f::new(0.5, 1.0, 0.5),
        inside_block: false,
        rotation: Rotation::new(0.0, 0.0),
        sneaking: false,
        has_item_in_hand: true,
        placing: Some("minecraft:chest".parse().expect("identifier")),
        orientation: OrientationKind::FacingHorizontalOpposite,
    };

    let mut placement = Placement::new();
    let decision = placement.use_on(&ctx, &solid_ground);
    let UseOnDecision::Place { prediction, .. } = &decision else {
        panic!("a chest onto solid ground must place, got {decision:?}");
    };
    assert_eq!(prediction.pos, target, "the placement goes into the air cell");
    assert_eq!(
        state_for_placement(
            "minecraft:chest",
            &block_states_of("minecraft:chest").expect("chest is a block"),
            OrientationKind::FacingHorizontalOpposite,
            &prediction.state,
        ),
        // Yaw 0 faces +Z (south), and a chest faces *away* from the player.
        Some(placement_oracle::CHEST_NORTH),
        "the prediction's geometry must survive into the resolved state"
    );
    assert_eq!(placement.pending().len(), 1);

    let mut placement = Placement::new();
    assert!(
        matches!(
            placement.use_on(&ctx, &chest),
            UseOnDecision::Interact { .. }
        ),
        "clicking an interactable block must not predict a placement"
    );
    assert!(
        placement.pending().is_empty(),
        "an interaction records nothing to reconcile"
    );

    // Obstruction and an unloaded/solid target both decline, which is what
    // keeps a prediction from landing inside the player or in a cell we cannot
    // see.
    for facts in [
        PlacementFacts {
            target_obstructed: true,
            ..solid_ground
        },
        PlacementFacts {
            target_replaceable: false,
            ..solid_ground
        },
    ] {
        assert!(
            matches!(
                Placement::new().use_on(&ctx, &facts),
                UseOnDecision::Nothing { .. }
            ),
            "an illegal target must not predict: {facts:?}"
        );
    }
}

/// A container is interactable through the block-entity census, not through
/// the name list — that is what makes the list's gaps cost a round trip
/// instead of a wrong right-click on a chest.
#[test]
fn every_container_is_interactable_and_plain_terrain_is_not() {
    let state = |name: &str| {
        (0..lodestone_data::block_states::STATE_COUNT)
            .find(|&id| lodestone_data::block_states::block_name(id) == Some(name))
            .unwrap_or_else(|| panic!("{name} is not in the 26.2 census"))
    };
    for name in [
        "minecraft:chest",
        "minecraft:barrel",
        "minecraft:furnace",
        "minecraft:hopper",
        "minecraft:oak_door",
        "minecraft:crafting_table",
    ] {
        assert!(
            is_interactable_state(state(name)),
            "{name} must suppress the placement prediction"
        );
    }
    for name in ["minecraft:stone", "minecraft:dirt", "minecraft:oak_planks"] {
        assert!(
            !is_interactable_state(state(name)),
            "{name} must not suppress it — this is the 95% case"
        );
    }
    assert!(is_air_state(state("minecraft:air")));
    assert!(!is_air_state(state("minecraft:water")));
}

#[test]
fn hotbar_selection_updates_and_echoes_to_the_server() {
    use lodestone_client::ClientAction;
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);

    // Vanilla default is slot 0, and selecting it again is a no-op (no
    // redundant packet).
    assert_eq!(sim.selected_slot(), 0);
    sim.select_slot(0);

    // A direct selection moves and echoes exactly one SetCarriedItem.
    sim.select_slot(3);
    assert_eq!(sim.selected_slot(), 3);

    // Out-of-range is ignored (no 10th slot), leaving selection and the
    // wire untouched.
    sim.select_slot(9);
    assert_eq!(sim.selected_slot(), 3);

    // Scroll wraps at both ends: +1 from 3 → 4, and from 8 → 0.
    sim.cycle_slot(1);
    assert_eq!(sim.selected_slot(), 4);
    sim.select_slot(8);
    sim.cycle_slot(1);
    assert_eq!(
        sim.selected_slot(),
        0,
        "scroll past the last slot wraps to 0"
    );
    sim.cycle_slot(-1);
    assert_eq!(
        sim.selected_slot(),
        8,
        "scroll before the first slot wraps to 8"
    );

    let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
    // Every *change* echoes SetCarriedItem; the no-op select_slot(0) and the
    // rejected select_slot(9) send nothing, so the wire shows only the moves.
    assert_eq!(
        sent,
        vec![
            ClientAction::SetCarriedItem { slot: 3 },
            ClientAction::SetCarriedItem { slot: 4 },
            ClientAction::SetCarriedItem { slot: 8 },
            ClientAction::SetCarriedItem { slot: 0 },
            ClientAction::SetCarriedItem { slot: 8 },
        ],
        "only real selection changes reach the outbound action seam"
    );
}

#[test]
fn camera_interpolates_between_ticks() {
    // Force a known prev/current split and a half-way alpha, then check the
    // camera eye sits between the two feet positions.
    let mut sim = Sim::new(test_config());
    sim.set_prev_position(Vec3d::new(0.0, 64.0, 0.0));
    sim.player_mut(|p| p.position = Vec3d::new(10.0, 64.0, 0.0));
    sim.clock_mut(|c| c.interp_alpha = 0.5);
    let cam = sim.camera(1.0);
    assert!(
        (cam.position.x - 5.0).abs() < 1e-4,
        "expected midpoint x=5, got {}",
        cam.position.x
    );
}

#[test]
fn frames_per_tick_tracks_ratio() {
    let mut sim = Sim::new(test_config());
    // Two frames of one full tick each ⇒ 2 frames / 2 ticks = 1.0.
    sim.step(1.0 / 20.0);
    sim.step(1.0 / 20.0);
    assert!((sim.frames_per_tick() - 1.0).abs() < 1e-6);
    // A frame with no accumulated tick still counts as a frame, so the
    // frames-per-tick ratio rises above 1.
    sim.step(0.0);
    assert!(sim.frames_per_tick() > 1.0, "extra frame raises the ratio");
}

#[test]
fn sprint_moves_faster_than_walk_via_attribute_seam() {
    // Walk forward for a second, then sprint the same time from the same
    // spot; sprinting must cover more ground. This drives the physics
    // `with_movement_speed` seam from a real caller.
    //
    // The local world is now real vanilla terrain (`lodestone-worldgen`),
    // so spawn sits on a slope and walking north walls the player out after
    // ~0.2 blocks — a wall, not the speed seam, would otherwise decide the
    // result. Flatten a private corridor along the walking line so what we
    // measure is physics speed and nothing else.
    fn distance(sprint: bool) -> f64 {
        let mut sim = Sim::new(test_config());
        // Player spawns at (0.5, feet, 0.5) facing north (-Z, yaw 180).
        // Lay a solid floor and clear head-room along -Z so the walk is
        // unobstructed regardless of the generated surface.
        let feet_y = sim.player().position.y.floor() as i32;
        for dz in -25..=1 {
            for dx in -1..=1 {
                sim.set_block_world([dx, feet_y - 1, dz], id::STONE);
                sim.set_block_world([dx, feet_y, dz], id::AIR);
                sim.set_block_world([dx, feet_y + 1, dz], id::AIR);
                sim.set_block_world([dx, feet_y + 2, dz], id::AIR);
            }
        }
        // Settle on the fresh floor first.
        for _ in 0..20 {
            sim.step(1.0 / 20.0);
        }
        let start = sim.player().position;
        sim.input_mut(|i| i.set(lodestone_controller::Action::Forward, true));
        sim.input_mut(|i| i.set(lodestone_controller::Action::Sprint, sprint));
        for _ in 0..20 {
            sim.step(1.0 / 20.0);
        }
        let d = sim.player().position.subtract(start);
        (d.x * d.x + d.z * d.z).sqrt()
    }
    let walk = distance(false);
    let sprint = distance(true);
    assert!(
        sprint > walk * 1.1,
        "sprint ({sprint:.3}) should clearly exceed walk ({walk:.3})"
    );
}

/// Swimming has to reach the *player*, not just exist in the physics crate.
/// Flood a pool in the demo world (whose palette has a real water block), hold
/// sprint + forward, and check the pose actually flips: `swimming` set, the eye
/// dropped to `Pose.SWIMMING`'s `0.4`, and the camera moved with it.
///
/// The first phase is the control: standing in exactly the same water without
/// sprinting must **not** swim, so the assertions below are about sprinting
/// while submerged and not about "being wet".
#[test]
fn sprinting_underwater_enters_the_swim_pose_and_drops_the_camera() {
    let mut sim = Sim::new(test_config());
    let feet_y = sim.player().position.y.floor() as i32;
    // A private pool: stone floor, water from the feet to well over the eye,
    // wide enough that a second of swimming (~1 block) stays inside it. Filling
    // the column with water is also what flattens the generated slope the player
    // spawns on — see `sprint_moves_faster_than_walk_via_attribute_seam`.
    for dz in -5..=5 {
        for dx in -5..=5 {
            sim.set_block_world([dx, feet_y - 1, dz], id::STONE);
            for dy in 0..=4 {
                sim.set_block_world([dx, feet_y + dy, dz], id::WATER);
            }
        }
    }

    for _ in 0..10 {
        sim.step(1.0 / 20.0);
    }
    assert!(
        sim.fluid_state().under_water(),
        "the pool must actually submerge the eye, or this gate proves nothing"
    );
    assert!(
        !sim.player().swimming,
        "control: submerged but not sprinting is not swimming"
    );
    assert_eq!(
        sim.player().eye_height,
        lodestone_physics::player::DEFAULT_EYE_HEIGHT
    );

    sim.input_mut(|i| i.set(lodestone_controller::Action::Forward, true));
    sim.input_mut(|i| i.set(lodestone_controller::Action::Sprint, true));
    // Step until the pose flips, so the tick the change lands on is known.
    let mut ticks_to_swim = None;
    for tick in 0..10 {
        sim.step(1.0 / 20.0);
        if sim.player().swimming {
            ticks_to_swim = Some(tick);
            break;
        }
    }
    assert!(
        ticks_to_swim.is_some(),
        "sprinting while submerged must enter the swim pose"
    );
    assert_eq!(
        sim.player().eye_height,
        SWIMMING_EYE_HEIGHT,
        "the shell owns the pose eye height; physics only reads it"
    );

    // Helper: pin the *position* interpolation so a camera assertion is about
    // the eye height, not about where between two ticks the feet are.
    //
    // `alpha` is deliberately a parameter, because it selects **which** of the
    // smoother's two values you see: `lerp(0.0)` is the *previous* tick's eased
    // eye height and `lerp(1.0)` is this tick's. That is the whole point of the
    // `O` twin, and reading at `0.0` right after a pose flip therefore shows the
    // pre-flip height — correct, and not what a mid-ease assertion wants.
    let camera_offset = |sim: &mut Sim, alpha: f32| {
        let settled = sim.player().position;
        sim.set_prev_position(settled);
        sim.clock_mut(|c| c.interp_alpha = alpha);
        sim.camera(1.0).position.y - sim.player().position.y as f32
    };

    // **The camera must NOT have snapped.** `Camera.tick()` eases its own eye
    // height toward the entity's — `eyeHeight += (target - eyeHeight) * 0.5F` —
    // so one tick after the pose flips it is still most of the way up at the
    // standing height. This is the assertion that proves `Sim::camera` reads
    // `eye_height_smoother` and not the raw pose value; before that existed the
    // view jerked 1.22 blocks in a single frame on entering water.
    let standing = lodestone_physics::player::DEFAULT_EYE_HEIGHT;
    let after_flip = camera_offset(&mut sim, 1.0);
    assert!(
        after_flip > SWIMMING_EYE_HEIGHT + 0.1 && after_flip < standing,
        "camera should be mid-ease between {SWIMMING_EYE_HEIGHT} and {standing} \
         one tick after the pose flip, got {after_flip}"
    );

    // …and it must converge. Each tick halves the remaining gap, so the
    // original `1e-4` tolerance needs ~14 ticks from a 1.22-block step; 24 is
    // comfortably past it without being sensitive to the exact rate.
    for _ in 0..24 {
        sim.step(1.0 / 20.0);
    }
    let settled_offset = camera_offset(&mut sim, 1.0);
    assert!(
        (settled_offset - SWIMMING_EYE_HEIGHT).abs() < 1e-4,
        "swim camera should settle {SWIMMING_EYE_HEIGHT} above the feet: got \
         {settled_offset}"
    );
}

/// Sneak is how you swim *downward* (`goDownInWater`), so the land-side
/// "sneaking cancels sprint" gate must not apply while submerged — otherwise
/// holding shift underwater stops the swim dead. Control: the same shift+sprint
/// on dry land still cancels sprint.
///
/// The *rule* now lives in `lodestone_controller::swim_adjusted_intent` and
/// is tested there against the pure function, and in that crate's
/// `the_intent_system_reads_submersion_for_the_swim_exception` against the
/// system. This one is deliberately kept as well, and asserts something
/// neither of those can: that a `Sim::step` — the real driver, with the real
/// `RawInput` resource and the real `Submersion` component — reaches the
/// intent the physics set will read. Without it, `Sim` could stop feeding the
/// ECS entirely and both of the controller's tests would still pass.
#[test]
fn sneak_cancels_sprint_on_land_but_not_under_water() {
    let mut sim = Sim::new(test_config());
    sim.input_mut(|i| i.set(lodestone_controller::Action::Forward, true));
    sim.input_mut(|i| i.set(lodestone_controller::Action::Sprint, true));
    sim.input_mut(|i| i.set(lodestone_controller::Action::Sneak, true));

    sim.step(lodestone_ecs::TICK_PERIOD);
    assert!(
        !sim.movement_intent().sprint,
        "control: on land, sneaking still vetoes sprint"
    );

    sim.set_fluid_state(FluidState {
        water_height: 2.0,
        eye_in_water: true,
        ..FluidState::NONE
    });
    sim.step(lodestone_ecs::TICK_PERIOD);
    let intent = sim.movement_intent();
    assert!(
        intent.sprint,
        "submerged, shift must not cancel a swim-sprint"
    );
    assert!(
        intent.sneak,
        "…and shift itself must survive, or the sink impulse is lost"
    );
}

/// The server derives the swimming pose itself, from `isSprinting()` — and it
/// only learns that from `ServerboundPlayerCommandPacket`, never from the input
/// packet's `sprint` bit. So the sprint *edge* has to reach the wire as a
/// `PlayerCommand`, exactly once per change.
#[test]
fn sprint_edges_reach_the_wire_as_player_commands() {
    use crate::net::NetUpdate;
    use lodestone_ecs::ecs::system::RunSystemOnce;
    use lodestone_model::PlayerCommand;

    let (net, actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    // Both halves of one login packet, because the packet carries the entity id
    // and `send_sprint_command` will not send without one. `NetUpdate::LoggedIn`
    // drives the phase (and therefore `Egress::in_world`); `ClientEvent::Login`
    // is what folds `ServerEntityId`, on the net thread, since the vitals
    // collapse deleted `poll_net`'s duplicate `set_server_entity_id` write.
    // Feeding only the `NetUpdate` left the id `None`, which made the whole
    // test a *precondition*-species vacuity: the query hit
    // `let Some(entity_id) = … else { continue }` every time, so the two
    // "no packet" assertions below held for a reason that had nothing to do
    // with edge-triggering.
    ingest(&mut sim, login_event(7));
    feed.send(NetUpdate::LoggedIn { entity_id: 7 }).unwrap();
    sim.poll_net();
    assert_eq!(
        sim.server_entity_id(),
        Some(7),
        "setup: without the folded id no sprint command can be sent at all, \
         and every assertion below passes vacuously"
    );
    while actions.try_recv().is_ok() {}

    // `EndClientTick` is filtered out, not asserted on: `drain_action_queue`
    // appends vanilla's tick tail on every call once `Egress::in_world` holds
    // (see its own doc), and `sprint_once` below sets `in_world`. This test is
    // about the sprint *edge*, and the tail is exactly as much noise here as the
    // per-tick movement packet the comment below explains away — that packet is
    // avoided by running one system rather than the schedule, which cannot work
    // for something the drain itself adds.
    // `connected_sim_emits_one_move_per_physics_tick` is where the tail is
    // asserted, so filtering here does not hide it from every gate.
    let drain = |actions: &std::sync::mpsc::Receiver<ClientAction>| -> Vec<ClientAction> {
        std::iter::from_fn(|| actions.try_recv().ok())
            .filter(|a| !matches!(a, ClientAction::EndClientTick))
            .collect()
    };

    // Since Stage 5 the sprint edge is `crate::interact::send_sprint_command`,
    // a `TickSet::Send` system. Run *that system* and then the driver's own
    // queue drain, rather than the whole `GameTick` schedule: the schedule also
    // emits the per-tick movement packet, which would swamp the
    // "no edge, no packet" assertions below. Deliberately **not** an assertion
    // on `ActionQueue` — the queue is not the wire, and this test's whole point
    // is that the command reaches the socket.
    //
    // `Egress` has to be set by hand for the same reason the old direct call
    // needed no gate: the demo fixture has no vanilla atlas, so `is_live()` is
    // false and `step` would derive `live: false`. The gate moved from the call
    // site into the system, which is where `send_player_input` already keeps
    // its identical one.
    let sprint_once = |sim: &mut Sim| {
        {
            let mut world = sim.ecs().write();
            world.insert_resource(Egress {
                in_world: true,
                live: true,
            });
            world
                .run_system_once(crate::interact::send_sprint_command)
                .expect("send_sprint_command runs");
        }
        sim.drain_action_queue();
    };

    // Not sprinting and never was: no packet at all (vanilla's `wasSprinting`
    // starts false).
    sprint_once(&mut sim);
    assert!(
        drain(&actions).is_empty(),
        "no sprint edge, no sprint packet"
    );

    sim.player_mut(|p| p.sprinting = true);
    sprint_once(&mut sim);
    assert_eq!(
        drain(&actions),
        vec![ClientAction::PlayerCommand {
            entity_id: 7,
            command: PlayerCommand::StartSprinting,
        }]
    );

    // Edge-triggered: holding sprint must not spam the server every tick.
    sprint_once(&mut sim);
    sprint_once(&mut sim);
    assert!(drain(&actions).is_empty(), "sprint is edge-triggered");

    sim.player_mut(|p| p.sprinting = false);
    sprint_once(&mut sim);
    assert_eq!(
        drain(&actions),
        vec![ClientAction::PlayerCommand {
            entity_id: 7,
            command: PlayerCommand::StopSprinting,
        }]
    );
}

#[test]
fn breaking_the_target_clears_it_and_schedules_a_remesh() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    // Aim straight down at the block under the player's feet.
    let feet = sim.player().position;
    sim.set_target(Some(crate::raycast::RayHit::face_center(
        [
            feet.x.floor() as i32,
            feet.y.floor() as i32 - 1,
            feet.z.floor() as i32,
        ],
        [0, 1, 0],
    )));
    assert!(sim.break_block(), "should break the solid block");
    assert!(sim.target().is_none(), "target cleared after break");
    assert!(sim.pending_meshes() > 0, "a remesh was scheduled");
}

// -----------------------------------------------------------------------
// Arm swing: the producer -> consumer wiring
// -----------------------------------------------------------------------
//
// `lodestone_entity::pose` proves the swing clock ticks and
// `lodestone_render::entity` proves the arm matrix moves. Neither can prove
// that anything in this shell ever *starts* a swing — the failure this repo
// has hit nine times. These gates assert the seam: a swing produced the way
// the real producers produce one reaches `hand_swing_progress` (which
// `app.rs` hands `RenderState::set_hand_swing_source`) and
// `third_person_body_state` (which feeds the self-avatar's
// `setupAttackAnimation`).

/// Aim straight down at the block under the player's feet, like
/// `breaking_the_target_clears_it_and_schedules_a_remesh`.
fn aim_at_the_floor(sim: &mut Sim) {
    let feet = sim.player().position;
    sim.set_target(Some(crate::raycast::RayHit::face_center(
        [
            feet.x.floor() as i32,
            feet.y.floor() as i32 - 1,
            feet.z.floor() as i32,
        ],
        [0, 1, 0],
    )));
}

/// Run whole ticks and report the largest swing progress seen.
fn peak_swing_over(sim: &mut Sim, ticks: u32) -> f32 {
    let mut peak = 0.0f32;
    for _ in 0..ticks {
        sim.step(1.0 / 20.0);
        peak = peak.max(sim.hand_swing_progress());
    }
    peak
}

#[test]
fn a_queued_main_hand_swing_reaches_the_arm_pose() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();

    // The negative control first, and it is the one that matters: with no
    // swing produced, the arm must sit at exact rest for the whole window.
    // Without this, "progress > 0" is also satisfied by a clock that free-runs
    // off frame time — which is the specific bug `entities.rs` documents
    // finding in the limb-swing code.
    let idle_peak = peak_swing_over(&mut sim, 20);
    assert_eq!(
        idle_peak, 0.0,
        "an idle player's arm must be at rest, but progress peaked at {idle_peak}"
    );

    // Now produce a swing exactly the way `lodestone_game::mining` does — it
    // pushes `SwingArm { Main }` onto `ActionQueue`, and `drive_mining`
    // forwards that queue verbatim. `mining.rs`'s own tests already pin that
    // it emits one; this pins that the shell animates it.
    sim.write(|w| {
        w.resource_mut::<ActionQueue>()
            .0
            .push(ClientAction::SwingArm { hand: Hand::Main });
    });
    let peak = peak_swing_over(&mut sim, 10);
    assert!(
        peak > 0.4,
        "a queued main-hand swing must drive the arm pose, but progress \
         peaked at only {peak} — `drain_action_queue` is not calling `swing_hand`, \
         or `hand_swing_progress` is not reading the clock it sets"
    );

    // And it ends: the swing is 6 ticks, so well after that the arm is rested
    // again. A swing that never finishes reads as a permanently cocked arm.
    let after = peak_swing_over(&mut sim, 30);
    assert_eq!(
        after, 0.0,
        "the swing must return to rest, but progress still peaked at {after}"
    );
}

/// An **off-hand** swing must not drive the arm. `drain_action_queue` matches
/// on `Hand::Main` specifically; without this control that match is untested
/// and a `SwingArm { .. }` wildcard would swing the right arm for a left-hand
/// action.
#[test]
fn an_off_hand_swing_does_not_drive_the_main_arm() {
    let mut sim = Sim::new(test_config());
    sim.write(|w| {
        w.resource_mut::<ActionQueue>()
            .0
            .push(ClientAction::SwingArm { hand: Hand::Off });
    });
    let peak = peak_swing_over(&mut sim, 10);
    assert_eq!(
        peak, 0.0,
        "an off-hand swing must leave the main arm at rest, got {peak}"
    );
}

/// The demo world has no action queue to piggy-back on, so `break_block` and
/// `place_block` start the swing themselves. This is the only world a headless
/// scene can exercise, so if it did not swing, no offline gate ever could.
#[test]
fn a_demo_world_break_swings_the_arm() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    aim_at_the_floor(&mut sim);
    // Load-bearing: if the break did not happen this test would pass
    // vacuously by asserting nothing about a swing that was never produced.
    assert!(sim.break_block(), "the demo block should have broken");
    let peak = peak_swing_over(&mut sim, 10);
    assert!(
        peak > 0.4,
        "a demo-world break must swing the arm, progress peaked at {peak}"
    );
}

/// Issue #72: a demo-world left-click with **nothing** targeted must still
/// swing — vanilla's `Minecraft.startAttack` reaches `player.swing(...)`
/// unconditionally after the switch, `MISS` included. Before this fix
/// `Sim::begin_attack` called `break_block()` alone on the demo world,
/// which swings only on a *successful* break and produces nothing when
/// there is no target.
#[test]
fn begin_attack_swings_the_arm_on_a_demo_world_miss() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    assert!(
        sim.target().is_none(),
        "test setup: nothing should be targeted yet"
    );
    sim.begin_attack();
    let peak = peak_swing_over(&mut sim, 10);
    assert!(
        peak > 0.4,
        "a miss must still swing the arm (issue #72), progress peaked at {peak}"
    );
}

/// Regression companion to the miss test above: routing `begin_attack`
/// through the new demo/live split must not break the existing
/// successful-break path.
#[test]
fn begin_attack_still_breaks_a_targeted_demo_block() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    aim_at_the_floor(&mut sim);
    sim.begin_attack();
    assert!(
        sim.target().is_none(),
        "a successful break clears the target, as `break_block` always did"
    );
    let peak = peak_swing_over(&mut sim, 10);
    assert!(
        peak > 0.4,
        "breaking a targeted demo block must still swing, progress peaked at {peak}"
    );
}

/// Issue #72's live-path miss case: no block, no entity, and the arm still
/// swings. Exercises `begin_attack_live` directly (no net connection is
/// needed — the swing is client-side and does not require one, matching
/// every other swing site's contract).
#[test]
fn begin_attack_live_swings_on_a_miss() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    assert!(sim.target().is_none());
    assert!(sim.entity_target().is_none());
    sim.begin_attack_live();
    let peak = peak_swing_over(&mut sim, 10);
    assert!(
        peak > 0.4,
        "a live miss must still swing the arm, progress peaked at {peak}"
    );
}

/// The `BLOCK`-only case: with no entity targeted, `begin_attack_live`
/// must still arm the hold-to-mine loop exactly as it did before this
/// change (the pre-existing, unmodified behaviour this fix must not
/// regress).
#[test]
fn begin_attack_live_arms_mining_when_only_a_block_is_targeted() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    aim_at_the_floor(&mut sim);
    sim.begin_attack_live();
    let attacking = sim.read(|w| w.resource::<Attacking>().0);
    assert!(
        attacking,
        "a block-only target must still arm the hold-to-mine loop"
    );
}

/// `case ENTITY` takes priority over `case BLOCK`: with both an entity and
/// a block targeted, attacking the entity must swing the arm and must
/// **not** also arm the hold-to-mine loop — vanilla's `hitResult` is one
/// value, never both at once.
#[test]
fn begin_attack_live_prefers_an_entity_target_over_mining() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    aim_at_the_floor(&mut sim);
    sim.write(|w| w.resource_mut::<EntityRayTarget>().0 = Some(42));
    sim.begin_attack_live();
    let peak = peak_swing_over(&mut sim, 10);
    assert!(
        peak > 0.4,
        "attacking an entity target must swing the arm, progress peaked at {peak}"
    );
    let attacking = sim.read(|w| w.resource::<Attacking>().0);
    assert!(
        !attacking,
        "an entity attack must not also arm the hold-to-mine loop"
    );
}

/// A dead local player must not attack — mirrors `use_item_live`'s own
/// `is_dead()` guard, and vanilla drops input entirely on the death
/// screen.
#[test]
fn begin_attack_live_does_nothing_while_dead() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    let local = sim.local_player();
    sim.write(|w| {
        w.entity_mut(local).insert(Dead);
        w.resource_mut::<EntityRayTarget>().0 = Some(42);
    });
    sim.begin_attack_live();
    let peak = peak_swing_over(&mut sim, 10);
    assert_eq!(peak, 0.0, "a dead player must not swing on attack");
}

/// Puts `item` into the local player's main-hand hotbar slot (native
/// index 0, [`Sim::selected_slot`]'s default) via the same
/// [`lodestone_ecs::SessionMenus`] fold a real `ContainerSetSlot`
/// packet drives — the pattern
/// `closing_a_server_menu_clears_it_locally_without_waiting_for_the_server`
/// already established for writing menu state directly in a hermetic
/// test.
fn give_main_hand_item(sim: &mut Sim, item: &str) {
    let local = sim.local;
    sim.write(|w| {
        if let Some(mut menus) = w.get_mut::<lodestone_ecs::SessionMenus>(local) {
            menus.0.apply(&lodestone_model::ClientEvent::InventorySlotChanged {
                slot: 0,
                item: Some(lodestone_model::ItemStack::new(
                    item.parse().expect("valid item id"),
                    1,
                )),
            });
        }
    });
}

/// Finding 2 (combat scoping doc): before this fix, `use_item_live`
/// returned unconditionally after `interact_entity` whenever *any*
/// entity was targeted — hostile mobs included, the overwhelmingly
/// common combat case — so a bow or shield could never even start a use.
/// Vanilla's own `case ENTITY` (`Minecraft.java:1693-1708`) only returns
/// on a *successful* interact; anything else falls through to the
/// generic use-item call (`:1730`) that actually raises a shield or
/// draws a bow.
///
/// This is the control the scoping doc asked for: it must fail
/// (`ClientAction::UseItem` absent) against the pre-fix `use_item_live`,
/// which this test's own doc-comment history confirms was checked by
/// hand (see the report for the reverted/restored run).
#[test]
fn use_item_live_falls_through_to_generic_use_with_an_entity_targeted() {
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);
    give_main_hand_item(&mut sim, "minecraft:bow");
    sim.write(|w| w.resource_mut::<EntityRayTarget>().0 = Some(42));

    sim.use_item_live();

    let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
    assert!(
        matches!(
            sent.first(),
            Some(ClientAction::InteractEntity {
                entity_id: 42,
                ..
            })
        ),
        "the entity interact itself must still be sent first, got {sent:?}"
    );
    assert!(
        sent.iter()
            .any(|a| matches!(a, ClientAction::UseItem { hand: Hand::Main, .. })),
        "an entity target must fall through to the generic use-item send \
         (this is what raises a shield or draws a bow at a mob) — got {sent:?}"
    );
}

/// Finding 2's other half: with **no** target at all — open air, or a mob
/// just past block reach with nothing behind it — `use_item_live` used to
/// `return` with nothing sent. Vanilla's own `hitResult == null` path
/// skips the block/entity switch entirely and still reaches the
/// unconditional fallback (`Minecraft.java:1681,1691,1730`).
#[test]
fn use_item_live_sends_generic_use_with_no_target_at_all() {
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);
    give_main_hand_item(&mut sim, "minecraft:bow");
    assert!(sim.target().is_none(), "precondition: no block targeted");
    assert!(sim.entity_target().is_none(), "precondition: no entity targeted");

    sim.use_item_live();

    let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
    assert!(
        sent.iter()
            .any(|a| matches!(a, ClientAction::UseItem { hand: Hand::Main, .. })),
        "a miss (no block, no entity) must still send the generic use-item action \
         — got {sent:?}"
    );
}

/// Negative control for both tests above: an **empty** main hand must
/// send nothing generic to use, matching vanilla's own
/// `!heldItem.isEmpty()` guard at the same call site
/// (`Minecraft.java:1730`). Without this, "always send `UseItem`"
/// would satisfy the two tests above vacuously.
#[test]
fn use_item_generic_sends_nothing_with_an_empty_main_hand() {
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);
    assert!(sim.target().is_none());
    assert!(sim.entity_target().is_none());

    sim.use_item_live();

    let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
    assert!(
        sent.is_empty(),
        "an empty main hand has nothing to use and must send nothing, got {sent:?}"
    );
}

/// Finding 1: [`Sim::end_use_live`] must send `ReleaseUseItem` when a use
/// was actually in progress — the packet that was a serverbound island
/// (encoded by all four protocol adapters, zero producers anywhere in
/// this shell). Bow, crossbow and shield are all `useOnRelease() ==
/// true` (`LivingEntity.java:3471-3475,3602-3616`) and cannot complete a
/// use without it.
#[test]
fn end_use_live_sends_release_use_item_after_a_use_press() {
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);
    give_main_hand_item(&mut sim, "minecraft:bow");
    assert!(sim.target().is_none());
    assert!(sim.entity_target().is_none());

    // The press: arms `UsingItem` (and, incidentally, sends the draw).
    sim.use_item_live();
    let _ = std::iter::from_fn(|| actions.try_recv().ok()).count();

    sim.end_use_live();
    let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
    assert_eq!(
        sent,
        vec![ClientAction::ReleaseUseItem],
        "releasing after a press must send exactly one ReleaseUseItem, got {sent:?}"
    );
}

/// Negative control: releasing with **no** prior press must send
/// nothing — proving `end_use_live` is actually gated on [`UsingItem`]
/// and not just "always send on release," which would pass the test
/// above vacuously.
#[test]
fn end_use_live_sends_nothing_with_no_prior_press() {
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);

    sim.end_use_live();

    let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
    assert!(
        sent.is_empty(),
        "a release with no press before it must send nothing, got {sent:?}"
    );

    // And a second release right after the first (both with no press) is
    // still silent — the flag does not get "stuck on".
    sim.end_use_live();
    let sent_again: Vec<ClientAction> =
        std::iter::from_fn(|| actions.try_recv().ok()).collect();
    assert!(sent_again.is_empty(), "still nothing on a repeated release");
}

/// Vanilla's `getCurrentItemAttackStrengthDelay`/`getAttackStrengthScale`
/// (`Player.java:1816-1828`): with no [`Attributes`] component at all (the
/// pre-login default `attribute_value` falls back to — see
/// `no_attributes_component_folds_to_the_registry_default` in
/// `lodestone_ecs::player`'s own tests for the identical fallback one
/// layer down), the unarmed `attack_speed` default of `4.0` gives a
/// 5-tick delay, so the scale ramps linearly from `0.0` to `1.0` over
/// exactly 5 real `GameTick`s (via [`Self::step`], not a hand-called
/// tick function — the same "reachable through the schedule" bar
/// `lodestone_ecs::player`'s island-class tests hold `PhysicsState`/
/// `AttackStrengthTicker` to) and clamps there rather than overshooting.
#[test]
fn attack_strength_scale_ramps_to_full_over_five_ticks_unarmed() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    assert_eq!(
        sim.attack_strength_scale(),
        0.0,
        "a fresh player must start at zero strength, matching Player's bare int field"
    );
    for expected_ticks in 1..=5u32 {
        sim.step(1.0 / 20.0);
        let want = (expected_ticks as f32 / 5.0).min(1.0);
        let got = sim.attack_strength_scale();
        assert!(
            (got - want).abs() < 1e-6,
            "after {expected_ticks} ticks expected scale {want}, got {got}"
        );
    }
    // One tick past the delay: still clamped at 1.0, not overshooting.
    sim.step(1.0 / 20.0);
    assert_eq!(sim.attack_strength_scale(), 1.0);
}

/// A weapon's `minecraft:attack_speed` modifier (a sword's net `1.6`, per
/// vanilla's item data) must change the delay, not just the unarmed
/// default — this is the whole reason the delay reads a live
/// server-fed [`Attributes`] snapshot instead of a hardcoded constant.
/// `20.0 / 1.6 = 12.5` ticks, so one tick in gives `1.0 / 12.5 = 0.08`.
#[test]
fn attack_strength_delay_follows_a_reported_attack_speed_attribute() {
    use std::str::FromStr;
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    let local = sim.local_player();
    let key = lodestone_model::Identifier::from_str("minecraft:attack_speed").unwrap();
    sim.write(|w| {
        w.entity_mut(local).insert(Attributes(vec![
            lodestone_model::EntityAttributeSnapshot {
                attribute: key,
                base: 1.6,
                modifiers: Vec::new(),
            },
        ]));
    });
    sim.step(1.0 / 20.0);
    let got = sim.attack_strength_scale();
    assert!(
        (got - 0.08).abs() < 1e-5,
        "a 1.6 attack-speed weapon should give scale 0.08 after one tick, got {got}"
    );
}

/// [`Sim::attack_entity`] must reset the ticker **immediately**, in the
/// same call, not on the next tick — vanilla's
/// `MultiPlayerGameMode.attack` calls `resetAttackStrengthTicker()`
/// synchronously right after `player.attack(entity)`
/// (`MultiPlayerGameMode.java:425-430`).
#[test]
fn attacking_an_entity_resets_the_strength_ticker_immediately() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    // Reach full strength first, so the reset is unambiguous.
    for _ in 0..5 {
        sim.step(1.0 / 20.0);
    }
    assert_eq!(sim.attack_strength_scale(), 1.0);

    sim.write(|w| w.resource_mut::<EntityRayTarget>().0 = Some(42));
    sim.begin_attack_live();

    assert_eq!(
        sim.attack_strength_scale(),
        0.0,
        "attacking an entity must reset the ticker before the next tick, not after it"
    );
}

// -- crit particles ------------------------------------------------------
//
// `Sim::maybe_spawn_crit_particles`, reached only through the real
// production entry point (`begin_attack_live`), never called directly —
// proving the wiring, not just the private helper in isolation.

/// Spawns a real, ingested entity (through the same `ClientEvent` path
/// production uses, not a hand-built ECS component set) at `feet + (2,
/// 0, 0)`, so it is both a valid attack target and, via [`EntityIndex`],
/// resolvable by [`Sim::maybe_spawn_crit_particles`].
fn spawn_crit_test_target(sim: &mut Sim, entity_id: i32, kind: &str) {
    let feet = sim.player().position;
    ingest(
        sim,
        lodestone_client::ClientEvent::EntitySpawned {
            entity_id,
            uuid: None,
            entity_type: kind.parse().expect("valid entity type key"),
            pos: lodestone_model::Vec3::new(feet.x + 2.0, feet.y, feet.z),
            rotation: Rotation::new(0.0, 0.0),
            velocity: None,
        },
    );
}

/// Charges the attack-strength ticker to full (5 ticks, unarmed) with
/// `sprint` held throughout — stepping is required for a sprint key to
/// reach [`MovementIntent`] at all, so the charge and the sprint intent
/// are established together rather than in two passes that could disagree
/// about which ticks actually ran. `Forward` is held alongside `Sprint`
/// because vanilla's own sprint gate requires forward movement intent —
/// holding the sprint key alone (watched failing) never sets
/// `MovementIntent::sprint`, the same gate `submerged_and_sprinting_
/// enters_the_swim_pose`'s existing setup already relies on.
fn reach_full_strength(sim: &mut Sim, sprint: bool) {
    if sprint {
        sim.input_mut(|i| i.set(lodestone_controller::Action::Forward, true));
        sim.input_mut(|i| i.set(lodestone_controller::Action::Sprint, true));
    }
    for _ in 0..5 {
        sim.step(1.0 / 20.0);
    }
    assert_eq!(
        sim.attack_strength_scale(),
        1.0,
        "test setup must reach full attack strength before the assertions below mean \
         anything"
    );
}

fn crit_particle_count(sim: &mut Sim) -> usize {
    sim.particles_mut(|p| p.engine_mut().particles().len())
}

/// The positive case: full strength, airborne (falling, not grounded),
/// not sprinting, not submerged, target is a `LivingEntity` — vanilla's
/// `canCriticalAttack` (`Player.java:1032-1041`) is satisfied on every
/// clause this port models, so the attack must spawn crit particles.
#[test]
fn a_full_strength_airborne_hit_on_a_living_target_spawns_crit_particles() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    spawn_crit_test_target(&mut sim, 77, "minecraft:pig");
    reach_full_strength(&mut sim, false);
    let local = sim.local;
    sim.write(|w| {
        let mut state = w.get_mut::<PhysicsState>(local).expect("local player");
        state.0.fall_distance = 3.0;
        state.0.on_ground = false;
    });

    let before = crit_particle_count(&mut sim);
    sim.write(|w| w.resource_mut::<EntityRayTarget>().0 = Some(77));
    sim.begin_attack_live();
    let after = crit_particle_count(&mut sim);

    assert!(
        after > before,
        "a full-strength airborne hit on a living target must spawn crit particles, \
         before={before} after={after}"
    );
}

/// **Negative control, watched failing.** With the identical setup above
/// except `on_ground = true`, vanilla's `!onGround` clause fails and no
/// particles must spawn — proving the positive test is not vacuously
/// green (e.g. from particles some *other* code path already emits).
#[test]
fn crit_particles_do_not_spawn_while_grounded() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    spawn_crit_test_target(&mut sim, 78, "minecraft:pig");
    reach_full_strength(&mut sim, false);
    let local = sim.local;
    sim.write(|w| {
        let mut state = w.get_mut::<PhysicsState>(local).expect("local player");
        state.0.fall_distance = 3.0;
        state.0.on_ground = true;
    });

    let before = crit_particle_count(&mut sim);
    sim.write(|w| w.resource_mut::<EntityRayTarget>().0 = Some(78));
    sim.begin_attack_live();
    let after = crit_particle_count(&mut sim);

    assert_eq!(
        after, before,
        "a grounded hit must not spawn crit particles even at full strength and \
         fall_distance > 0"
    );
}

/// **Negative control.** Sprinting fails vanilla's `!isSprinting` clause.
#[test]
fn crit_particles_do_not_spawn_while_sprinting() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    spawn_crit_test_target(&mut sim, 79, "minecraft:pig");
    reach_full_strength(&mut sim, true);
    let local = sim.local;
    sim.write(|w| {
        let mut state = w.get_mut::<PhysicsState>(local).expect("local player");
        state.0.fall_distance = 3.0;
        state.0.on_ground = false;
    });
    assert!(
        sim.movement_intent().sprint,
        "test setup must actually be sprinting, or this control tests nothing"
    );

    let before = crit_particle_count(&mut sim);
    sim.write(|w| w.resource_mut::<EntityRayTarget>().0 = Some(79));
    sim.begin_attack_live();
    let after = crit_particle_count(&mut sim);

    assert_eq!(
        after, before,
        "a sprinting hit must not spawn crit particles"
    );
}

/// **Negative control.** A dropped item is not a `LivingEntity`
/// (`Player.java:1039`'s `entity instanceof LivingEntity` clause) —
/// vanilla never plays a crit sparkle on a punched item stack.
#[test]
fn crit_particles_do_not_spawn_against_a_non_living_target() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    spawn_crit_test_target(&mut sim, 80, "minecraft:item");
    reach_full_strength(&mut sim, false);
    let local = sim.local;
    sim.write(|w| {
        let mut state = w.get_mut::<PhysicsState>(local).expect("local player");
        state.0.fall_distance = 3.0;
        state.0.on_ground = false;
    });

    let before = crit_particle_count(&mut sim);
    sim.write(|w| w.resource_mut::<EntityRayTarget>().0 = Some(80));
    sim.begin_attack_live();
    let after = crit_particle_count(&mut sim);

    assert_eq!(
        after, before,
        "a hit on a non-living entity must not spawn crit particles"
    );
}

/// **Negative control.** Below `fullStrengthAttack`'s `> 0.9F` threshold,
/// vanilla's outer gate in `Player.attack` never reaches
/// `canCriticalAttack` at all — this is the ticker axis, not the
/// fall/ground/sprint/water axis the other controls cover.
#[test]
fn crit_particles_do_not_spawn_below_full_attack_strength() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    spawn_crit_test_target(&mut sim, 81, "minecraft:pig");
    // One tick in: well under the 5-tick unarmed delay, so
    // `attack_strength_scale_at(0.5)` is nowhere near `0.9`.
    sim.step(1.0 / 20.0);
    assert!(sim.attack_strength_scale() < 0.9);
    let local = sim.local;
    sim.write(|w| {
        let mut state = w.get_mut::<PhysicsState>(local).expect("local player");
        state.0.fall_distance = 3.0;
        state.0.on_ground = false;
    });

    let before = crit_particle_count(&mut sim);
    sim.write(|w| w.resource_mut::<EntityRayTarget>().0 = Some(81));
    sim.begin_attack_live();
    let after = crit_particle_count(&mut sim);

    assert_eq!(
        after, before,
        "an attack well under full strength must not spawn crit particles"
    );
}

/// The geometric half of entity targeting: [`Sim::update_entity_target`]
/// must find a spawned entity the ray points straight at, and report it
/// by its server (`MinecraftEntityId`), never a `bevy_ecs::Entity`.
#[test]
fn update_entity_target_finds_a_spawned_entity_along_the_ray() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    let feet = sim.player().position;
    ingest(
        &mut sim,
        lodestone_client::ClientEvent::EntitySpawned {
            entity_id: 99,
            uuid: None,
            entity_type: "minecraft:pig".parse().expect("valid entity type key"),
            pos: lodestone_model::Vec3::new(feet.x + 2.0, feet.y, feet.z),
            rotation: Rotation::new(0.0, 0.0),
            velocity: None,
        },
    );
    // A horizontal ray at a height just above the pig's own feet — safely
    // inside any real pig hitbox's vertical span without needing to know
    // its exact height, and well below a human eye height (1.6), which
    // would sail clean over a pig-sized box on a perfectly level ray.
    let origin = [feet.x, feet.y + 0.1, feet.z];
    let dir = [1.0, 0.0, 0.0];
    sim.update_entity_target(origin, dir, None);
    assert_eq!(
        sim.entity_target(),
        Some(99),
        "the ray should find the spawned pig by its server entity id"
    );
}

/// An entity past [`ENTITY_REACH`] must not be targetable, even though it
/// is well within block [`REACH`] — vanilla's shorter entity-interaction
/// range, not the block one.
#[test]
fn update_entity_target_ignores_an_entity_beyond_entity_reach() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    let feet = sim.player().position;
    ingest(
        &mut sim,
        lodestone_client::ClientEvent::EntitySpawned {
            entity_id: 7,
            uuid: None,
            entity_type: "minecraft:pig".parse().expect("valid entity type key"),
            // Within block REACH (4.5) but past ENTITY_REACH (3.0).
            pos: lodestone_model::Vec3::new(feet.x + 4.0, feet.y, feet.z),
            rotation: Rotation::new(0.0, 0.0),
            velocity: None,
        },
    );
    // Same height convention as `update_entity_target_finds_a_spawned_entity_along_the_ray`
    // — this must fail on *reach*, not on the ray sailing over the box.
    let origin = [feet.x, feet.y + 0.1, feet.z];
    let dir = [1.0, 0.0, 0.0];
    sim.update_entity_target(origin, dir, None);
    assert_eq!(
        sim.entity_target(),
        None,
        "an entity beyond entity-interaction range must not be targetable"
    );
}

/// Issue #12's knockback half: a `ClientboundSetEntityMotionPacket`
/// (`ClientEvent::EntityVelocity`) naming the local player's own server
/// entity id must overwrite `PlayerState.velocity` outright — vanilla's
/// `Entity.lerpMotion` is `setDeltaMovement(movement)`, an unconditional
/// replace, and `LocalPlayer` declares no override (`Entity.java:2649-2651`).
/// Before this fix the event fell into the generic `Velocity` component
/// instead, which nothing reads for the local player, so a server-applied
/// hit never moved the client at all.
#[test]
fn server_sent_knockback_replaces_the_local_players_velocity() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    ingest(&mut sim, login_event(3));
    assert_eq!(
        sim.player().velocity,
        Vec3d::ZERO,
        "test setup: a fresh player starts at rest"
    );
    ingest(
        &mut sim,
        lodestone_client::ClientEvent::EntityVelocity {
            entity_id: 3,
            velocity: lodestone_model::Vec3::new(1.0, 2.0, -3.0),
        },
    );
    assert_eq!(
        sim.player().velocity,
        Vec3d::new(1.0, 2.0, -3.0),
        "knockback naming our own id must land in PlayerState.velocity, \
         the field `player_physics` actually integrates"
    );
}

/// The swing is a **tick** state machine. Reading it across many sub-tick
/// frames must not advance it — the defect
/// `limb_swing_tracks_per_tick_travel_not_the_interpolation_gap` records for
/// the walk cycle, where a per-frame drive made the animation up to 3x too
/// fast and frame-rate dependent.
#[test]
fn swing_progress_is_tick_driven_not_frame_driven() {
    let mut sim = Sim::new(test_config());
    sim.swing_hand();
    sim.step(1.0 / 20.0); // one whole tick: the clock starts
    sim.step(1.0 / 20.0); // and advances once
    let after_two_ticks = sim.hand_swing_progress();

    // 200 sub-tick frames at 1 ms. `FrameClock` accumulates them, so a few
    // whole ticks *will* elapse across 200 ms — the claim is not "nothing
    // changes", it is that the change tracks elapsed *ticks*, so 200 tiny
    // frames advance the swing no further than the 4 ticks their total
    // duration contains.
    for _ in 0..200 {
        sim.step(0.001);
    }
    let after_frames = sim.hand_swing_progress();
    let ticks_elapsed = 4; // 200 ms / 50 ms
    let ceiling = after_two_ticks + (ticks_elapsed + 1) as f32 / 6.0;
    assert!(
        after_frames <= ceiling,
        "200 sub-tick frames advanced the swing to {after_frames}, past the {ceiling} \
         that {ticks_elapsed} ticks of elapsed time allows — the clock is being \
         driven per frame"
    );
}

/// Both consumers read the same clock, so the first-person arm and the
/// self-avatar's body can never disagree about where in the swing we are.
#[test]
fn the_third_person_body_swings_off_the_same_clock_as_the_arm() {
    let mut sim = Sim::new(test_config());
    sim.cycle_camera_type();
    sim.swing_hand();
    // Step to a tick where the swing is genuinely mid-arc, so `assert_eq` is
    // comparing something other than two zeroes.
    let mut arm = 0.0;
    for _ in 0..4 {
        sim.step(1.0 / 20.0);
        arm = sim.hand_swing_progress();
        if arm > 0.1 {
            break;
        }
    }
    assert!(arm > 0.1, "the swing should be mid-arc, got {arm}");
    let body = sim
        .third_person_body_state()
        .expect("third person is on")
        .anim
        .attack_anim;
    assert!(
        (body - arm).abs() < 1e-6,
        "the self-avatar's attack_anim ({body}) must match the arm's ({arm})"
    );
}

#[test]
fn chunk_dirty_signal_reschedules_a_loaded_column() {
    // A `ChunkLoaded`/`NetUpdate::Chunk { x, z }` signal must re-mesh the
    // column it names (the §12.24 dirty-region trigger), so the live-world
    // swap is a source change, not new plumbing.
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    assert_eq!(sim.pending_meshes(), 0, "drained to a clean slate");
    let pos = *sim
        .chunk_world()
        .read()
        .iter()
        .next()
        .expect("local world has a column")
        .0;
    let (cx, cz) = (pos.x, pos.z);
    sim.mark_column_dirty(cx, cz);
    assert!(
        sim.pending_meshes() > 0,
        "the loaded column was re-scheduled"
    );
}

#[test]
fn chunk_arrival_also_remeshes_its_loaded_neighbours() {
    // A section's geometry depends on its whole 3×3×3 neighbourhood, so a
    // column meshed before its neighbour loaded baked its seam against air —
    // which is what puts a falling water "wall" at every chunk border. The
    // arrival signal must therefore dirty the eight loaded neighbours too.
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    let pos = *sim
        .chunk_world()
        .read()
        .iter()
        .next()
        .expect("local world has a column")
        .0;
    // Pick a column with at least one loaded horizontal neighbour.
    let (cx, cz) = (pos.x, pos.z);
    let neighbours: Vec<(i32, i32)> = (-1..=1)
        .flat_map(|dx| (-1..=1).map(move |dz| (dx, dz)))
        .filter(|&(dx, dz)| (dx, dz) != (0, 0))
        .map(|(dx, dz)| (cx + dx, cz + dz))
        .filter(|&(nx, nz)| sim.chunk_world().contains_column(nx, nz))
        .collect();
    assert!(
        !neighbours.is_empty(),
        "fixture must have a loaded neighbour, else this asserts nothing"
    );

    sim.on_column_arrived(cx, cz);
    // `heal_dirty_columns` is an `Update` system now; run the schedule the way
    // `Sim::step` does rather than calling a method. `DIRTY_COLUMN_BUDGET` is
    // 4 and the fixture has up to 8 loaded neighbours, so drive it until the
    // dirty set is empty.
    while !sim.terrain(|t| t.dirty_columns.is_empty()) {
        sim.ecs().write().run_schedule(lodestone_ecs::Update);
    }
    let _ = neighbours.len();
    let meshed: HashSet<(i32, i32)> = sim
        .drain_all_meshes()
        .into_iter()
        .map(|m| (m.key.cx, m.key.cz))
        .chain(sim.drain_removals().into_iter().map(|k| (k.cx, k.cz)))
        .collect();

    assert!(meshed.contains(&(cx, cz)), "the arriving column was meshed");
    for n in &neighbours {
        assert!(
            meshed.contains(n),
            "loaded neighbour {n:?} was not re-meshed — its seam stays baked \
             against air (the chunk-border water wall)"
        );
    }
}

#[test]
fn neighbour_remesh_skips_columns_that_are_not_loaded() {
    // The control for the test above: queueing absent columns would mesh
    // nothing, log a drop, and let "every arrival dirties 8 neighbours" pass
    // without any of them being real.
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.on_column_arrived(9999, 9999);
    assert!(
        sim.terrain(|t| t.dirty_columns.is_empty()),
        "no neighbour of an out-of-world column is loaded, so none is queued"
    );
}

#[test]
fn chunk_dirty_signal_ignores_an_absent_column() {
    // Columns we don't hold (e.g. before the live world source is wired in)
    // must be a no-op, never a panic or spurious work.
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.mark_column_dirty(9999, 9999);
    assert_eq!(sim.pending_meshes(), 0, "absent column schedules nothing");
}

#[test]
fn placing_against_a_face_adds_a_block() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    let feet = sim.player().position;
    // Target a floor block a few blocks away (clear of the player AABB),
    // place on its top face.
    let bx = feet.x.floor() as i32 + 3;
    let bz = feet.z.floor() as i32;
    let s = crate::worldgen::surface_height(bx, bz);
    sim.set_target(Some(crate::raycast::RayHit::face_center(
        [bx, s, bz],
        [0, 1, 0],
    )));
    {
        let store = sim.chunk_world();
        let world = store.read();
        let view = WorldCollision::new(&world);
        assert_eq!(view.block_at(bx, s + 1, bz), id::AIR, "cell starts empty");
    }
    assert!(sim.place_block(), "should place onto the top face");
    let store = sim.chunk_world();
    let world = store.read();
    let view = WorldCollision::new(&world);
    assert_ne!(view.block_at(bx, s + 1, bz), id::AIR, "block now present");
}

#[test]
fn cannot_place_inside_the_player() {
    let mut sim = Sim::new(test_config());
    for _ in 0..20 {
        sim.step(1.0 / 20.0);
    }
    let feet = sim.player().position;
    // Target the block under the feet, whose top face is where the player
    // stands — placing there would clip the player, so it must be refused.
    sim.set_target(Some(crate::raycast::RayHit::face_center(
        [
            feet.x.floor() as i32,
            feet.y.floor() as i32 - 1,
            feet.z.floor() as i32,
        ],
        [0, 1, 0],
    )));
    assert!(!sim.place_block(), "placing inside the player is refused");
}

/// Issue #58's precondition half: a real walking player must actually
/// accumulate `walkDist` and ease the amplitude up, and **only the render
/// camera** may see the result.
///
/// The corridor is not decoration. The offline world is real generated
/// terrain (`lodestone-worldgen`), the player spawns on a slope, and walking
/// north walls them out after ~0.2 blocks — `distance_walked_scales_with_the`
/// speed test above learned that the hard way. A bob gate run against a
/// walled-in player reads `walk_phase: -0.0, bob: 0.0` and asserts nothing,
/// which is the *precondition* species of vacuous test.
#[test]
fn walking_accumulates_a_real_bob_that_only_the_render_camera_sees() {
    let mut sim = Sim::new(test_config());
    // Player spawns at (0.5, feet, 0.5) facing north (-Z, yaw 180).
    let feet_y = sim.player().position.y.floor() as i32;
    for dz in -25..=1 {
        for dx in -1..=1 {
            sim.set_block_world([dx, feet_y - 1, dz], id::STONE);
            sim.set_block_world([dx, feet_y, dz], id::AIR);
            sim.set_block_world([dx, feet_y + 1, dz], id::AIR);
            sim.set_block_world([dx, feet_y + 2, dz], id::AIR);
        }
    }
    // Settle on the fresh floor: while airborne `updateBob`'s `onGround` gate
    // holds the amplitude at zero, so a gate that never lands measures the
    // fall rather than the walk.
    for _ in 0..20 {
        sim.step(1.0 / 20.0);
    }
    assert!(
        sim.player().on_ground,
        "precondition: the player must be standing before the walk starts"
    );
    let still = sim.bob_frame();
    assert_eq!(still.bob, 0.0, "a settled, still player has no amplitude");
    assert_eq!(
        sim.render_camera(1.0).position,
        sim.camera(1.0).position,
        "and with no bob the two cameras are bit-identical, not merely close"
    );

    let start = sim.player().position;
    sim.input_mut(|i| i.set(lodestone_controller::Action::Forward, true));
    for _ in 0..30 {
        sim.step(1.0 / 20.0);
    }
    let travelled = (sim.player().position.z - start.z).abs();
    assert!(
        travelled > 1.0,
        "precondition: the corridor must let the player actually walk; only \
         {travelled:.3} blocks covered, so the bob below would be measuring a \
         walled-in player"
    );

    let walking = sim.bob_frame();
    assert!(
        walking.bob > 0.02,
        "the amplitude must ease up from real movement, got {}",
        walking.bob
    );
    assert!(
        walking.bob <= 0.1 + 1e-6,
        "and must never exceed vanilla's 0.1 ceiling, got {}",
        walking.bob
    );
    // `walkDist` is `distance * 0.6` accumulated, then negated, so a metre of
    // travel is well over half a unit of phase.
    assert!(
        walking.walk_phase.abs() > 0.5,
        "the stride phase must advance, got {}",
        walking.walk_phase
    );

    // The half that would be a gameplay bug rather than a visual one.
    // `Self::camera` is the block-targeting ray origin *and* the audio
    // listener; vanilla bobs neither, because its bob is folded into the
    // projection matrix and `getPickRay` never reads that.
    assert_ne!(
        sim.render_camera(1.0).position,
        sim.camera(1.0).position,
        "the drawn camera must bob"
    );

    // And the option zeroes the frame outright rather than scaling it, so
    // `bobbed_camera` short-circuits and the two cameras are byte-equal again.
    sim.set_view_bobbing(false);
    assert_eq!(sim.bob_frame(), crate::camera_rig::BobFrame::default());
    assert_eq!(
        sim.render_camera(1.0).position,
        sim.camera(1.0).position,
        "with View Bobbing off, render_camera must be bit-identical to camera"
    );
    assert_eq!(sim.render_camera(1.0).pitch, sim.camera(1.0).pitch);
    // Control: turning it back on restores the difference, so the equality
    // above is the option working and not the walk having decayed.
    sim.set_view_bobbing(true);
    assert_ne!(
        sim.render_camera(1.0).position,
        sim.camera(1.0).position,
        "control failed: the bob is gone regardless of the option, so the \
         equality above proves nothing about the option"
    );
}

/// The camera-side half of `bobHurt`: a local-player damage report must reach
/// the interpolated bob frame with its direction, and must **survive View
/// Bobbing being off** — vanilla's `bobHurt` is unconditional
/// (`GameRenderer.java:534-536`), only `bobView` is gated on the option.
///
/// The net-apply feed (`ClientEvent::EntityHurtAnimation` naming the local
/// player's own id → [`Sim::on_local_player_hurt`]) is live now — `net.rs`'s
/// `forward` produces `NetUpdate::HurtAnimation` and `net_apply` filters it
/// against `server_entity_id()`. This test still drives the hook directly, which
/// keeps it hermetic. What it pins is the *camera's* contract:
/// the countdown and the wire `yaw` (90° here, a side hit — a frontal hit is
/// `hurtDir 0`, the pure-roll case, see `render_camera`) both reach the frame,
/// and the option must not mute them.
#[test]
fn local_player_hurt_reaches_the_bob_frame_and_survives_view_bobbing_off() {
    let mut sim = Sim::new(test_config());
    // Precondition: a never-hit player has no flash and no direction.
    assert!(sim.bob_frame().hurt <= 0.0, "no flash before any hit");
    assert_eq!(sim.bob_frame().hurt_dir_degrees, 0.0);

    sim.on_local_player_hurt(90.0);
    let hurt = sim.bob_frame();
    assert!(hurt.hurt > 0.0, "a fresh hit must be flashing");
    assert_eq!(hurt.hurt_dir_degrees, 90.0, "the wire yaw must survive");

    // Only the walk terms are gated on the option; the tilt is not.
    sim.set_view_bobbing(false);
    let off = sim.bob_frame();
    assert_eq!(off.walk_phase, 0.0, "the walk terms must still be muted");
    assert_eq!(off.bob, 0.0, "the walk terms must still be muted");
    assert!(off.hurt > 0.0, "bobHurt must not be muted by the option");
    assert_eq!(off.hurt_dir_degrees, 90.0);

    // The countdown is driven by the 20 Hz tick, like `LivingEntity.tick`'s.
    sim.step(1.0 / 20.0);
    assert!(
        sim.bob_frame().hurt < off.hurt,
        "the tilt must count down one tick at a time"
    );

    // `render_camera` still passes a zero strength, and that is now a *routing*
    // fact rather than a hold: `bobbed_camera` cannot carry roll, so the tilt
    // travels the eye-space seam instead. The camera's own pitch is therefore
    // untouched by the flash — asserted, because a future "fix" that smeared the
    // roll into pitch would look like progress and would be wrong.
    sim.set_view_bobbing(true);
    assert_eq!(
        sim.render_camera(1.0).pitch,
        sim.camera(1.0).pitch,
        "the tilt must not be smeared into the camera's pitch"
    );
}

/// The hop the test above used to call the missing one: a local-player damage
/// report must reach an **actual eye-space matrix**, and the accessibility option
/// must be able to switch it off.
///
/// This is the gate that catches the defect this feature spent months in: every
/// piece — the countdown, the direction, the quartic easing, the option — was
/// built and unit-tested, and the composed transform handed to the renderer was a
/// hard-coded identity. Asserting on `bob_frame().hurt` cannot see that; asserting
/// on the matrix can.
///
/// The magnitude is predicted rather than compared for inequality: at `hurt == 8`
/// the tilt is `-14·sin(0.4096π) = -13.03°`, whose matrix entries carry
/// `sin(13.03°) = 0.2255`. A tolerance of `0.01` therefore separates "the tilt
/// arrived" from "something moved" by more than twenty times.
#[test]
fn a_local_player_hit_reaches_a_real_eye_space_matrix() {
    let mut sim = Sim::new(test_config());
    assert_eq!(
        sim.damage_tilt_eye_transform().to_cols_array(),
        glam::Mat4::IDENTITY.to_cols_array(),
        "an unhurt player's transform must be exactly the identity"
    );

    sim.on_local_player_hurt(0.0);
    // Two ticks in, `hurt` is 8, which is close to the quartic peak.
    sim.step(1.0 / 20.0);
    sim.step(1.0 / 20.0);
    let frame = sim.bob_frame();
    // Recomputed here from the jar's constants rather than read back out of the
    // implementation: `-hurt' * 14`, where `hurt' = sin(t^4 * PI)`, `t = hurt/10`.
    let t = frame.hurt / 10.0;
    let expected_degrees = -14.0 * (t.powi(4) * std::f32::consts::PI).sin();
    let m = sim.damage_tilt_eye_transform();
    // A head-on hit is pure roll about eye +Z, so eye-space up moves in x by
    // exactly `-sin(tilt)`.
    let up = m.transform_vector3(glam::Vec3::Y);
    let predicted_x = -expected_degrees.to_radians().sin();
    assert!(
        (up.x - predicted_x).abs() < 0.01,
        "up moved to {up:?}; a {expected_degrees} degree roll predicts x = {predicted_x}"
    );
    assert!(
        up.x.abs() > 0.2,
        "precondition: the tilt near its peak is a fifth of a unit, not noise"
    );

    // The accessibility option is a real off switch, all the way through the sim.
    sim.set_damage_tilt_strength(0.0);
    let off = sim.damage_tilt_eye_transform().transform_vector3(glam::Vec3::Y);
    assert!(
        off.x.abs() < 1e-6,
        "a zero Damage Tilt strength must leave the matrix inert, got {off:?}"
    );
}

/// Issue #154, end-to-end: `Sim::spyglass_scoping`'s two halves
/// (`Self::using_item` and the held-item identity check) have to actually
/// reach `Self::render_camera`'s FOV, not just exist. Predicts the *exact*
/// FOV from `lodestone_render::spyglass_fov_modifier`'s tested `0.1`
/// constant rather than asserting only that the number changed — a wrong
/// multiplier would still pass a same-direction-only check.
#[test]
fn spyglass_scoping_zooms_the_render_camera_by_exactly_a_tenth() {
    let (net, _actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);

    let base_fov = sim.render_camera(1.0).fov_y_degrees;
    assert_eq!(
        base_fov,
        crate::camera_rig::FOV_Y_DEGREES,
        "precondition: an empty hand must not zoom at all"
    );

    give_main_hand_item(&mut sim, "minecraft:spyglass");
    sim.use_item_live();
    let zoomed_fov = sim.render_camera(1.0).fov_y_degrees;
    assert_eq!(
        zoomed_fov,
        base_fov * lodestone_render::spyglass_fov_modifier(true),
        "a held, in-use spyglass must scale the FOV by exactly vanilla's 0.1 \
         override, not merely reduce it"
    );
    assert!(
        (zoomed_fov - 7.0).abs() < 1e-6,
        "70 degrees * 0.1 is 7.0 exactly; got {zoomed_fov}"
    );

    // -- negative control -------------------------------------------------
    // Using a non-spyglass item must not zoom, proving the assertions above
    // test the item's identity and not merely "is using any item".
    sim.end_use_live();
    give_main_hand_item(&mut sim, "minecraft:bow");
    sim.use_item_live();
    assert_eq!(
        sim.render_camera(1.0).fov_y_degrees,
        base_fov,
        "using a bow must not zoom — only a spyglass does"
    );

    // And releasing the spyglass must drop the zoom back to base, so the
    // wiring is proven live rather than latched permanently on the first
    // press.
    sim.end_use_live();
    give_main_hand_item(&mut sim, "minecraft:spyglass");
    assert_eq!(
        sim.render_camera(1.0).fov_y_degrees,
        base_fov,
        "holding a spyglass without using it must not zoom"
    );
}

/// Issue #391's gate: the walk bob must reach the projection **at vanilla's
/// own magnitude, on vanilla's own axes**, driven by a real walking `Sim`.
///
/// # Why the existing gates could not have caught a wrong amplitude
///
/// Every other bob gate *supplies its own* `BobFrame`: the unit tests and
/// `tests/view_bob_pixels.rs` hand `ViewBob::tick`/`bobbed_camera` numbers
/// they chose, so they prove the arithmetic and can say nothing about whether
/// `Sim` feeds it realistic ones. That is `CLAUDE.md`'s *world* species —
/// the flaw would live in the input data and be invisible in the test source.
/// So step 1 here pins the **inputs** against vanilla's own walk speed,
/// measured from the player's position and not read back out of the bob.
///
/// # Why the far point is the discriminator
///
/// The bob is a translation *and* two rotations. A point at infinity is
/// unaffected by translation, so its screen displacement is the **nod alone**
/// — a nod-free bob moves it exactly `0.0` px. That is the separation
/// `docs/view-bobbing.md` records the chest-bbox pixel gate cannot make (its
/// +8.50 px is within 0.2 px of the +8.31 a nod-free bob gives). Conversely
/// the far point's *horizontal* displacement must stay at zero: the sway is a
/// translation and cannot move infinity, and the roll is deliberately dropped
/// by the fold, so any yaw leaking out of `bobbed_camera` shows up here.
///
/// The near point then carries the translation, and the two axes are
/// distinguishable by *shape* as well as size: the dip is rectified
/// (`-|cos|`, one-way) while the sway is a full sine (both ways). A gate that
/// only asked "did the frame change" passes on a bob with the wrong
/// amplitude, the wrong phase or the wrong axis; every number below is
/// predicted from `GameRenderer.bobView`'s constants before it is measured.
#[test]
fn the_walk_bob_reaches_the_projection_at_vanillas_own_magnitude_and_axis() {
    /// Vanilla's walking speed, blocks per tick: `4.317 m/s / 20`.
    const WALK_BLOCKS_PER_TICK: f32 = 0.2159;
    /// `AbstractClientPlayer.updateBob`'s `Math.min(0.1F, ...)` ceiling,
    /// which a walking player saturates.
    const BOB_CEILING: f32 = 0.1;
    /// The nominal viewport the pixel predictions below are stated for.
    const VIEW_W: f32 = 1920.0;
    const VIEW_H: f32 = 1080.0;
    const ASPECT: f32 = VIEW_W / VIEW_H;

    let mut sim = Sim::new(test_config());
    // The corridor is a precondition, not decoration — see
    // `walking_accumulates_a_real_bob_that_only_the_render_camera_sees`.
    // Longer than that one's because this walks for ~5 s.
    let feet_y = sim.player().position.y.floor() as i32;
    for dz in -60..=1 {
        for dx in -1..=1 {
            sim.set_block_world([dx, feet_y - 1, dz], id::STONE);
            sim.set_block_world([dx, feet_y, dz], id::AIR);
            sim.set_block_world([dx, feet_y + 1, dz], id::AIR);
            sim.set_block_world([dx, feet_y + 2, dz], id::AIR);
        }
    }
    for _ in 0..20 {
        sim.step(1.0 / 20.0);
    }
    assert!(sim.player().on_ground, "precondition: standing before the walk");

    // --- 1. The inputs. Vanilla's walk speed, vanilla's ceiling. ---------
    sim.input_mut(|i| i.set(lodestone_controller::Action::Forward, true));
    for _ in 0..30 {
        sim.step(1.0 / 20.0);
    }
    let before = sim.player().position;
    let phase_before = sim.bob_frame().walk_phase;
    sim.step(1.0 / 20.0);
    let moved = ((sim.player().position.x - before.x) as f32)
        .hypot((sim.player().position.z - before.z) as f32);
    assert!(
        (moved - WALK_BLOCKS_PER_TICK).abs() < 2e-3,
        "precondition: the player must be walking at vanilla's real speed, \
         not some fixture crawl — {moved:.5} blocks/tick against \
         {WALK_BLOCKS_PER_TICK}"
    );
    let settled = sim.bob_frame();
    assert!(
        (settled.bob - BOB_CEILING).abs() < 1e-4,
        "a walking player saturates `min(0.1, speed)`; got {}",
        settled.bob
    );
    // `LocalPlayer.move`: `addWalkedDistance(length(dx, dz) * 0.6)`, negated
    // by `getBackwardsInterpolatedWalkDistance`. Compared against `moved`,
    // which came from the position and not from the bob.
    let advance = phase_before - settled.walk_phase;
    assert!(
        (advance - moved * 0.6).abs() < 2e-4,
        "the stride phase must advance by exactly 0.6x the distance actually \
         travelled: {advance:.6} against {:.6}",
        moved * 0.6
    );

    // --- 2. The pixels, sampled at frame rate rather than tick rate. -----
    // 60 fps so the partial-tick interpolation is exercised and the sampling
    // lands within 0.07 rad of the nod's peak, which is what lets the
    // magnitude assertion below be tight.
    let screen = |c: &Camera, w: glam::Vec3| {
        let clip = c.view_projection() * w.extend(1.0);
        (
            (1.0 + clip.x / clip.w) * 0.5 * VIEW_W,
            (1.0 - clip.y / clip.w) * 0.5 * VIEW_H,
        )
    };
    let (mut far_dy_lo, mut far_dy_hi) = (f32::MAX, f32::MIN);
    let (mut far_dx_lo, mut far_dx_hi) = (f32::MAX, f32::MIN);
    let (mut near_dx_lo, mut near_dx_hi) = (f32::MAX, f32::MIN);
    let (mut near_dy_lo, mut near_dy_hi) = (f32::MAX, f32::MIN);
    for _ in 0..90 {
        sim.step(1.0 / 60.0);
        let cam = sim.camera(ASPECT);
        let bobbed = sim.render_camera(ASPECT);
        // **Both probes sit on `cam.forward()`, not on `-Z`.** They differ
        // only in distance, so the far one is the near one with the
        // translation's parallax divided away.
        //
        // Deriving the direction from the same expression the draw uses is
        // load-bearing, per `CLAUDE.md` — the offline spawn pitch is `10`,
        // not `0`, and a probe placed naively down `-Z` sits 10 deg above the
        // view centre. A pitch change of `t` moves a point at angle `a` by
        // `sec^2(a)/tan(fov/2)`, so that probe read **6.93 px** where the
        // on-axis prediction is 6.73: a 3% error, in the direction that looks
        // like the bob being slightly too strong. Chasing it as a code defect
        // is exactly the trap of restating a constant instead of deriving it.
        let far = cam.position + cam.forward() * 4096.0;
        let near = cam.position + cam.forward() * 3.0;
        for (p, dx_lo, dx_hi, dy_lo, dy_hi) in [
            (far, &mut far_dx_lo, &mut far_dx_hi, &mut far_dy_lo, &mut far_dy_hi),
            (near, &mut near_dx_lo, &mut near_dx_hi, &mut near_dy_lo, &mut near_dy_hi),
        ] {
            let (bx, by) = screen(&bobbed, p);
            let (sx, sy) = screen(&cam, p);
            *dx_lo = dx_lo.min(bx - sx);
            *dx_hi = dx_hi.max(bx - sx);
            *dy_lo = dy_lo.min(by - sy);
            *dy_hi = dy_hi.max(by - sy);
        }
    }
    let box_of = |dx: (f32, f32), dy: (f32, f32)| {
        format!("dx [{:.3}, {:.3}] dy [{:.3}, {:.3}] px", dx.0, dx.1, dy.0, dy.1)
    };
    let far_box = box_of((far_dx_lo, far_dx_hi), (far_dy_lo, far_dy_hi));
    let near_box = box_of((near_dx_lo, near_dx_hi), (near_dy_lo, near_dy_hi));
    // Captured unless `--nocapture`, and the reason every message below
    // quotes a box rather than a fraction: a single number cannot tell a
    // too-small bob from one on the wrong axis.
    println!("bob probe at infinity: {far_box}\nbob probe at 3 blocks: {near_box}");

    // --- 3. The nod, in isolation, against vanilla's constant. -----------
    // `abs(cos(bd*PI - 0.2) * bob) * 5.0` degrees, peaking at `bob * 5`.
    // A rotation of `t` about eye-space +X lifts an on-axis point at
    // infinity to `ndc_y = tan(t) / tan(fov_y / 2)`, i.e. *up* the screen.
    let nod_peak_deg = BOB_CEILING * 5.0;
    let nod_peak_px =
        VIEW_H * 0.5 * nod_peak_deg.to_radians().tan() / 35.0f32.to_radians().tan();
    assert!(
        (far_dy_lo + nod_peak_px).abs() < nod_peak_px * 0.015,
        "the nod must reach the projection at vanilla's full 0.5 deg: expected \
         a peak of -{nod_peak_px:.3} px on a point at infinity, measured \
         {far_box}. Zero here is a nod-free bob, which the chest-bbox pixel \
         gate cannot tell from a correct one."
    );
    assert!(
        far_dy_hi < 0.02,
        "the nod is rectified (`abs`), so a point at infinity may only ever \
         move *up*; measured {far_box}"
    );
    assert!(
        far_dx_lo > -0.05 && far_dx_hi < 0.05,
        "the bob must not yaw: the sway is a pure translation and cannot move \
         a point at infinity, and the fold drops the roll rather than \
         smearing it onto yaw. Measured {far_box}"
    );

    // --- 4. The translation, on the near point, by axis and by shape. ----
    // Sway: `sin(bd*PI) * bob * 0.5`, so +/-0.05 blocks laterally. At 3
    // blocks that is `0.05/3` of an eye-space unit, and the horizontal half
    // angle is `tan(35 deg) * aspect`.
    let sway_px = VIEW_W * 0.5 * (BOB_CEILING * 0.5 / 3.0)
        / (35.0f32.to_radians().tan() * ASPECT);
    assert!(
        near_dx_hi > sway_px * 0.9 && near_dx_lo < -sway_px * 0.9,
        "the sway is a full sine and must swing the near point *both* ways by \
         about {sway_px:.3} px; measured {near_box}"
    );
    // Dip: `-abs(cos(bd*PI) * bob)`, so the eye drops up to 0.1 blocks and a
    // point 3 blocks ahead rises 0.1/3 of a unit *in eye space*, i.e. moves
    // **down** the screen. Rectified, so it is one-way, and it is opposed by
    // the nod near the phase where the dip vanishes — hence a floor on the
    // downward peak rather than a sign assertion.
    let dip_px = VIEW_H * 0.5 * (BOB_CEILING / 3.0) / 35.0f32.to_radians().tan();
    assert!(
        near_dy_hi > (dip_px - nod_peak_px) * 0.9,
        "the dip must drop the eye a full 0.1 blocks, pushing a point 3 blocks \
         ahead down by about {:.3} px net of the nod; measured {near_box}",
        dip_px - nod_peak_px
    );
}

#[test]
fn an_interior_block_change_dirties_exactly_its_own_section() {
    // Local (8,8,8) touches no section boundary, so a live block update
    // there must cost one re-mesh — not the 27 a blanket neighbourhood
    // would submit, and not the ~216 a whole-column signal would.
    let dirty = dirty_sections_for_blocks(3, 4, 5, &[[8, 8, 8]]);
    assert_eq!(
        dirty.iter().copied().collect::<Vec<_>>(),
        vec![(3, 4, 5)],
        "an interior cell reaches no neighbouring section"
    );
}

#[test]
fn a_block_change_on_a_face_also_dirties_that_neighbour() {
    // The bug this pins: breaking a block at local x=15 on a live server
    // leaves the +x neighbour's face baked against the *old* state, which
    // shows as a stale face or z-fighting at every chunk border while
    // mining. The -x neighbour must NOT be dirtied — that is the half of
    // the filter a "dirty all 27" implementation gets wrong.
    let dirty = dirty_sections_for_blocks(3, 4, 5, &[[15, 8, 8]]);
    assert_eq!(
        dirty.iter().copied().collect::<Vec<_>>(),
        vec![(3, 4, 5), (4, 4, 5)],
        "a +x face cell dirties its own section and the +x neighbour only"
    );
}

#[test]
fn a_corner_block_change_dirties_the_full_corner_octant() {
    // (0,0,0) touches three faces, three edges and one corner: 8 sections.
    // Edge and corner neighbours matter because AO samples the 3 cells
    // around each vertex, which reach diagonally across section corners.
    let dirty = dirty_sections_for_blocks(0, 0, 0, &[[0, 0, 0]]);
    assert_eq!(dirty.len(), 8, "a corner cell reaches an octant: {dirty:?}");
    assert!(
        dirty.contains(&(-1, -1, -1)),
        "the diagonal corner is included"
    );
    assert!(!dirty.contains(&(1, 0, 0)), "the far side is not reachable");
}

#[test]
fn a_whole_section_update_is_bounded_by_the_neighbourhood_not_the_cell_count() {
    // A 4096-cell `SECTION_BLOCKS_UPDATE` (a full section rewrite) must not
    // submit 4096 re-meshes. 27 is the hard ceiling because that is the
    // entire neighbourhood any cell in the section can reach.
    let all: Vec<[u8; 3]> = (0..16u8)
        .flat_map(|x| (0..16u8).flat_map(move |y| (0..16u8).map(move |z| [x, y, z])))
        .collect();
    assert_eq!(
        all.len(),
        4096,
        "control: the fixture really is a full section"
    );
    let dirty = dirty_sections_for_blocks(0, 0, 0, &all);
    assert_eq!(dirty.len(), 27, "bounded by the 3x3x3 neighbourhood");
}

// -----------------------------------------------------------------------
// §4.1(c): one `World`, one `GameTick`, one accumulator
// -----------------------------------------------------------------------

/// **The (c) authority test.** One `World` means one `LocalPlayer`.
///
/// `spawn_local_player` and `spawn_session` both spawn an entity carrying the
/// `LocalPlayer` marker. They used to be in different `World`s, so both could
/// exist; in one `World` they have to be one entity, or every
/// `With<LocalPlayer>` system (`tick_hud_overlays`, the physics and egress
/// systems) silently runs against two players and the HUD reads whichever the
/// query happened to yield.
#[test]
fn the_one_world_holds_exactly_one_local_player() {
    let sim = Sim::new(test_config());
    assert_eq!(local_player_count(sim.ecs()), 1);
    // …and it is the entity the driver named, not some other one.
    assert!(
        sim.ecs()
            .read()
            .get::<lodestone_ecs::SessionScoreboard>(sim.local_player())
            .is_some(),
        "the session fold's components must hang off Sim's own local player"
    );
}

/// The control that proves the count above discriminates: spawning the session
/// entity separately — which is exactly what
/// `lodestone_client::state::SharedState::default` does when it is *not* handed
/// a `World` — takes it to two.
#[test]
fn a_separately_spawned_session_entity_makes_two_local_players() {
    let sim = Sim::new(test_config());
    lodestone_ecs::spawn_session(&mut sim.ecs().write());
    assert_eq!(
        local_player_count(sim.ecs()),
        2,
        "the detector must be able to see a second LocalPlayer"
    );
}

/// Note the shape: **one** guard, named, then queried.
///
/// The obvious spelling — `handle.write().query_filtered::<…>().iter(&handle.write())`
/// — takes the write lock twice in one expression and hangs forever, because
/// `parking_lot::RwLock` is not reentrant. It was written that way first and
/// deadlocked the test binary, which is why `EcsHandle`'s rule 1 is stated as
/// "one statement, one guard" rather than as advice.
fn local_player_count(handle: &EcsHandle) -> usize {
    let mut world = handle.write();
    let mut state =
        world.query_filtered::<Entity, bevy_ecs::prelude::With<lodestone_ecs::LocalPlayer>>();
    state.iter(&world).count()
}

/// **The clock-divergence gate.** A maximal stall must advance the *entity*
/// systems' tick count and the player's by the same amount, and that amount
/// must be vanilla's ten.
///
/// This is the measurement Stage 5 recorded and could not fix: `Sim::step`
/// banked `dt.clamp(0.0, 0.25)` (five ticks) while `EntityInterpolator` banked
/// the pacer's `0.5 s` unclamped (ten), so a maximal stall advanced item
/// physics five ticks further than player physics — per stall, cumulatively,
/// with the excess real time discarded rather than reconciled. Counting a
/// system in `TickSet::Animate` (where `tick_walk_animation` lives) against
/// `FrameClock::ticks` is what would have caught it: before (c) those were two
/// schedules in two `World`s and could not have agreed.
#[test]
fn a_maximal_stall_advances_the_entity_and_player_clocks_by_the_same_ten_ticks() {
    use bevy_ecs::resource::Resource;
    use bevy_ecs::schedule::IntoScheduleConfigs;

    #[derive(Resource, Default)]
    struct AnimateRuns(u64);

    let mut sim = Sim::new(test_config());
    {
        let mut world = sim.ecs().write();
        world.init_resource::<AnimateRuns>();
        world.schedule_scope(GameTick, |_w, schedule| {
            schedule.add_systems(
                (|mut runs: bevy_ecs::system::ResMut<AnimateRuns>| runs.0 += 1)
                    .in_set(lodestone_ecs::TickSet::Animate),
            );
        });
    }

    let before = sim.tick_count();
    // Sixty seconds: 1200 ticks of real time, i.e. far past any budget.
    sim.step(60.0);
    let player_ticks = sim.tick_count() - before;
    let animate_runs = sim.ecs().read().resource::<AnimateRuns>().0;

    assert_eq!(
        player_ticks,
        u64::from(lodestone_ecs::MAX_CATCH_UP_TICKS),
        "the one accumulator's catch-up policy is vanilla's ten, not the \
         shell's old five"
    );
    assert_eq!(
        animate_runs, player_ticks,
        "the entity animation tick and the player tick are one schedule on \
         one clock; a difference here is the divergence §4.1(c) deleted"
    );
    // The excess is dropped, not carried: the next frame owes nothing.
    assert!(
        sim.clock().accumulator < lodestone_ecs::TICK_PERIOD,
        "accumulator {} should be a sub-tick residual",
        sim.clock().accumulator
    );
}

/// A quit-to-title resets the **one** accumulator and leaves monotonic time
/// alone.
///
/// `end_session` used to reset the interpolator's accumulator (by replacing the
/// whole interpolator) and not the player's, so a reconnect re-phased the two
/// clocks arbitrarily. There is one to reset now, and the chat timestamps that
/// ride on `FrameClock::secs` must survive it — a line stamped before the
/// teardown still has to age correctly afterwards.
#[test]
fn end_session_resets_the_one_accumulator_and_not_the_monotonic_clock() {
    let mut sim = Sim::with_demo_world(test_config());
    // Leave a deliberate sub-tick residual.
    sim.step(lodestone_ecs::TICK_PERIOD * 1.5);
    assert!(
        sim.clock().accumulator > 0.0,
        "control: there is a residual"
    );
    let secs_before = sim.clock().secs;
    let ticks_before = sim.tick_count();

    sim.end_session();

    assert_eq!(sim.clock().accumulator, 0.0);
    assert_eq!(sim.clock().interp_alpha, 0.0);
    assert!(
        (sim.clock().secs - secs_before).abs() < 1e-12,
        "monotonic time must not rewind, or pre-teardown chat ages break"
    );
    assert_eq!(sim.tick_count(), ticks_before);
}

/// A session teardown clears the render-side entity tracks.
///
/// This used to be a side effect of replacing the whole `EntityInterpolator`
/// (and therefore of dropping its `World`). With one `World` it has to be an
/// explicit despawn, which is exactly the kind of thing that gets dropped in a
/// refactor and shows up as the previous server's mobs still drawn on the title
/// **You could open a crafting table and not get out of it.**
///
/// `close_open_menu` sent `ContainerClose` and nothing else, so
/// [`Sim::open_menu`] stayed `Some` forever — a vanilla server does not echo a
/// close back. Everything downstream keys off that: `active_container_menu`,
/// the key-dispatch gate, the container draw. The dispatch was fixed first and
/// the bug survived, because the function the keys correctly reached did not
/// clear anything.
///
/// The control matters as much as the assertion: it proves the menu really was
/// open first, so a fold that silently failed to open it could not make this
/// pass vacuously.
#[test]
fn closing_a_server_menu_clears_it_locally_without_waiting_for_the_server() {
    use lodestone_model::ClientEvent;

    let mut sim = Sim::with_demo_world(test_config());
    let local = sim.local;
    sim.write(|w| {
        if let Some(mut menus) = w.get_mut::<lodestone_ecs::SessionMenus>(local) {
            menus.0.apply(&ClientEvent::ScreenOpened {
                window_id: 5,
                menu_type: lodestone_model::Identifier::new("minecraft", "crafting").unwrap(),
                title: lodestone_model::Text::literal("Crafting"),
            });
            // 3x3 grid + result + 36 player slots: the content packet is what
            // actually promotes `pending` to `opened`.
            menus.0.apply(&ClientEvent::ContainerContent {
                window_id: 5,
                state_id: 1,
                items: vec![None; 46],
                carried_item: None,
            });
        }
    });
    assert!(
        sim.open_menu().is_some(),
        "control: the menu must actually be open, or this gate proves nothing"
    );

    sim.close_open_menu();

    assert!(
        sim.open_menu().is_none(),
        "closing must clear the local menu immediately — a vanilla server sends \
         no close back, so anything that waits for the wire waits forever"
    );
}

/// screen.
///
/// Issue #36: there is no `EntitySnapshot` to hand `fold_entities` any more —
/// the ingest components it now reads directly are spawned through the real
/// `ClientEvent::EntitySpawned` -> `IngestQueue` -> `NetIngest` path (the
/// [`ingest`] helper), then `Sim::fold_entities` folds them, exactly like a
/// live session's `Sim::step` does.
#[test]
fn end_session_clears_the_entity_tracks() {
    let mut sim = Sim::with_demo_world(test_config());
    ingest(
        &mut sim,
        lodestone_client::ClientEvent::EntitySpawned {
            entity_id: 7,
            uuid: None,
            entity_type: "minecraft:pig".parse().expect("valid entity type key"),
            pos: lodestone_model::Vec3::new(1.0, 64.0, 1.0),
            rotation: Rotation::new(0.0, 0.0),
            velocity: None,
        },
    );
    sim.fold_entities();
    assert_eq!(
        sim.read(crate::entities::tracked_entity_count),
        1,
        "control: the fold really did spawn a track"
    );

    sim.end_session();
    assert_eq!(sim.read(crate::entities::tracked_entity_count), 0);
    assert!(sim.entity_draws().is_empty());
}

// -- world border + spawn point + game rules (issue #436) --------------
//
// `SessionWorldBorder`, `SessionSpawnPoint` and `SessionGameRules` were
// folded, reset on quit-to-title and gated through the real
// `SharedState::apply` path with **no reader anywhere in the shell**. These
// gates drive the real fold and the real accessor.

/// **Vanilla's border-warning formula, against values computed outside this
/// code.**
///
/// `Hud.extractVignette` (`Hud.java:1057-1069`) on a *static* border reduces
/// to `warningDistance == warningBlocks` exactly, because
/// `StaticBorderExtent.getLerpSpeed()` returns `0.0`
/// (`WorldBorder.java:534-535`) and `max(warningBlocks, 0)` is
/// `warningBlocks`. That makes the arithmetic hand-checkable:
///
/// A border of diameter 100 centred on the origin has its edge at ±50. A
/// player at `x = 47` is `3` blocks from it. With `warning_blocks = 5`:
/// `strength = 1 - 3/5 = 0.4`. Every number here comes from vanilla's
/// constants and the packet, not from our implementation.
#[test]
fn the_border_warning_strength_matches_vanillas_hand_computed_value() {
    use lodestone_game::worldborder::{BorderExtent, WorldBorder};

    let border = WorldBorder {
        center_x: 0.0,
        center_z: 0.0,
        extent: BorderExtent::Static { size: 100.0 },
        warning_blocks: 5,
        ..WorldBorder::default()
    };

    let (dist, warn_at, strength) = super::session::border_warning(&border, 47.0, 0.0, 0.0);
    assert!((dist - 3.0).abs() < 1e-9, "edge at 50, player at 47 => 3 blocks: got {dist}");
    assert!(
        (warn_at - 5.0).abs() < 1e-9,
        "a static border's warning distance is warning_blocks exactly, since \
         getLerpSpeed() is 0.0: got {warn_at}"
    );
    assert!(
        (strength - 0.4).abs() < 1e-6,
        "1 - 3/5 = 0.4, hand-computed from vanilla's own expression: got {strength}"
    );

    // Well inside: no warning at all. `6 > 5`, so the `<` fails.
    let (_, _, none) = super::session::border_warning(&border, 44.0, 0.0, 0.0);
    assert!(
        (none - 0.0).abs() < 1e-9,
        "6 blocks out is beyond the 5-block warning band: got {none}"
    );

    // Exactly at the edge is full strength; outside is clamped to 1.0 rather
    // than exceeding it, which is what vanilla's own `Mth.clamp` does one
    // step later (`Hud.java:1073`).
    let (_, _, at_edge) = super::session::border_warning(&border, 50.0, 0.0, 0.0);
    assert!((at_edge - 1.0).abs() < 1e-6, "at the edge => 1 - 0/5 = 1.0: got {at_edge}");
    let (outside, _, beyond) = super::session::border_warning(&border, 80.0, 0.0, 0.0);
    assert!(outside < 0.0, "outside the border the distance is negative: got {outside}");
    assert!(
        (beyond - 1.0).abs() < 1e-6,
        "and the strength clamps at 1.0 rather than running away: got {beyond}"
    );
}

/// **The control for the gate above, and it rejects the wrong hypothesis
/// rather than merely accepting the right one.**
///
/// The obvious wrong port is to use the border's *radius* where vanilla uses
/// the distance to the nearest edge, or the *diameter* where it uses the
/// radius. Both produce a plausible-looking number. A player at `x = 47`
/// inside a 100-diameter border is `3` blocks from the edge, `47` from the
/// centre and `53` from the far edge — three candidate values, only one of
/// which lands inside a 5-block warning band at all.
#[test]
fn the_border_warning_rejects_the_radius_and_diameter_hypotheses() {
    use lodestone_game::worldborder::{BorderExtent, WorldBorder};

    let border = WorldBorder {
        extent: BorderExtent::Static { size: 100.0 },
        warning_blocks: 5,
        ..WorldBorder::default()
    };
    let (dist, _, _) = super::session::border_warning(&border, 47.0, 0.0, 0.0);

    assert!(
        (dist - 47.0).abs() > 1.0,
        "must NOT be the distance from the centre (47) — that hypothesis \
         would never warn inside any normal border: got {dist}"
    );
    assert!(
        (dist - 53.0).abs() > 1.0,
        "must NOT be the distance to the far edge (53): got {dist}"
    );
    assert!(
        (dist - 3.0).abs() < 1e-9,
        "it is the distance to the NEAREST edge (3): got {dist}"
    );
}

/// **The world border reaches the shell through the real fold**, not through a
/// hand-built `WorldBorder`.
///
/// Drives `ClientEvent`s through the same `NetIngest` schedule the net thread
/// runs, then reads `Sim::world_border_warning` — the accessor `app/redraw.rs`
/// calls every frame. Before this accessor, `SessionWorldBorder` had zero
/// readers in the entire shell.
#[test]
fn a_folded_world_border_reaches_the_shells_own_accessor() {
    use lodestone_client::ClientEvent;

    let mut sim = Sim::new(test_config());
    ingest(&mut sim, login_event(1));

    // The precondition that makes the assertion meaningful: an unreported
    // border must answer `None`, so a passing result below cannot be the
    // default leaking through.
    assert!(
        sim.world_border_warning().is_none(),
        "precondition: with no border packet the accessor must report nothing, \
         not the MAX_SIZE default dressed up as a real border"
    );

    ingest(
        &mut sim,
        ClientEvent::WorldBorderInitialized {
            x: 0.0,
            z: 0.0,
            old_size: 100.0,
            new_size: 100.0,
            lerp_time_ms: 0,
            absolute_max_size: 29_999_984,
            warning_blocks: 5,
            warning_time: 15,
        },
    );

    // Pin the position rather than assuming it. The first draft of this gate
    // predicted `50.0` on the belief that a fresh `Sim` starts the player at
    // the origin; it starts at the block *centre* (`x = 0.5`), so the real
    // answer was `49.5` and the assertion caught the assumption. Setting the
    // position makes the prediction independent of that default entirely.
    sim.player_mut(|p| {
        p.position.x = 47.0;
        p.position.z = 0.0;
    });

    let (dist, warn_at, strength) = sim
        .world_border_warning()
        .expect("a reported border must reach the accessor");
    assert!(
        (warn_at - 5.0).abs() < 1e-9,
        "the folded warning_blocks must be the packet's 5, not the default: got {warn_at}"
    );
    // Edge of a 100-diameter border centred on the origin is x = 50, so a
    // player at x = 47 is 3 blocks out. This proves the *centre and size*
    // folded too, not merely the warning band.
    assert!(
        (dist - 3.0).abs() < 1e-6,
        "x=47 inside a 100-diameter border centred on the origin is 3 blocks \
         from the edge: got {dist}"
    );
    assert!(
        (strength - 0.4).abs() < 1e-6,
        "and the strength through the real fold must equal the hand-computed \
         1 - 3/5: got {strength}"
    );
}

/// **`SessionSpawnPoint` and `SessionGameRules` reach their shell accessors**,
/// through the same real fold.
#[test]
fn folded_spawn_point_and_game_rules_reach_the_shells_own_accessors() {
    use lodestone_client::ClientEvent;

    let mut sim = Sim::new(test_config());
    ingest(&mut sim, login_event(1));

    assert!(
        sim.spawn_point().pos().is_none(),
        "precondition: no spawn reported yet"
    );
    assert_eq!(
        sim.game_rules().immediate_respawn(),
        None,
        "precondition: no game rule reported yet — `None` is 'unreported', \
         which is NOT the same as `Some(false)`"
    );

    ingest(
        &mut sim,
        ClientEvent::SpawnPositionChanged {
            dimension: "minecraft:overworld".parse().expect("valid dimension id"),
            pos: lodestone_model::BlockPos::new(12, 64, -30),
            angle: 90.0,
            pitch: 0.0,
        },
    );
    assert_eq!(
        sim.spawn_point().pos(),
        Some(lodestone_model::BlockPos::new(12, 64, -30)),
        "the folded spawn position must reach the accessor the HUD reads"
    );
}

/// The inventory avatar's walk cycle: `Sim::local_body_anim` must report a live
/// limb swing **while the camera is first-person**, which is the only mode the
/// inventory screen is ever open in.
///
/// The wrong hypothesis is computed in the same run rather than described:
/// `third_person_body_state()` is asserted to be `None` here, which is what the
/// avatar used to be fed through (and what made the walk cycle read as blocked by
/// a crate boundary). So a regression that put the `is_first_person()` early
/// return back cannot pass — the two arms disagree by construction.
///
/// A `limb_swing_amount` of exactly `0.0` before any movement is the control:
/// without it, "greater than zero after walking" is satisfied by a rig that
/// reports a constant.
#[test]
fn the_avatar_pose_carries_the_walk_cycle_in_first_person() {
    let mut sim = Sim::new(test_config());
    // Settle one tick so `body_pose` has a previous position to measure against.
    sim.step(1.0 / 20.0);

    assert!(
        sim.camera_type().is_first_person(),
        "precondition: a fresh Sim starts in first person"
    );
    assert!(
        sim.third_person_body_state().is_none(),
        "premise: the third-person reader returns None here — this is the gate \
         that made the walk cycle look unreachable"
    );
    let at_rest = sim.local_body_anim();
    assert_eq!(
        at_rest.limb_swing_amount, 0.0,
        "control: a standing player's limb swing amount is exactly zero, so the \
         assertion below is not satisfiable by a constant"
    );

    // Walk. `body_pose.tick` measures the *travelled* horizontal distance, so the
    // player has to actually move — driving the input alone would not do it.
    sim.input_mut(|i| i.set(lodestone_controller::Action::Forward, true));
    for _ in 0..10 {
        sim.step(1.0 / 20.0);
    }

    let walking = sim.local_body_anim();
    assert!(
        walking.limb_swing_amount > 0.0,
        "a walking player's avatar must have a non-zero limb swing amount, got {}",
        walking.limb_swing_amount
    );
    assert!(
        walking.limb_swing > 0.0,
        "…and the stride phase must have advanced, got {}",
        walking.limb_swing
    );
    // Still first person, so the old path is still `None`: the pose is reaching a
    // consumer the gated reader structurally could not serve.
    assert!(
        sim.third_person_body_state().is_none(),
        "the camera must not have changed mode under us"
    );
}
