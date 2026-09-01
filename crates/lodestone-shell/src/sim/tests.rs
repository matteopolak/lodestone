use std::collections::HashSet;

use super::*;
use crate::config::{Config, Mode};
use lodestone_ecs::player::SWIMMING_EYE_HEIGHT;
use lodestone_physics::UseEffects;

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
    dig_break_inputs(entry, bare_handed_tool_mining(entry), false, true, false, false)
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
    let tooled = dig_break_inputs(census::STONE, diamond_pickaxe, false, true, false, false);
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
        false,
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

/// The live-server loop (owner report): a repeated `PLAYER_POSITION` to the
/// same absolute coordinate, a fresh teleport id every time, forever. Traced
/// to `Sim::step`'s own documented ordering — "one `Update` schedule, then N
/// catch-up `GameTick` schedules, then `poll_net`/`fold_entities`/`Extract`"
/// — which means a `NetUpdate::Teleport` already sitting in the channel when
/// `step` is called is not applied to `PhysicsState` until *after* this
/// frame's `TickSet::Send` has already queued an outbound `Move` from the
/// **pre-teleport** position. `select_move_packet`'s own doc names exactly
/// this symptom ("still reports the old world's coordinates after a
/// transfer/reconfigure") and points upstream at whatever feeds `pos` —
/// this is that upstream.
///
/// A `far` target (500, 80, -500) rather than anything near the default
/// spawn, so a stale pre-teleport claim cannot coincide with it by chance.
#[test]
fn move_sent_the_same_tick_as_a_teleport_carries_the_new_position() {
    use crate::net::NetUpdate;
    let (net, actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
    sim.poll_net(); // -> Connected, zero ticks run yet.
    assert_eq!(sim.session_phase(), SessionPhase::Connected);

    let far = lodestone_client::Vec3::new(500.0, 80.0, -500.0);
    feed.send(NetUpdate::Teleport {
        pos: far,
        rotation: Rotation::new(0.0, 0.0),
        flags: lodestone_model::event::TeleportFlags {
            relative_x: false,
            relative_y: false,
            relative_z: false,
            relative_yaw: false,
            relative_pitch: false,
        },
    })
    .unwrap();

    // One tick: the teleport must be visible to *this* tick's Send, not next
    // frame's.
    sim.step(1.0 / 20.0);

    let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
    let mv = sent
        .iter()
        .find_map(|a| match a {
            ClientAction::Move { pos, .. } => Some(*pos),
            _ => None,
        })
        .expect("a connected, in-world sim must send exactly one Move this tick");

    assert!(
        (mv.x - far.x).abs() < 0.01 && (mv.y - far.y).abs() < 0.01 && (mv.z - far.z).abs() < 0.01,
        "the outbound Move sent in the same tick a teleport arrives must claim the \
         teleported position, not a stale pre-teleport one: got {mv:?}, wanted ~{far:?}"
    );
}

/// The brief's requested control on link 1 (the `relatives` bitmask): the
/// same wire rotation must produce genuinely different local player state
/// depending on whether it is flagged relative or absolute. Non-zero,
/// pairwise-distinct baseline/delta (`CLAUDE.md`'s evidence-standards
/// rule) so an accidental "always absolute" or "always relative"
/// implementation cannot coincidentally pass.
#[test]
fn teleport_relative_rotation_differs_from_absolute_rotation() {
    use crate::net::NetUpdate;

    fn teleported_rotation(relative: bool) -> (f32, f32) {
        let (net, _actions, feed) = NetClient::loopback_with_feed();
        let mut sim = Sim::new(test_config());
        sim.attach_net(net);
        feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
        sim.poll_net(); // -> Connected

        // A non-zero baseline rotation, distinct from the packet's own delta.
        sim.player_mut(|p| {
            p.yaw = 10.0;
            p.pitch = 5.0;
        });

        feed.send(NetUpdate::Teleport {
            pos: lodestone_client::Vec3::new(0.0, 0.0, 0.0),
            rotation: Rotation::new(20.0, 7.0),
            flags: lodestone_model::event::TeleportFlags {
                relative_x: false,
                relative_y: false,
                relative_z: false,
                relative_yaw: relative,
                relative_pitch: relative,
            },
        })
        .unwrap();
        sim.poll_net();

        let p = sim.player();
        (p.yaw, p.pitch)
    }

    let relative = teleported_rotation(true);
    let absolute = teleported_rotation(false);

    assert_eq!(
        relative,
        (30.0, 12.0),
        "relative_yaw/relative_pitch must add the packet's delta to the current pose"
    );
    assert_eq!(
        absolute,
        (20.0, 7.0),
        "an absolute rotation flag must overwrite, not add to, the current pose"
    );
    assert_ne!(
        relative, absolute,
        "the relatives bitmask must change the outcome -- a fix that ignores it \
         entirely would otherwise still pass"
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

/// Issue #23 (mob spawner/trial spawner): [`Sim::spawner_source`]'s own
/// island detector, matching
/// [`bell_source_tracks_connection_state_and_is_safe_before_login`]'s shape
/// and reasoning — the closure captures `Sim::spawner_spins` and the partial
/// tick, the same shape bell's own closure carries.
#[test]
fn spawner_source_tracks_connection_state_and_is_safe_before_login() {
    let mut sim = Sim::new(test_config());
    assert!(
        sim.spawner_source().is_none(),
        "no net attached at all must report no source, matching bell_source/skull_source"
    );

    let (net, _actions, _feed) = NetClient::loopback_with_feed();
    sim.attach_net(net);
    let source = sim
        .spawner_source()
        .expect("a net is attached, so a source must exist even before login completes");
    assert_eq!(
        source(glam::Vec3::ZERO),
        Vec::new(),
        "no ClientHandle has been published yet, so the closure must return \
         no spawns rather than panicking on the empty OnceLock"
    );
}

/// [`Sim::beacon_source`]'s own island detector, matching
/// [`bell_source_tracks_connection_state_and_is_safe_before_login`]'s shape.
/// Unlike bell's closure, this one carries no cloned tracker — just the game
/// tick and partial tick — so the panic-safety half is the same claim: no
/// `ClientHandle` published yet must return no spawns, not panic reading the
/// world through an empty `OnceLock`.
#[test]
fn beacon_source_tracks_connection_state_and_is_safe_before_login() {
    let mut sim = Sim::new(test_config());
    assert!(
        sim.beacon_source().is_none(),
        "no net attached at all must report no source, matching bell_source/skull_source"
    );

    let (net, _actions, _feed) = NetClient::loopback_with_feed();
    sim.attach_net(net);
    let source = sim
        .beacon_source()
        .expect("a net is attached, so a source must exist even before login completes");
    assert_eq!(
        source(glam::Vec3::ZERO),
        Vec::new(),
        "no ClientHandle has been published yet, so the closure must return \
         no spawns rather than panicking on the empty OnceLock"
    );
}

/// Issue #23 (vault): [`Sim::vault_source`]'s own island detector, matching
/// [`beacon_source_tracks_connection_state_and_is_safe_before_login`]'s shape
/// exactly — both closures capture only `game_time`/`partial_tick` and a
/// `SharedHandle`, no per-position tracker.
#[test]
fn vault_source_tracks_connection_state_and_is_safe_before_login() {
    let mut sim = Sim::new(test_config());
    assert!(
        sim.vault_source().is_none(),
        "no net attached at all must report no source, matching beacon_source/bell_source"
    );

    let (net, _actions, _feed) = NetClient::loopback_with_feed();
    sim.attach_net(net);
    let source = sim
        .vault_source()
        .expect("a net is attached, so a source must exist even before login completes");
    assert_eq!(
        source(glam::Vec3::ZERO),
        Vec::new(),
        "no ClientHandle has been published yet, so the closure must return \
         no spawns rather than panicking on the empty OnceLock"
    );
}

/// Issue #23 (brushable block): [`Sim::brushable_source`]'s own island
/// detector, matching [`vault_source_tracks_connection_state_and_is_safe_before_login`]'s
/// shape exactly — the closure captures only a `SharedHandle`, no clock at
/// all.
#[test]
fn brushable_source_tracks_connection_state_and_is_safe_before_login() {
    let mut sim = Sim::new(test_config());
    assert!(
        sim.brushable_source().is_none(),
        "no net attached at all must report no source, matching vault_source/campfire_source"
    );

    let (net, _actions, _feed) = NetClient::loopback_with_feed();
    sim.attach_net(net);
    let source = sim
        .brushable_source()
        .expect("a net is attached, so a source must exist even before login completes");
    assert_eq!(
        source(glam::Vec3::ZERO),
        Vec::new(),
        "no ClientHandle has been published yet, so the closure must return \
         no spawns rather than panicking on the empty OnceLock"
    );
}

/// Issue #23 (copper golem statue): [`Sim::copper_golem_statue_source`]'s
/// own island detector, matching
/// [`shelf_source_tracks_connection_state_and_is_safe_before_login`]'s shape.
#[test]
fn copper_golem_statue_source_tracks_connection_state_and_is_safe_before_login() {
    let mut sim = Sim::new(test_config());
    assert!(
        sim.copper_golem_statue_source().is_none(),
        "no net attached at all must report no source, matching skull_source/shelf_source"
    );

    let (net, _actions, _feed) = NetClient::loopback_with_feed();
    sim.attach_net(net);
    let source = sim
        .copper_golem_statue_source()
        .expect("a net is attached, so a source must exist even before login completes");
    assert_eq!(
        source(glam::Vec3::ZERO),
        Vec::new(),
        "no ClientHandle has been published yet, so the closure must return \
         no spawns rather than panicking on the empty OnceLock"
    );
}

/// Issue #23 (shelf): [`Sim::shelf_source`]'s own island detector, matching
/// [`brushable_source_tracks_connection_state_and_is_safe_before_login`]'s
/// shape exactly.
#[test]
fn shelf_source_tracks_connection_state_and_is_safe_before_login() {
    let mut sim = Sim::new(test_config());
    assert!(
        sim.shelf_source().is_none(),
        "no net attached at all must report no source, matching brushable_source/vault_source"
    );

    let (net, _actions, _feed) = NetClient::loopback_with_feed();
    sim.attach_net(net);
    let source = sim
        .shelf_source()
        .expect("a net is attached, so a source must exist even before login completes");
    assert_eq!(
        source(glam::Vec3::ZERO),
        Vec::new(),
        "no ClientHandle has been published yet, so the closure must return \
         no spawns rather than panicking on the empty OnceLock"
    );
}

/// Issue #23 (decorated pot): [`Sim::decorated_pot_source`]'s own island
/// detector, matching [`bell_source_tracks_connection_state_and_is_safe_before_login`]'s
/// shape and reasoning — see that test's doc for why a plain accessor check
/// is the honest scope here.
#[test]
fn decorated_pot_source_tracks_connection_state_and_is_safe_before_login() {
    let mut sim = Sim::new(test_config());
    assert!(
        sim.decorated_pot_source().is_none(),
        "no net attached at all must report no source, matching bell_source/shulker_source"
    );

    let (net, _actions, _feed) = NetClient::loopback_with_feed();
    sim.attach_net(net);
    let source = sim
        .decorated_pot_source()
        .expect("a net is attached, so a source must exist even before login completes");
    assert_eq!(
        source(glam::Vec3::ZERO),
        Vec::new(),
        "no ClientHandle has been published yet, so the closure must return \
         no spawns rather than panicking on the empty OnceLock"
    );
}

/// Issue #23 (conduit): [`Sim::conduit_source`]'s own island detector,
/// matching the bell/pot siblings above.
#[test]
fn conduit_source_tracks_connection_state_and_is_safe_before_login() {
    let mut sim = Sim::new(test_config());
    assert!(
        sim.conduit_source().is_none(),
        "no net attached at all must report no source, matching bell_source/decorated_pot_source"
    );

    let (net, _actions, _feed) = NetClient::loopback_with_feed();
    sim.attach_net(net);
    let source = sim
        .conduit_source()
        .expect("a net is attached, so a source must exist even before login completes");
    assert_eq!(
        source(glam::Vec3::ZERO),
        Vec::new(),
        "no ClientHandle has been published yet, so the closure must return \
         no spawns rather than panicking on the empty OnceLock"
    );
}

/// Issue #23 (conduit): [`Sim::step`] must actually advance
/// `Sim::conduit_ticks` once per tick while connected, not merely hold the
/// field — the same "correct function fed a constant by its producer" trap
/// this session's other four fixes hit. A tick where no conduit is anywhere
/// near the player is still observable: `Sim::step` must run without
/// panicking on the not-yet-published `ClientHandle`, matching every other
/// per-tick block-entity fold's "empty rather than a panic" contract.
#[test]
fn stepping_ticks_conduits_without_panicking_before_login() {
    let (net, _actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);

    sim.step(5.0 / 20.0);

    assert!(
        sim.tick_count() > 0,
        "ticks must still run while connecting, exactly like the movement-packet gate above"
    );
}

/// [`stepping_ticks_conduits_without_panicking_before_login`]'s shape, for
/// `SpawnerSpins::tick` — the newest per-tick gather in the same
/// `if let Some(net) = ..` block, and the one with the most inputs to get
/// wrong before login (a world read, an NBT-aware candidate scan, and a
/// distance test), so it earns its own explicit pin rather than relying on
/// the conduit test's coverage of the same guarded block.
#[test]
fn stepping_ticks_spawners_without_panicking_before_login() {
    let (net, _actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);

    sim.step(5.0 / 20.0);

    assert!(
        sim.tick_count() > 0,
        "ticks must still run while connecting, exactly like the movement-packet gate above"
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
    // The same event must also reach the display models with its full data —
    // both of them, because the HUD overlay and the inventory column are
    // separate folds of this one state and either could be the dead one.
    let icons = crate::effects::hud_icons(&sim.active_effects());
    assert_eq!(icons.len(), 1, "the HUD effect model must fold it too");
    assert_eq!(icons[0].icon, "mob_effect/levitation");
    assert!(
        !icons[0].beneficial,
        "levitation is HARMFUL, so it belongs in the overlay's lower row"
    );
    let rows = crate::effects::inventory_rows(&sim.active_effects(), &|_| None);
    assert_eq!(rows.len(), 1, "the inventory column must fold it too");
    assert_eq!(rows[0].duration, "00:10"); // 200 ticks -> 10 s

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
        always_show: false,
        pos: Vec3::new(origin.x, origin.y, origin.z),
        offset: Vec3f::new(0.1, 0.1, 0.1),
        max_speed: 0.02,
        count: 9,
        options: lodestone_model::event::ParticleOptions::None,
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
/// to be a real workload rather than an empty engine trivially satisfying
/// the assertion below — the *world*-species guard `CLAUDE.md` asks for —
/// and well under `ParticleEngine::DEFAULT_CAPACITY` (16 384) so the engine
/// does not silently drop the tail.
const HOLD_MEASUREMENT_PARTICLES: i32 = 4_000;

/// The small end of the volume check, a tenth of
/// [`HOLD_MEASUREMENT_PARTICLES`]. Measured at both this and the full count
/// — see
/// [`extract_particles_does_not_hold_the_world_guard_across_the_per_particle_work`]
/// for why the guard-acquisition count has to come out identical at both ends
/// despite the tenfold difference in particle volume.
const HOLD_MEASUREMENT_PARTICLES_SMALL: i32 = HOLD_MEASUREMENT_PARTICLES / 10;

/// The exact guard-acquisition count [`guard_acquisitions_for_extract`] must
/// report, at any particle count: [`Sim::reset_lock_holds`]'s own hold (its
/// doc explains why zeroing the counters still leaves one behind), plus
/// `extract_particles`'s `self.clock()` read, plus the two writes
/// `with_particles_unlocked` takes to move `ParticleSim` out of the `World`
/// and back. See
/// [`extract_particles_does_not_hold_the_world_guard_across_the_per_particle_work`]'s
/// own doc for why this is derived rather than guessed.
const EXTRACT_EXPECTED_HOLDS: u64 = 4;

/// The exact guard-acquisition count [`guard_acquisitions_for_prefix_shape`]
/// must report, at any particle count: [`Sim::reset_lock_holds`]'s own hold,
/// plus the single `hold_write` that wraps the entire pre-fix extract.
const PREFIX_EXPECTED_HOLDS: u64 = 2;

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
            lodestone_model::event::ParticleOptions::None,
        );
    });
    let camera = sim.camera(1.0);
    (sim, camera)
}

/// [`sim_with_particles`] at the full [`HOLD_MEASUREMENT_PARTICLES`].
fn sim_with_many_particles() -> (Sim, Camera) {
    sim_with_particles(HOLD_MEASUREMENT_PARTICLES)
}

/// [`Sim::lock_holds`]'s guard-*acquisition* count (not duration) for one
/// `extract_particles` call over `count` particles, plus how many particles
/// actually survived to be extracted — the volume half of the *world*-species
/// check, since a count measured over an empty engine would satisfy any
/// assertion trivially.
///
/// [`Sim::reset_lock_holds`] first, so the count below is this one call's,
/// not the whole session's — `Sim::new`'s own setup already takes guards of
/// its own before this function ever runs.
fn guard_acquisitions_for_extract(count: i32) -> (u64, usize) {
    let (mut sim, camera) = sim_with_particles(count);
    sim.reset_lock_holds();
    let alive = sim.extract_particles(&camera).alive;
    (sim.lock_holds().holds, alive)
}

/// As [`guard_acquisitions_for_extract`], but for the **pre-fix shape**: the
/// whole extract run inside one write guard — the exact shape
/// `Sim::extract_particles` used to be before the fix this file documents.
///
/// `light` is the offline arm (`self.net == None`), which is also what the
/// positive case measures against — see [`sim_with_particles`] — so the two
/// shapes are compared on identical inputs.
fn guard_acquisitions_for_prefix_shape(count: i32) -> (u64, usize) {
    let (mut sim, camera) = sim_with_particles(count);
    sim.reset_lock_holds();
    let alive = lodestone_ecs::hold_write(sim.ecs(), |w| {
        w.resource_mut::<ParticleSim>()
            .0
            .extract(&camera, 0.0, &|_, _, _| None)
    })
    .alive;
    (sim.lock_holds().holds, alive)
}

/// **The measurement §4.1(c) could not make, re-expressed as a counter.**
///
/// `Sim::extract_particles` was the longest `World` guard hold in the process:
/// it took the write guard by hand and held it across the whole extract *and*
/// one chunk-store lookup per live particle for light. `docs/world-unification.md`
/// bounded that structurally — "no guard spans a frame" — and said so out loud:
/// *treat the bound as structural, not measured*. A duration claim with nothing
/// measuring the duration is the species of vacuous test `CLAUDE.md` names, so
/// this used to be a ratio of **guarded time at two particle counts** instead.
///
/// That duration form was itself real — it correctly distinguished the two
/// shapes on an otherwise-idle machine — but it flaked whenever this checkout's
/// other agents were compiling: both arms are measured *sequentially*, so a
/// load spike landing between the small-count and large-count extract
/// corrupts the ratio in exactly the direction that looks like a regression.
/// Measured at 5.34x against a 3x bound on a loaded machine, standing on
/// unmodified code. Three separate agents independently re-ran it alone and
/// single-threaded, confirmed it passes there, and correctly concluded "not a
/// defect" — cost paid three times for the same non-finding.
///
/// The property under test was never actually about *time*: it is structural
/// — does the `World` guard span the per-particle loop? — and `Sim::lock_holds`
/// already answers that with a **count**, not a clock:
/// `with_particles_unlocked` (`sim.rs`) takes the guard *exactly twice* per
/// call — once to remove `ParticleSim` out of the `World`, once to put it back
/// — with the entire per-particle extract running in the gap between them,
/// under no guard at all. `extract_particles` itself takes one more, a read,
/// for `self.clock().interp_alpha` ahead of that. That count cannot be
/// inflated by scheduler noise or concurrent load the way a nanosecond figure
/// can, because acquiring and releasing a lock costs the same whether or not
/// anything else on the machine is busy, and it cannot be inflated by particle
/// volume either, because nothing about *how many* particles get processed in
/// the unguarded gap changes how many times the guard itself is taken.
///
/// The expected value is [`EXTRACT_EXPECTED_HOLDS`] rather than the "3" the
/// paragraph above adds up to, because [`Sim::reset_lock_holds`]'s own doc
/// says why: it records its *own* guard hold **after** zeroing the counters,
/// so the baseline the moment it returns is 1, not 0. Missing that the first
/// time this was written produced a failing assertion against a real,
/// deterministic count — not flakiness, just an under-counted expectation —
/// which is worth recording exactly because it is the failure mode "predict
/// the exact count" trades a duration bound's vagueness for: get the count
/// wrong and it fails **every time**, loudly, rather than most of the time,
/// quietly. So the expected count is exact and identical at 400 particles and
/// at 4,000 — [`EXTRACT_EXPECTED_HOLDS`], not "close to it" — which is the
/// sharper form of "predict the expected value, don't merely bound it"
/// `CLAUDE.md`'s *magnitude*-species note asks for.
///
/// Its negative control is
/// [`the_pre_fix_shape_of_extract_particles_fails_the_hold_bound`], which
/// reproduces the old shape (the whole extract inside *one* guard, plus the
/// same reset artifact) and must report [`PREFIX_EXPECTED_HOLDS`], not
/// [`EXTRACT_EXPECTED_HOLDS`], at both particle counts — still flat in the
/// count, but the wrong flat number, which is exactly what distinguishes "the
/// guard is taken twice, briefly, around O(1) resource moves" from "the guard
/// is taken once, around the whole call, including every particle in the loop
/// that produced this run's `alive` count".
#[test]
fn extract_particles_does_not_hold_the_world_guard_across_the_per_particle_work() {
    let (small_holds, small_alive) =
        guard_acquisitions_for_extract(HOLD_MEASUREMENT_PARTICLES_SMALL);
    let (large_holds, large_alive) = guard_acquisitions_for_extract(HOLD_MEASUREMENT_PARTICLES);

    // The *world*-species guard: the flaw in a vacuous test lives in the
    // input, not the assert. An extract over an empty engine would satisfy the
    // count below trivially, so assert the volume first — at both ends, since
    // the count is meaningless as a claim about "per-particle work" if either
    // side did no work.
    assert!(
        small_alive >= HOLD_MEASUREMENT_PARTICLES_SMALL as usize
            && large_alive >= HOLD_MEASUREMENT_PARTICLES as usize,
        "the measurement needs real volume at both ends; alive={small_alive} and {large_alive}"
    );

    // The exact, load-independent claim: precisely `EXTRACT_EXPECTED_HOLDS`
    // guard acquisitions — the reset call's own hold, a clock read, then
    // remove `ParticleSim` and put it back — regardless of how many particles
    // sat in the unguarded gap between the last two.
    assert_eq!(
        small_holds, EXTRACT_EXPECTED_HOLDS,
        "extract_particles over {small_alive} particles took {small_holds} World guard \
         acquisitions, not the expected {EXTRACT_EXPECTED_HOLDS} (reset's own hold, a clock \
         read, remove ParticleSim, put it back)"
    );
    assert_eq!(
        large_holds, EXTRACT_EXPECTED_HOLDS,
        "extract_particles over {large_alive} particles took {large_holds} World guard \
         acquisitions, not the expected {EXTRACT_EXPECTED_HOLDS} — a 10x particle count must \
         not change how many times the guard is acquired, or the guard has started spanning \
         the per-particle work again"
    );
}

/// The negative control for the count above, and the reason it is evidence
/// rather than decoration: the *pre-fix shape* — extract run inside the write
/// guard — must report the wrong count, measured by the same instrument.
///
/// This is deliberately hand-written rather than a switch on `Sim`: a test
/// switch would have to survive in production code, and what needs proving is
/// that the detector distinguishes two shapes, not that a flag works.
///
/// Unlike the duration form this replaces, this control's expectation does not
/// depend on load or timing at all: `lodestone_ecs::hold_write` wraps the
/// *entire* extract in one guard by construction (see this function's own
/// body), so the guard-acquisition count is exactly
/// [`PREFIX_EXPECTED_HOLDS`] — deterministically, on any machine, at any
/// particle count — never "close to it" or "usually that".
#[test]
fn the_pre_fix_shape_of_extract_particles_fails_the_hold_bound() {
    let (small_holds, small_alive) =
        guard_acquisitions_for_prefix_shape(HOLD_MEASUREMENT_PARTICLES_SMALL);
    let (large_holds, large_alive) =
        guard_acquisitions_for_prefix_shape(HOLD_MEASUREMENT_PARTICLES);

    assert!(
        small_alive >= HOLD_MEASUREMENT_PARTICLES_SMALL as usize
            && large_alive >= HOLD_MEASUREMENT_PARTICLES as usize,
        "same input volume as the positive case; alive={small_alive} and {large_alive}"
    );

    assert_eq!(
        small_holds, PREFIX_EXPECTED_HOLDS,
        "the pre-fix shape must take exactly {PREFIX_EXPECTED_HOLDS} World guards over \
         {small_alive} particles (reset's own hold, plus the whole extract wrapped in one \
         `hold_write`), got {small_holds}"
    );
    assert_eq!(
        large_holds, PREFIX_EXPECTED_HOLDS,
        "the pre-fix shape must take exactly {PREFIX_EXPECTED_HOLDS} World guards over \
         {large_alive} particles, got {large_holds}"
    );
    assert_ne!(
        small_holds, EXTRACT_EXPECTED_HOLDS,
        "the detector must fire on the shape it exists to reject: this control's guard count \
         must disagree with the correct shape's count of {EXTRACT_EXPECTED_HOLDS} in \
         `extract_particles_does_not_hold_the_world_guard_across_the_per_particle_work`, or \
         that bound is not discriminating"
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
    let started = crate::platform::Instant::now();
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

/// The other half of `ClientLevel.doAddParticle`'s filter: `options.particles`.
///
/// The pair is chosen so both arms are **deterministic**.
/// `Particles::particle_level_permits` transcribes
/// `calculateParticleLevel`, which is probabilistic for `DECREASED` (one spawn
/// in three is folded down to `MINIMAL`) but not for the other two: `ALL` never
/// folds, and `MINIMAL` is lifted only by the always-show flag, which this
/// fixture deliberately leaves clear. So `All` -> 9 and `Minimal` -> 0 are
/// exact counts, and a `Decreased` arm would need a statistical bound to say
/// anything — deliberately not asserted here rather than asserted loosely.
///
/// Both arms feed the **same** event to the **same** `Sim`, differing only in
/// the pushed level, so the second arm is the control for the first: it proves
/// the count is attributable to the option rather than to anything else about
/// the fixture.
#[test]
fn the_particles_option_gates_the_spawn_and_all_is_not_a_no_op() {
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

    let origin = sim.player().position;
    let burst = |feed: &std::sync::mpsc::SyncSender<NetUpdate>| {
        feed.send(NetUpdate::Particles {
            kind: "flame".into(),
            long_distance: false,
            always_show: false,
            pos: Vec3::new(origin.x, origin.y, origin.z),
            offset: Vec3f::new(0.1, 0.1, 0.1),
            max_speed: 0.02,
            count: 9,
            options: lodestone_model::event::ParticleOptions::None,
        })
        .unwrap();
    };

    sim.set_particle_level(crate::config::ParticleLevel::Minimal);
    burst(&feed);
    sim.poll_net();
    assert_eq!(
        sim.particles_mut(|p| p.engine_mut().particles().len()),
        0,
        "Minimal must drop a nearby, non-override burst entirely — \
         `doAddParticle` spawns only when the folded level is not MINIMAL"
    );

    sim.set_particle_level(crate::config::ParticleLevel::All);
    burst(&feed);
    sim.poll_net();
    assert_eq!(
        sim.particles_mut(|p| p.engine_mut().particles().len()),
        9,
        "control failed to fail: All must spawn the same burst the Minimal arm \
         dropped, or the zero above says nothing about the option"
    );
}

/// `alwaysShow` — the second bool on `ClientboundLevelParticlesPacket`, which
/// was decoded and then dropped: `ClientEvent::Particles` did not carry it, so
/// `net_apply.rs` passed a literal `false` and the **Minimal** setting deleted
/// every packet particle that did not also set `overrideLimiter`.
///
/// The rule is a *reprieve*, not an exemption, which is what makes this gate's
/// shape unusual. `ClientLevel.calculateParticleLevel` lifts `MINIMAL` to
/// `DECREASED` one time in ten, and `DECREASED` folds back down one time in
/// three, so an always-show burst on `Minimal` survives with probability
/// `1/10 x 2/3 = 1/15`. A single send therefore proves nothing in either
/// direction and only a count over many sends can separate the hypotheses:
///
/// | hypothesis | spawns out of `SENDS` bursts |
/// |---|---|
/// | the flag never reaches `particle_level_permits` (the bug) | exactly 0 |
/// | the flag is an *exemption* (the plausible wrong port) | exactly `SENDS` |
/// | vanilla's reprieve | ~`SENDS / 15` |
///
/// `SENDS = 900` puts the expected count at 60 and makes a zero result
/// impossible in practice — `(14/15)^900` is about `1e-27` — while the
/// exemption hypothesis is separated by a factor of fifteen, far outside any
/// binomial spread. The bounds below are therefore wide on purpose: they are
/// there to tell three hypotheses apart, not to pin a number.
///
/// The `always_show: false` arm is the control, and it is *exact*: with the
/// flag clear the fold is deterministic and no burst may survive at all. It
/// runs on the same `Sim`, at the same level, with the same one-particle
/// burst, so the only difference between the two arms is the field under test.
#[test]
fn always_show_gives_a_minimal_setting_particle_a_reprieve_and_not_an_exemption() {
    use crate::net::NetUpdate;
    use lodestone_client::Vec3;
    use lodestone_particle::Sheet;

    const SENDS: usize = 900;

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
    sim.set_particle_level(crate::config::ParticleLevel::Minimal);

    let origin = sim.player().position;
    // One particle per burst and a zero lifetime is not available, so the
    // engine is drained between sends instead: the count under test is "how
    // many bursts got through", and a burst that spawns leaves exactly one
    // particle behind.
    // Each burst carries `count: 1` with no offset, so a burst that survives
    // the filter leaves exactly one particle behind and the engine's population
    // *is* the number that got through. Queued in one batch and drained with a
    // single `poll_net` rather than one step per send, which is the whole
    // difference between a two-second gate and a half-minute one; the relay
    // channel holds 1024, comfortably above `SENDS`.
    let mut survivors = |feed: &std::sync::mpsc::SyncSender<NetUpdate>,
                         sim: &mut Sim,
                         always_show: bool| {
        for _ in 0..SENDS {
            feed.send(NetUpdate::Particles {
                kind: "flame".into(),
                // Clear, so nothing bypasses the level filter by the other
                // route: `overrideLimiter` skips the particle-level test
                // entirely, and with it set this gate would pass whatever
                // `always_show` did.
                long_distance: false,
                always_show,
                pos: Vec3::new(origin.x, origin.y, origin.z),
                offset: Vec3f::new(0.0, 0.0, 0.0),
                max_speed: 0.0,
                count: 1,
                options: lodestone_model::event::ParticleOptions::None,
            })
            .unwrap();
        }
        sim.poll_net();
        sim.particles_mut(|p| {
            let n = p.engine_mut().particles().len();
            p.engine_mut().clear();
            n
        })
    };

    let with_flag = survivors(&feed, &mut sim, true);
    let without_flag = survivors(&feed, &mut sim, false);

    assert_eq!(
        without_flag, 0,
        "control: with always_show clear, Minimal must drop every one of {SENDS} \
         non-override bursts — if this is non-zero the level filter is not \
         running at all and the other arm says nothing"
    );
    assert!(
        with_flag > 0,
        "always_show never reached particle_level_permits: {SENDS} bursts and not \
         one survived, where vanilla's one-in-fifteen reprieve makes zero a \
         1e-27 event"
    );
    assert!(
        with_flag < SENDS / 2,
        "always_show is being treated as an exemption rather than a reprieve: \
         {with_flag} of {SENDS} bursts survived, where vanilla's fold predicts \
         about {}",
        SENDS / 15
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
        always_show: false,
        pos: far,
        offset: Vec3f::new(0.0, 0.0, 0.0),
        max_speed: 0.0,
        count: 3,
        options: lodestone_model::event::ParticleOptions::None,
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
        always_show: false,
        pos: far,
        offset: Vec3f::new(0.0, 0.0, 0.0),
        max_speed: 0.0,
        count: 3,
        options: lodestone_model::event::ParticleOptions::None,
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
        SessionPhase::Ended(end) => {
            let reason = end.plain();
            assert!(reason.contains("Server closed"), "reason lost: {reason}");
            assert_eq!(
                end.kind,
                crate::sim::SessionEndKind::Disconnected,
                "a server-sent disconnect is not a client-side failure"
            );
            assert!(
                !reason.starts_with("disconnected: "),
                "the prefix was ours, not vanilla's, and it is gone: {reason}"
            );
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
        SessionPhase::Ended(end) => {
            let reason = end.plain();
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
        SessionPhase::Ended(end) => {
            let reason = end.plain();
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
        SessionPhase::Ended(end) => {
            let reason = end.plain();
            assert!(reason.contains("connection refused"), "got {reason}");
            assert_eq!(
                end.kind,
                crate::sim::SessionEndKind::Failed,
                "a net error is a client-side failure, not a server disconnect — \
                 that distinction is what gives the screen the right title"
            );
        }
        other => panic!("expected Ended, got {other:?}"),
    }
}

/// The absence control for the two tests below: this is the exact defect
/// report ("if i open to lan, then open it a second time it kicks me and
/// says lan is already up") reproduced directly at the `Sim` boundary, using
/// the *old* wrong variant a second publish attempt used to be reported
/// through. It must still end the session — proving the detector the next
/// test relies on (`session_phase != Ended`) would actually have caught the
/// bug, rather than merely asserting the negative and hoping the mechanism
/// works.
#[test]
fn net_update_error_would_have_caught_the_old_already_published_kick() {
    use crate::net::NetUpdate;
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 7 }).unwrap();
    ingest(&mut sim, login_event(7));
    sim.poll_net();
    assert_eq!(sim.session_phase(), SessionPhase::Connected, "premise");

    // The literal message `net.rs` sent for this case before
    // `NetUpdate::LanPublishError` existed.
    feed.send(NetUpdate::Error(
        "open to LAN: this world is already published".into(),
    ))
    .unwrap();
    sim.poll_net();
    assert!(
        matches!(sim.session_phase(), SessionPhase::Ended(_)),
        "control failed: NetUpdate::Error must still end a session, or the \
         positive test below is not measuring anything"
    );
}

/// The fix (issue #535's "kicks me" report): a second Open to LAN press —
/// `IntegratedServer::publish` returning `AlreadyExists` — must reach the
/// player as one more chat line on a session that is still alive, never as a
/// disconnect. See `NetUpdate::LanPublishError`'s own doc for the full
/// button → net thread → publish handler → error path trace, and the control
/// immediately above for evidence the assertion below would actually fail if
/// the old code path (`NetUpdate::Error`) were used instead.
#[test]
fn a_second_lan_publish_reports_a_chat_error_without_ending_the_session() {
    use crate::net::NetUpdate;
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 7 }).unwrap();
    ingest(&mut sim, login_event(7));
    sim.poll_net();
    assert_eq!(
        sim.session_phase(),
        SessionPhase::Connected,
        "premise: a live session to publish from"
    );

    // The first publish succeeds.
    feed.send(NetUpdate::LanOpened { port: 25565 }).unwrap();
    sim.poll_net();
    assert_eq!(sim.session_phase(), SessionPhase::Connected);
    assert!(
        sim.is_lan_published(),
        "premise: the world really is published now, or the second call \
         below is not the scenario this test claims to cover"
    );

    // The second publish — a second press of the same button — fails
    // server-side, and must not disturb the session at all.
    feed.send(NetUpdate::LanPublishError(
        "open to LAN: this world is already published".into(),
    ))
    .unwrap();
    sim.poll_net();
    assert_eq!(
        sim.session_phase(),
        SessionPhase::Connected,
        "a second publish attempt must never disconnect an otherwise \
         healthy session — this is the discriminating assertion, not the \
         chat line below"
    );
    let chat = sim.recent_chat(10);
    assert!(
        chat.iter()
            .any(|(line, _)| line.contains("already published")),
        "the failure must still reach the player, through the ordinary chat \
         path: {chat:?}"
    );
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
        // A system message carries no signature to check, so the driver's
        // verdict is `false` — see `NetUpdate::Chat::verified`, which is
        // "unproven", not "forged".
        verified: false,
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

/// The driver's signature verdict decides a player message's `MessageTrust`,
/// and both answers are asserted — the discriminating pair, because a stamp
/// that ignores the flag agrees with one arm by construction.
///
/// This is the gate for a defect of the "a correct consumer fed a constant by
/// its producer" shape: `MessageTrust` had three variants and a real signature
/// check ran in the client driver, while `net.rs`'s router matched
/// `ClientEvent::Chat` with `..` and dropped `ack`, so `net_apply` stamped
/// **every** player message `NotSecure`. Under that code the `verified: true`
/// arm below reads `NotSecure` and fails; nothing about the `false` arm would
/// have noticed.
///
/// It drives the real `NetUpdate::Chat` through the real `Sim::poll_net` and
/// reads the stored `ChatEntry` back off the real `SessionChat` component, so
/// it covers the fold rather than a restatement of it. It does **not** cover
/// `net.rs`'s `forward` — that maps `ClientEvent` to `NetUpdate` one layer up.
#[test]
fn a_verified_player_message_is_stored_secure_and_an_unverified_one_is_not() {
    use crate::net::NetUpdate;
    use lodestone_game::chat::{ChatEntry, MessageTrust};

    let mut seen = Vec::new();
    for (verified, expected) in [(true, MessageTrust::Secure), (false, MessageTrust::NotSecure)] {
        let (net, _actions, feed) = NetClient::loopback_with_feed();
        let mut sim = Sim::new(test_config());
        sim.attach_net(net);
        feed.send(NetUpdate::Chat {
            text: lodestone_model::Text::literal("hi"),
            player: true,
            sender: None,
            verified,
        })
        .unwrap();
        sim.poll_net();

        let local = sim.local_entity();
        let trust = sim.read(|w| {
            w.get::<lodestone_ecs::session::SessionChat>(local)
                .and_then(|chat| match chat.0.feed().iter().next_back() {
                    Some(ChatEntry::Player { trust, .. }) => Some(*trust),
                    _ => None,
                })
        });
        seen.push((verified, trust));
        assert_eq!(
            trust,
            Some(expected),
            "verified = {verified} must store {expected:?}"
        );
    }
    // Collected and asserted on the collection so a regression reports both
    // arms rather than aborting on the first: the claim is that the two
    // *differ*, which one arm alone cannot express.
    assert_ne!(
        seen[0].1, seen[1].1,
        "the two arms must not coincide, or the flag is being ignored: {seen:?}"
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
        // A system message carries no signature to check, so the driver's
        // verdict is `false` — see `NetUpdate::Chat::verified`, which is
        // "unproven", not "forged".
        verified: false,
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
        // A system message carries no signature to check, so the driver's
        // verdict is `false` — see `NetUpdate::Chat::verified`, which is
        // "unproven", not "forged".
        verified: false,
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
    assert_eq!(crate::overlay::spans_text(&title), "Welcome");
    assert_eq!(
        subtitle.as_deref().map(crate::overlay::spans_text),
        Some("to the server".to_owned())
    );

    // A clear packet must empty the overlay again.
    feed.send(NetUpdate::TitleEvent(ClientEvent::TitlesCleared {
        reset_times: false,
    }))
    .unwrap();
    sim.poll_net();
    assert!(sim.title_overlay().is_none());
}

/// A **hex** colour survives `title_overlay` and `action_bar_overlay`.
///
/// These two accessors flattened with `Text::to_legacy_string()`, and that call was
/// where a modern server's title colour died: the sixteen named colours have `§`
/// codes and the font layer applies codes at draw time, so they survived a
/// `String` — `TextColor::Rgb` has none. Hex is therefore the *only* input on which
/// "flattens to a legacy string" and "hands over spans" differ, which is why a
/// named colour here would be the coincident-input species of vacuous test.
///
/// The three values are pairwise distinct so a title/subtitle/action-bar mix-up
/// cannot pass, and the mismatches are collected so one bad arm does not hide the
/// other two.
#[test]
fn a_hex_colour_survives_the_title_and_action_bar_accessors() {
    use crate::net::NetUpdate;
    use lodestone_model::{ClientEvent, Text, TextColor, TextStyle};

    // Not multiples of 0x11, not near a named colour, all different.
    const TITLE: u32 = 0x001f_2e3d;
    const SUBTITLE: u32 = 0x004a_6b8c;
    const ACTION: u32 = 0x00c4_7b19;

    let hex = |text: &str, rgb: u32| Text {
        style: TextStyle {
            font: None,
            color: Some(TextColor::Rgb(rgb)),
            ..TextStyle::default()
        },
        ..Text::literal(text)
    };

    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    feed.send(NetUpdate::TitleEvent(ClientEvent::TitleText {
        text: hex("T", TITLE),
    }))
    .unwrap();
    feed.send(NetUpdate::TitleEvent(ClientEvent::SubtitleText {
        text: hex("S", SUBTITLE),
    }))
    .unwrap();
    feed.send(NetUpdate::ActionBar(hex("A", ACTION))).unwrap();
    sim.poll_net();

    let (title, subtitle, _) = sim
        .title_overlay()
        .expect("a server-sent title must reach the HUD accessor");
    let (action, _) = sim
        .action_bar_overlay()
        .expect("a GameInfo message must reach the action-bar accessor");
    let subtitle = subtitle.expect("the subtitle must reach the accessor too");

    let mut wrong = Vec::new();
    for (name, spans, want) in [
        ("title", &title, TITLE),
        ("subtitle", &subtitle, SUBTITLE),
        ("action_bar", &action, ACTION),
    ] {
        let got: Vec<Option<TextColor>> = spans.iter().map(|s| s.style.color).collect();
        if got != vec![Some(TextColor::Rgb(want))] {
            wrong.push(format!("{name}: want Rgb(#{want:06x}) throughout, got {got:?}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:?}");
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
    assert_eq!(crate::overlay::spans_text(&text), "Boss incoming");
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
        "Hud.java: a freshly triggered highlight is at full opacity, no fade-in"
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
         timer (Hud.java's item-and-hover-name identity check, not slot \
         equality) — alpha went from {faded_alpha} to {after_switch}, which only \
         happens if it retriggered"
    );
}

/// The seam this issue landed (#656): [`Sim::held_item_overlay_spans`] —
/// which `app/redraw.rs`'s `hud_frame.held_item_spans =
/// self.sim.held_item_overlay_spans()` reads every frame, mirroring
/// `hud_frame.held_item = self.sim.held_item_overlay()` right above it —
/// must carry a held item's hex-coloured custom name all the way to a HUD
/// **vertex colour**, not merely return spans from the accessor.
///
/// Builds the item through the real production path a live server's
/// `set_container_slot`/`set_container_content` would drive
/// (`ClientEvent::InventorySlotChanged` → `Menus::apply` →
/// `lodestone_ecs::session::tick_hud_overlays` →
/// `HeldItemHighlight::set_spans`, from `styled_hover_name_spans`), then
/// feeds the accessor's own output into `HudFrame`/`HudGeometry::build`
/// exactly the shape `app/redraw.rs` assembles, and checks three
/// pairwise-distinct vertex RGBs (hex, an inline `§` code and a named
/// colour — the same three-clause shape `hud::tests::
/// held_item_spans_carry_hex_named_and_inline_legacy_colour_to_distinct_vertices`
/// uses, so a fixture with only named colours cannot hide the hex-drop bug).
///
/// The control repeats the *same* real item through the legacy
/// `Sim::held_item_overlay`/`HudFrame::held_item` path
/// (`styled_hover_name`'s `Text::to_legacy_string`) and requires the hex
/// colour to be lost there — proving the positive assertion above is
/// measuring the seam this test exists for, not a coincidence.
#[test]
fn held_item_overlay_spans_carry_hex_colour_from_a_real_item_to_a_vertex() {
    use lodestone_model::text::{Text, TextColor, TextContent, TextStyle};
    use lodestone_model::{Identifier, ItemComponents, ItemStack as ModelItemStack};

    let hex = Text {
        content: TextContent::Literal("Hex".to_string()),
        style: TextStyle {
            font: None,
            color: Some(TextColor::Rgb(0x1a_2b3c)),
            ..TextStyle::default()
        },
        ..Text::default()
    };
    // The inline convention: a server-authored custom name whose colour
    // lives inside the literal text as a `§c` code rather than as a
    // component-level style.
    let inline_legacy = Text::literal("\u{00a7}cRed");
    let named = Text {
        content: TextContent::Literal("Gray".to_string()),
        style: TextStyle {
            font: None,
            color: Some(TextColor::Gray),
            ..TextStyle::default()
        },
        ..Text::default()
    };
    let custom_name = Text {
        extra: vec![hex, inline_legacy, named],
        ..Text::default()
    };

    let item_id: Identifier = "minecraft:diamond_sword".parse().expect("valid item id");
    let mut components = ItemComponents::default();
    components.custom_name = Some(custom_name);
    let wire_stack = ModelItemStack {
        item: item_id,
        count: 1,
        components,
    };

    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    let local = sim.local;
    sim.write(|w| {
        if let Some(mut menus) = w.get_mut::<lodestone_ecs::SessionMenus>(local) {
            menus.0.apply(&lodestone_model::ClientEvent::InventorySlotChanged {
                slot: 0,
                item: Some(wire_stack),
            });
        }
    });
    sim.step(1.0 / 20.0);

    let (spans, alpha) = sim
        .held_item_overlay_spans()
        .expect("a hex-coloured custom name must still show a held-item overlay");
    assert!(alpha > 0.0, "a freshly triggered highlight is at full opacity");

    let stats = DebugStats::default();
    let geo = crate::hud::HudGeometry::build(
        &crate::hud::HudFrame {
            crosshair: false,
            show_debug: false,
            held_item_spans: Some((spans, alpha)),
            ..crate::hud::HudFrame::new(&stats)
        },
        640,
        480,
    );
    assert!(
        geo.vertex_count() > 0,
        "sanity: the label must draw something at all"
    );

    let byte = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
    let has_colour = |verts: &[f32], rgb: (u8, u8, u8)| {
        verts
            .chunks_exact(6)
            .any(|v| (byte(v[2]), byte(v[3]), byte(v[4])) == rgb)
    };
    let expected = [
        ("hex", (0x1a_u8, 0x2b_u8, 0x3c_u8)),
        ("inline §c", (0xff_u8, 0x55_u8, 0x55_u8)),
        ("named gray", (0xaa_u8, 0xaa_u8, 0xaa_u8)),
    ];
    let missing: Vec<&str> = expected
        .iter()
        .filter(|(_, rgb)| !has_colour(&geo.verts, *rgb))
        .map(|(name, _)| *name)
        .collect();
    assert!(
        missing.is_empty(),
        "these colours never reached a vertex: {missing:?} (full expected set: {expected:?})"
    );

    // Control: the same real held item, but through the lossy
    // `Sim::held_item_overlay`/`HudFrame::held_item` path — must lose the
    // hex colour, or the assertion above proves nothing about which field
    // actually carries it.
    let (legacy_name, legacy_alpha) = sim
        .held_item_overlay()
        .expect("control: the legacy accessor must also see the same held item");
    let legacy_geo = crate::hud::HudGeometry::build(
        &crate::hud::HudFrame {
            crosshair: false,
            show_debug: false,
            held_item: Some((legacy_name, legacy_alpha)),
            ..crate::hud::HudFrame::new(&stats)
        },
        640,
        480,
    );
    assert!(
        !has_colour(&legacy_geo.verts, (0x1a, 0x2b, 0x3c)),
        "control failed: the legacy `held_item` path was expected to lose the hex \
         colour (that is the bug #656 tracks), but it drew it anyway — this test's \
         premise is wrong"
    );
}

/// `app/redraw.rs`'s per-frame render loop is built on live GPU/window
/// state, so no unit test in this crate can call it directly — the same
/// constraint `menu::nav::tests::
/// app_rs_still_threads_every_chat_option_into_the_hud_frame` and
/// `redraw_rs_still_pushes_the_glint_options_to_all_three_sites` work around
/// by grepping that file's own source text instead. This is the same
/// technique for issue #656's held-item seam: `Sim::held_item_overlay_spans`
/// (`sim/session.rs`) reaching `HudFrame::held_item_spans` at the app-wiring
/// layer, mirroring the pre-existing legacy `held_item_overlay` →
/// `HudFrame::held_item` line right above it.
///
/// This lives in `sim/tests.rs` rather than beside the line it checks —
/// `include_str!`-ing a file from *within itself* would make the assertion
/// tautological, since the search string would always be present as the
/// literal argument to this very `.contains()` call. Living in a different
/// file (as the `chat_opts`/glint-options precedents already do) is what
/// keeps the check meaningful.
///
/// `held_item_overlay_spans_carry_hex_colour_from_a_real_item_to_a_vertex`
/// (just above) proves the accessor's own output reaches a vertex colour;
/// this proves the line that hands that output to `HudFrame` in the first
/// place is still present, so the two together cover the whole seam.
#[test]
fn redraw_rs_still_forwards_held_item_overlay_spans_to_the_hud_frame() {
    let src = include_str!("../app/redraw.rs");
    assert!(
        src.contains("hud_frame.held_item_spans = self.sim.held_item_overlay_spans();"),
        "app/redraw.rs no longer forwards `Sim::held_item_overlay_spans` into \
         `HudFrame::held_item_spans` — the held-item label is back to losing hex \
         colours, with nothing else in this crate able to see it because the real \
         draw loop cannot be unit tested"
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
                    chat_session: None,
                    list_order: None,
                    hat_visible: None,
                },
                PlayerListEntry {
                    uuid: alice,
                    name: Some("Alice".into()),
                    game_mode: Some(GameMode::Survival),
                    latency: Some(12),
                    display_name: Some(Text::literal("Alice the Brave")),
                    listed: Some(true),
                    properties: None,
                    chat_session: None,
                    list_order: None,
                    hat_visible: None,
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

/// `Sim::tick_nearby_entities` must resolve a neighbour's real scoreboard team
/// into its `NearbyEntity::collision_rule`/`allied`, not leave every neighbour
/// at [`lodestone_physics::push::NearbyEntity::living`]'s `Always`/`false`
/// default forever — the defect the owner reported (a vanilla client refusing
/// to be pushed while ours accepted it) traces to exactly this: production had
/// zero readers of a non-default `CollisionRule` before this test, because the
/// only constructor `tick_nearby_entities` ever called was `::living`.
///
/// Two neighbours, not one, so the gate can be told from "every neighbour
/// reads as `Never`" — a team-name holder that never resolves would default
/// every neighbour to `Always` (see `tick_nearby_entities`'s own comment on
/// why an unresolved holder keeps the safe default), and a broken resolver
/// that always returns `Never` would be caught by Carol having no team and
/// still reading `Always`.
///
/// This does not exercise [`NearbyEntities::self_collision_rule`] (the local
/// player's *own* team) — that resolution goes through `NetClient::local_uuid`,
/// which `NetClient::loopback_with_feed` never publishes (it is a private
/// field this test cannot reach without touching `net.rs`), so
/// `self_collision_rule` reads its safe `Always` default here. The neighbour
/// half is the half the owner's report is actually about: a real remote
/// player pushing *us*.
#[test]
fn tick_nearby_entities_resolves_a_neighbours_scoreboard_team() {
    use lodestone_model::event::{TeamAction, TeamParameters, Visibility};
    use lodestone_model::{ClientEvent, GameMode, PlayerListEntry, Text};
    use uuid::Uuid;

    let mut sim = Sim::new(test_config());
    let feet = sim.player().position;

    let bob = Uuid::from_u128(101);
    let carol = Uuid::from_u128(102);

    // The tab list carries the account name a scoreboard team's member list
    // actually keys on (`Player.getScoreboardName()`) — see
    // `crate::sim::collide::scoreboard_holder`.
    ingest(
        &mut sim,
        ClientEvent::PlayerListUpdate {
            entries: vec![
                PlayerListEntry {
                    uuid: bob,
                    name: Some("Bob".into()),
                    game_mode: Some(GameMode::Survival),
                    latency: Some(20),
                    display_name: None,
                    listed: Some(true),
                    properties: None,
                    chat_session: None,
                    list_order: None,
                    hat_visible: None,
                },
                PlayerListEntry {
                    uuid: carol,
                    name: Some("Carol".into()),
                    game_mode: Some(GameMode::Survival),
                    latency: Some(20),
                    display_name: None,
                    listed: Some(true),
                    properties: None,
                    chat_session: None,
                    list_order: None,
                    hat_visible: None,
                },
            ],
        },
    );

    // A real server's `/team add red` + `/scoreboard teams option red
    // collisionRule never` + `/team join red Bob` — Bob is on a team whose
    // rule forbids the push outright; Carol has no team at all, exactly the
    // discriminating pair the task needs: one forbidden, one allowed, so a
    // resolver that ignores the team entirely (leaving everyone `Always`)
    // fails on Bob, and a resolver that is simply broken in the other
    // direction (everyone reads `Never`) fails on Carol.
    ingest(
        &mut sim,
        ClientEvent::TeamUpdate {
            name: "red".into(),
            action: TeamAction::Create {
                params: Box::new(TeamParameters {
                    display_name: Text::literal("Red"),
                    prefix: Text::literal(""),
                    suffix: Text::literal(""),
                    name_tag_visibility: Visibility::Always,
                    collision_rule: lodestone_model::CollisionRule::Never,
                    color: None,
                    friendly_fire: true,
                    see_friendly_invisibles: true,
                }),
                members: vec!["Bob".into()],
            },
        },
    );

    for (entity_id, uuid) in [(9001, bob), (9002, carol)] {
        ingest(
            &mut sim,
            ClientEvent::EntitySpawned {
                entity_id,
                uuid: Some(uuid),
                entity_type: "minecraft:player".parse().expect("valid entity type key"),
                pos: lodestone_model::Vec3::new(feet.x + 1.0, feet.y, feet.z),
                rotation: Rotation::new(0.0, 0.0),
                velocity: None,
            },
        );
    }

    let nearby = sim.tick_nearby_entities();
    assert_eq!(
        nearby.list.len(),
        2,
        "both Bob and Carol must be in range and pass the push census"
    );

    let never_count = nearby
        .list
        .iter()
        .filter(|n| n.collision_rule == lodestone_physics::push::CollisionRule::Never)
        .count();
    let always_count = nearby
        .list
        .iter()
        .filter(|n| n.collision_rule == lodestone_physics::push::CollisionRule::Always)
        .count();
    assert_eq!(
        never_count, 1,
        "exactly Bob's NearbyEntity must carry his team's Never rule"
    );
    assert_eq!(
        always_count, 1,
        "Carol has no team, so hers must keep the Always default — proving \
         the resolver is not just returning Never unconditionally"
    );
    assert!(
        nearby.list.iter().all(|n| !n.allied),
        "the local player has no team in this harness, so `ownTeam != null` \
         must veto `allied` for every neighbour regardless of their own team"
    );
}

#[test]
fn tick_nearby_entities_keeps_a_boat_as_a_hard_collider_without_making_it_a_crowd_pusher() {
    let mut sim = Sim::new(test_config());
    let feet = sim.player().position;
    ingest(
        &mut sim,
        lodestone_model::ClientEvent::EntitySpawned {
            entity_id: 9010,
            uuid: None,
            entity_type: "minecraft:oak_boat".parse().expect("valid boat key"),
            pos: lodestone_model::Vec3::new(feet.x + 1.0, feet.y, feet.z),
            rotation: Rotation::new(0.0, 0.0),
            velocity: None,
        },
    );

    let nearby = sim.tick_nearby_entities();
    assert_eq!(nearby.list.len(), 1, "the non-pushing boat must not be filtered out");
    assert!(nearby.list[0].collidable);
    assert!(!nearby.list[0].pushes_players);
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

/// The owner's own bug report: punching the air must put a `SwingArm` on the
/// wire, not just animate the local arm. `begin_attack_live_swings_on_a_miss`
/// above already proved the *local* half; it used no net connection at all
/// (deliberately, per its own doc) and so could not have caught this — the
/// wire send and the local animation were, before this fix, two different
/// calls (`ActionQueue`'s `SwingArm` vs `Sim::swing_hand` directly), and only
/// one of them was reached on a miss. A client with no `SwingArm` on the wire
/// here is exactly the reported defect: other players never see the swing.
#[test]
fn begin_attack_live_sends_swing_arm_on_the_wire_on_a_miss() {
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);
    assert!(sim.target().is_none(), "precondition: no block targeted");
    assert!(sim.entity_target().is_none(), "precondition: no entity targeted");

    sim.begin_attack_live();

    let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
    assert!(
        sent.iter()
            .any(|a| matches!(a, ClientAction::SwingArm { hand: Hand::Main })),
        "a live miss must send a main-hand SwingArm over the wire so other \
         players see it, got {sent:?}"
    );
}

/// The `ENTITY` arm has the identical defect, one hop earlier: `attack_entity`
/// sends `InteractEntity { interaction: Attack, .. }` but never `SwingArm`
/// itself, and `begin_attack_live` used to reach only the local-only
/// `Sim::swing_hand` afterward. Vanilla's own animation call (`player.swing`
/// in `Minecraft.startAttack`) is unconditional and outside the switch, so it
/// covers `ENTITY` too — the attack packet does not carry the swing.
#[test]
fn begin_attack_live_sends_swing_arm_on_the_wire_on_an_entity_hit() {
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);
    sim.write(|w| w.resource_mut::<EntityRayTarget>().0 = Some(42));

    sim.begin_attack_live();

    let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
    let attack = sent.iter().position(|a| {
        matches!(
            a,
            ClientAction::InteractEntity {
                interaction: EntityInteraction::Attack,
                ..
            }
        )
    });
    let swing = sent
        .iter()
        .position(|a| matches!(a, ClientAction::SwingArm { hand: Hand::Main }));
    assert!(
        attack.is_some(),
        "control precondition: the attack packet itself must still be sent, \
         or the absence below is just an unwired Sim — got {sent:?}"
    );
    assert!(
        swing.is_some(),
        "attacking an entity must also send a main-hand SwingArm over the \
         wire so other players see the swing, got {sent:?}"
    );
    assert!(
        attack < swing,
        "vanilla's `MultiPlayerGameMode.attack` sends the attack packet, and \
         only afterward does `Minecraft.startAttack`'s unconditional \
         `player.swing(...)` send the swing — got {sent:?}"
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

/// Issue #613: `ClientAction::SpectatorAction` had zero producers anywhere.
/// `Minecraft.startAttack`'s spectator branch is checked *before* any item or
/// hit-result logic — [`begin_attack_live_prefers_an_entity_target_over_mining`]
/// above proves the ordinary switch prefers an entity target; this proves a
/// spectator's left-click on that same entity target takes a completely
/// different branch and never reaches `attack_entity`/`Attacking` at all.
#[test]
fn begin_attack_live_spectates_the_entity_target_instead_of_attacking() {
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);
    let local = sim.local;
    sim.write(|w| {
        w.entity_mut(local)
            .insert(ServerGameMode(Some(lodestone_client::GameMode::Spectator)));
        w.resource_mut::<EntityRayTarget>().0 = Some(42);
    });

    sim.begin_attack_live();

    let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
    assert_eq!(
        sent,
        vec![ClientAction::SpectatorAction {
            target_entity_id: Some(42)
        }],
        "a spectator's left-click on an entity must send SpectatorAction(Some(id)) \
         and nothing else — no attack packet, no swing"
    );
    let attacking = sim.read(|w| w.resource::<Attacking>().0);
    assert!(!attacking, "spectating must not arm the hold-to-mine loop");
}

/// The other half of the same gate: a spectator's left-click with no entity
/// target (a block, or nothing at all) sends `SpectatorAction(None)` —
/// `MultiPlayerGameMode.spectatorNoAction`. Distinct from the miss-swings-the-arm
/// case a non-spectator hits, matching vanilla's own "neither arm swings"
/// behaviour.
#[test]
fn begin_attack_live_sends_no_target_spectator_action_on_a_miss() {
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);
    let local = sim.local;
    sim.write(|w| {
        w.entity_mut(local)
            .insert(ServerGameMode(Some(lodestone_client::GameMode::Spectator)));
    });
    assert!(sim.entity_target().is_none(), "precondition: no entity targeted");

    sim.begin_attack_live();

    let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
    assert_eq!(
        sent,
        vec![ClientAction::SpectatorAction {
            target_entity_id: None
        }],
        "a spectator's left-click with no entity under the crosshair must send \
         SpectatorAction(None), got {sent:?}"
    );
    let peak = peak_swing_over(&mut sim, 10);
    assert_eq!(peak, 0.0, "a spectator's click must not swing the arm either way");
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

/// Same idiom as [`give_main_hand_item`], carrying a real
/// `minecraft:written_book_content` — the component
/// `crates/protocol/v770/src/adapter/inventory.rs`'s
/// `read_written_book_content` populates off the wire, folded in through the
/// same `ClientEvent` production uses rather than written into the menu by
/// hand.
fn give_main_hand_written_book(sim: &mut Sim, title: &str, author: &str, generation: u8, pages: &[&str]) {
    let local = sim.local;
    let mut stack = lodestone_model::ItemStack::new(
        "minecraft:written_book".parse().expect("valid item id"),
        1,
    );
    stack.components.written_book_content = Some(lodestone_model::WrittenBookContent {
        title: title.to_owned(),
        author: author.to_owned(),
        generation,
        pages: pages.iter().map(|p| lodestone_model::Text::literal(*p)).collect(),
        resolved: true,
    });
    sim.write(|w| {
        if let Some(mut menus) = w.get_mut::<lodestone_ecs::SessionMenus>(local) {
            menus.0.apply(&lodestone_model::ClientEvent::InventorySlotChanged {
                slot: 0,
                item: Some(stack.clone()),
            });
        }
    });
}

/// The book-reading screen's producer: a signed book in the main hand must
/// be reported with the title, author, generation and pages the wire
/// carried.
///
/// **This is the link that was missing**, not the decode: v770 has decoded
/// `minecraft:written_book_content` into `ItemComponents` for as long as
/// `book_content_wiring.rs` has existed, and
/// `lodestone_game::item::ItemStack::written_book_content` had **zero**
/// production readers — so a signed book folded into the menu correctly and
/// reached nothing at all. Right-clicking it did nothing and its tooltip
/// said "Written Book".
///
/// Fields are pairwise distinct (a title and an author that are different
/// words, generation `2` rather than the `0` a fresh signature carries, two
/// differing pages) so a transposition of the two adjacent strings cannot
/// survive.
#[test]
fn a_signed_book_in_hand_is_reported_with_its_own_metadata_and_pages() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    assert!(
        sim.written_book_in_hand().is_none(),
        "precondition: an empty hand holds no book"
    );

    give_main_hand_written_book(&mut sim, "Wandering Notes", "Steve", 2, &["First page", "Second page"]);

    let open = sim
        .written_book_in_hand()
        .expect("a signed book in the main hand must open the reading screen");
    assert_eq!(
        open,
        crate::menu::book_view::BookViewOpen {
            title: "Wandering Notes".to_owned(),
            author: "Steve".to_owned(),
            generation: 2,
            pages: vec![
                lodestone_model::Text::literal("First page"),
                lodestone_model::Text::literal("Second page"),
            ],
        }
    );
}

/// The two book screens must not answer for each other's item. Without this,
/// `try_use`'s fork could route a signed book into the *editor* (which would
/// then send an `EditBook` for an immutable book) or a draft into the reader.
///
/// Both directions, collected rather than asserted in the loop.
#[test]
fn the_two_book_screens_do_not_claim_each_others_item() {
    let mut failures = Vec::new();

    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    give_main_hand_written_book(&mut sim, "T", "A", 0, &["p"]);
    if sim.writable_book_in_hand().is_some() {
        failures.push("a signed book was claimed by the editor's producer");
    }
    if sim.written_book_in_hand().is_none() {
        failures.push("control: a signed book was not claimed by the reader's producer");
    }

    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    give_main_hand_item(&mut sim, "minecraft:writable_book");
    if sim.written_book_in_hand().is_some() {
        failures.push("an unsigned draft was claimed by the reader's producer");
    }
    if sim.writable_book_in_hand().is_none() {
        failures.push("control: an unsigned draft was not claimed by the editor's producer");
    }

    assert!(failures.is_empty(), "{failures:?}");
}

/// A server `OPEN_BOOK` packet names the hand it intends to display. This
/// keeps that selector through the relay and verifies the projection does not
/// silently choose the main hand when an off-hand book was requested.
#[test]
fn server_open_book_keeps_its_offhand_selector() {
    use crate::net::NetUpdate;

    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);
    give_main_hand_item(&mut sim, "minecraft:stick");

    let local = sim.local;
    let mut offhand_book = lodestone_model::ItemStack::new(
        "minecraft:written_book".parse().expect("valid item id"),
        1,
    );
    offhand_book.components.written_book_content = Some(lodestone_model::WrittenBookContent {
        title: "Offhand notes".to_owned(),
        author: "Alex".to_owned(),
        generation: 0,
        pages: vec![lodestone_model::Text::literal("the requested book")],
        resolved: true,
    });
    sim.write(|w| {
        w.get_mut::<lodestone_ecs::SessionMenus>(local)
            .expect("local player has menus")
            .0
            .apply(&lodestone_model::ClientEvent::InventorySlotChanged {
                slot: lodestone_game::menu::OFFHAND_NATIVE as i32,
                item: Some(offhand_book),
            });
    });

    feed.send(NetUpdate::BookOpened { main_hand: false })
        .expect("loopback is connected");
    sim.poll_net();

    assert_eq!(sim.take_pending_book_open(), Some(false));
    assert!(sim.written_book_in_hand_at(true).is_none());
    assert_eq!(
        sim.written_book_in_hand_at(false)
            .expect("off-hand book must be selected")
            .title,
        "Offhand notes"
    );
}

/// A lectern carries just its display-book slot, not the usual 36 appended
/// player slots. Its page lives in menu property zero and is the exact button
/// index returned to the UI.
#[test]
fn lectern_book_view_reads_slot_zero_and_container_page_data() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    let mut book = lodestone_model::ItemStack::new(
        "minecraft:written_book".parse().expect("valid item id"),
        1,
    );
    book.components.written_book_content = Some(lodestone_model::WrittenBookContent {
        title: "Lectern manual".to_owned(),
        author: "Librarian".to_owned(),
        generation: 1,
        pages: vec![
            lodestone_model::Text::literal("first"),
            lodestone_model::Text::literal("second"),
        ],
        resolved: true,
    });
    let local = sim.local;
    sim.write(|w| {
        let mut menus = w
            .get_mut::<lodestone_ecs::SessionMenus>(local)
            .expect("local player has menus");
        menus.0.apply(&lodestone_model::ClientEvent::ScreenOpened {
            window_id: 12,
            menu_type: "minecraft:lectern".parse().expect("valid menu id"),
            title: lodestone_model::Text::literal("Lectern"),
        });
        menus.0.apply(&lodestone_model::ClientEvent::ContainerContent {
            window_id: 12,
            state_id: 4,
            items: vec![Some(book)],
            carried_item: None,
        });
        menus.0.apply(&lodestone_model::ClientEvent::ContainerData {
            window_id: 12,
            property: 0,
            value: 1,
        });
    });

    let (window_id, open, page) = sim.lectern_book_view().expect("lectern book opens");
    assert_eq!(window_id, 12);
    assert_eq!(page, 1);
    assert_eq!(open.title, "Lectern manual");
    assert_eq!(open.pages[1], lodestone_model::Text::literal("second"));
}

/// Same idiom as [`give_main_hand_item`], carrying a real `minecraft:
/// equippable` component so `Sim::predict_equip_swap` has something to find
/// — matching what `crates/protocol/v770/src/adapter/inventory.rs`'s
/// `read_component_patch` really populates for an armour item off the wire
/// (`ItemComponents::equippable`, seeded from `lodestone_data::
/// item_prototypes`), not the bare `ItemStack::new` [`give_main_hand_item`]
/// builds.
fn give_main_hand_equippable_item(sim: &mut Sim, item: &str, slot: EquipmentSlot) {
    let local = sim.local;
    let mut stack =
        lodestone_model::ItemStack::new(item.parse().expect("valid item id"), 1);
    stack.components.equippable = Some(slot);
    sim.write(|w| {
        if let Some(mut menus) = w.get_mut::<lodestone_ecs::SessionMenus>(local) {
            menus.0.apply(&lodestone_model::ClientEvent::InventorySlotChanged {
                slot: 0,
                item: Some(stack),
            });
        }
    });
}

/// Issue #613: `ClientAction::Stab` had zero producers. `Minecraft.startAttack`
/// checks the held item for `DataComponents.PIERCING_WEAPON` *before* the
/// normal ENTITY/BLOCK/MISS switch and takes it unconditionally — proved here
/// with an entity under the crosshair (the case the ordinary switch would
/// otherwise prefer, per `begin_attack_live_prefers_an_entity_target_over_mining`)
/// so a regression that let the entity switch win first would show up as a
/// missing `Stab`/extra `InteractEntity` rather than passing by coincidence.
#[test]
fn begin_attack_live_stabs_with_a_spear_instead_of_the_normal_attack_switch() {
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);
    give_main_hand_item(&mut sim, "minecraft:diamond_spear");
    sim.write(|w| w.resource_mut::<EntityRayTarget>().0 = Some(42));

    sim.begin_attack_live();

    let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
    assert!(
        sent.iter().any(|a| matches!(a, ClientAction::Stab)),
        "holding a piercing weapon must send Stab, got {sent:?}"
    );
    assert!(
        !sent.iter().any(|a| matches!(a, ClientAction::InteractEntity { .. })),
        "a piercing weapon must take over the switch entirely, not send the \
         ordinary attack packet too — got {sent:?}"
    );
    assert!(
        sent.iter()
            .any(|a| matches!(a, ClientAction::SwingArm { hand: Hand::Main })),
        "vanilla still swings the main hand after piercingAttack — got {sent:?}"
    );
    let attacking = sim.read(|w| w.resource::<Attacking>().0);
    assert!(!attacking, "a stab must not arm the hold-to-mine loop either");
}

/// A non-spear weapon must not take the `Stab` branch — the negative control
/// for the item-identity gate `is_piercing_weapon` implements.
#[test]
fn begin_attack_live_does_not_stab_with_an_ordinary_sword() {
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);
    give_main_hand_item(&mut sim, "minecraft:diamond_sword");
    sim.write(|w| w.resource_mut::<EntityRayTarget>().0 = Some(42));

    sim.begin_attack_live();

    let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
    assert!(
        !sent.iter().any(|a| matches!(a, ClientAction::Stab)),
        "an ordinary sword must never send Stab, got {sent:?}"
    );
    assert!(
        sent.iter().any(|a| matches!(
            a,
            ClientAction::InteractEntity {
                interaction: EntityInteraction::Attack,
                ..
            }
        )),
        "an ordinary sword must still take the normal attack switch, got {sent:?}"
    );
}

/// Finding 2 (combat scoping doc): before this fix, `use_item_live`
/// returned unconditionally after `interact_entity` whenever *any*
/// entity was targeted — hostile mobs included, the overwhelmingly
/// common combat case — so a bow or shield could never even start a use.
/// Vanilla's own `case ENTITY` (`Minecraft.java`) only returns
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
/// unconditional fallback (`Minecraft.java`).
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
/// (`Minecraft.java`). Without this, "always send `UseItem`"
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

/// The third branch, and the one the two above left behind: with a **block**
/// under the crosshair, `use_item_live` used to `return` after its
/// `UseItemOn` + `SwingArm` pair and never reach the generic use.
///
/// That gate is worth more than the branch it fixes, because the server's
/// `ServerPlayerGameMode.useItemOn` never reaches `Item.use`: `USE_ITEM` is
/// the *only* route by which a boat is placed, food is eaten, a drink is
/// drunk, a helmet is equipped on use, or a bow starts drawing. All of that
/// worked aimed at open air or at a mob and did nothing aimed at a block —
/// which is why a boat could be placed over deep water (where the block ray
/// misses entirely) and not on a shoreline.
///
/// A boat is the subject deliberately: `minecraft:oak_boat` is not a block,
/// so it can never take the placement path and the fall-through is the whole
/// of its behaviour.
#[test]
fn use_item_live_falls_through_to_generic_use_with_a_block_targeted() {
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);
    give_main_hand_item(&mut sim, "minecraft:oak_boat");
    sim.set_ray_target_for_test(Some(RayHit::face_center([4, 64, 4], [0, 1, 0])));
    assert!(sim.target().is_some(), "precondition: a block is targeted");
    assert!(
        sim.entity_target().is_none(),
        "precondition: no entity targeted, or the entity branch answers instead \
         and this proves nothing about the block one"
    );

    sim.use_item_live();

    let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
    let use_on = sent
        .iter()
        .position(|a| matches!(a, ClientAction::UseItemOn { .. }));
    let use_item = sent
        .iter()
        .position(|a| matches!(a, ClientAction::UseItem { hand: Hand::Main, .. }));
    assert!(
        use_on.is_some(),
        "the block interaction itself must still be sent, got {sent:?}"
    );
    assert!(
        use_item.is_some(),
        "a block target must also fall through to the generic use-item send — \
         this is the only packet that places a boat, eats food or draws a bow. \
         Got {sent:?}"
    );
    assert!(
        use_on < use_item,
        "`use_item_on` must precede `use_item` on the wire, as vanilla's \
         `case BLOCK` reaches `gameMode.useItem` only after `gameMode.useItemOn` \
         — got {sent:?}"
    );
}

/// The discriminating control for the branch above, and the reason it is not
/// an unconditional `use_item_generic()` call.
///
/// Vanilla's `case BLOCK` is **not** a `break` like `case ENTITY`'s:
/// `Minecraft.startUseItem` returns on `InteractionResult.Success` *and* on
/// `InteractionResult.Fail`, reaching the generic use only for a non-consuming
/// result. So a **placeable** item in hand must not produce a second send: the
/// item's own `useOn` is what answered, and falling through would let a carved
/// pumpkin aimed at an illegal face equip itself onto the player's head
/// instead of doing nothing.
///
/// `minecraft:stone` against this harness's target yields
/// `UseOnDecision::Nothing` (the loopback net publishes no `ClientHandle`, so
/// `block_at` reads `None` and the placement is declined as illegal) — which
/// is precisely vanilla's `Fail`, and precisely the arm an unconditional
/// fall-through would get wrong. Same `Sim`, same target, same call as the
/// gate above; only the held item differs.
#[test]
fn a_placeable_item_on_a_block_does_not_also_send_the_generic_use() {
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);
    give_main_hand_item(&mut sim, "minecraft:stone");
    sim.set_ray_target_for_test(Some(RayHit::face_center([4, 64, 4], [0, 1, 0])));

    sim.use_item_live();

    let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
    assert!(
        sent.iter()
            .any(|a| matches!(a, ClientAction::UseItemOn { .. })),
        "control precondition: the block send must still happen, or the absence \
         below is just an unwired Sim — got {sent:?}"
    );
    assert!(
        !sent
            .iter()
            .any(|a| matches!(a, ClientAction::UseItem { .. })),
        "a block item answered the click itself, so the generic use must not \
         follow it — got {sent:?}"
    );
}

// -----------------------------------------------------------------------
// Armour equip prediction (`Sim::predict_equip_swap`)
// -----------------------------------------------------------------------
//
// Right-clicking an armour piece from the hotbar with nothing under the
// crosshair lands in `use_item_generic` — vanilla's `Item.use()` →
// `Equippable.swapWithEquipmentSlot` — the same landing site
// `use_item_live_sends_generic_use_with_no_target_at_all` already proves for
// a bow. Before `predict_equip_swap` existed that method sent `UseItem` and
// wrote nothing locally, so the helmet only appeared once the server's own
// `SET_SLOT` pair for the head and hotbar slots came back: the round trip
// the "missing client prediction" report was about.

/// The control: without the prediction, the head slot (menu index 5) starts
/// and stays empty across the call — this is the assertion that fails on the
/// pre-fix `use_item_generic`, which only sent `UseItem` and touched no menu
/// state at all.
#[test]
fn use_item_generic_predicts_the_armour_equip_swap_locally() {
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);
    give_main_hand_equippable_item(&mut sim, "minecraft:diamond_helmet", EquipmentSlot::Head);
    assert!(sim.target().is_none(), "precondition: no block targeted");
    assert!(sim.entity_target().is_none(), "precondition: no entity targeted");
    assert!(
        sim.player_menu().slot_item(5).is_none(),
        "precondition: the head slot must start empty"
    );

    sim.use_item_live();

    // The prediction itself: the head slot shows the helmet *before* any
    // server acknowledgement — nothing here drains a socket or applies a
    // `ClientEvent` from the wire.
    let head = sim.player_menu().slot_item(5).cloned();
    assert!(
        head.as_ref()
            .is_some_and(|s| s.item().to_string() == "minecraft:diamond_helmet"),
        "the helmet must be predicted into the head slot locally, with no \
         server round trip — got {head:?}"
    );
    // A straight swap: nothing was worn before, so the hotbar slot the
    // helmet came from must be predicted empty, not still holding it.
    assert!(
        sim.player_menu().player_native(0).is_none(),
        "the hotbar slot the helmet was drawn from must be predicted empty \
         after the swap, got {:?}",
        sim.player_menu().player_native(0)
    );
    // The server is still authoritative and must hear the click — the
    // prediction is additive, not a replacement for the send.
    let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
    assert!(
        sent.iter()
            .any(|a| matches!(a, ClientAction::UseItem { hand: Hand::Main, .. })),
        "the swap must not skip the wire send — got {sent:?}"
    );
}

/// The reconciliation arm: a server that disagrees with the local guess must
/// win. Simulates the authoritative `SET_SLOT` pair a real server sends after
/// `broadcastChanges` diffs `player.inventoryMenu` — here, one that names a
/// *different* helmet than the one predicted (standing in for any server
/// refusal or race), which must overwrite the predicted contents rather than
/// leave them standing.
#[test]
fn a_disagreeing_server_set_slot_overwrites_the_predicted_equip() {
    let (net, _actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);
    give_main_hand_equippable_item(&mut sim, "minecraft:diamond_helmet", EquipmentSlot::Head);

    sim.use_item_live();
    assert_eq!(
        sim.player_menu()
            .slot_item(5)
            .map(|s| s.item().to_string()),
        Some("minecraft:diamond_helmet".to_string()),
        "precondition: the prediction landed the diamond helmet"
    );

    // The server's own truth disagrees — e.g. the swap never actually
    // happened server-side (enchantment/creative gating this client cannot
    // model, per `Sim::predict_equip_swap`'s own doc) and the head slot is
    // really still empty.
    let local = sim.local;
    sim.write(|w| {
        if let Some(mut menus) = w.get_mut::<lodestone_ecs::SessionMenus>(local) {
            menus.0.apply(&lodestone_model::ClientEvent::ContainerSlot {
                window_id: 0,
                state_id: 1,
                slot: 5,
                item: None,
            });
        }
    });

    assert!(
        sim.player_menu().slot_item(5).is_none(),
        "a disagreeing server SET_SLOT must overwrite the prediction, got {:?}",
        sim.player_menu().slot_item(5)
    );
}

// -----------------------------------------------------------------------
// Throwing a projectile item swings the arm
// -----------------------------------------------------------------------
//
// `SnowballItem.use`/`EggItem.use`/`EnderpearlItem.use`/`ThrowablePotionItem.use`
// (`.cache/mc/26.2/src/net/minecraft/world/item/`) all return
// `InteractionResult.SUCCESS`, whose `swingSource()` is `CLIENT`
// (`InteractionResult.java`) — vanilla's `Minecraft.startUseItem` swings on
// exactly that condition. `use_item_generic` is the shell's landing site for
// all four (none of them is a block or an `EntityRayTarget` hit in the common
// case). It used to call `swing_hand()` unconditionally whenever the main
// hand was non-empty; it now asks `generic_use_swings` first, so these gates
// are also the positive half of that table — the discriminating check is
// whether the swing actually **reaches the arm pose**, not merely whether
// `use_item_generic` was reached (`peak_swing_over` is `hand_swing_progress`
// wired the same way `a_queued_main_hand_swing_reaches_the_arm_pose` proves
// for a mining swing). The negative half is
// `a_use_that_vanilla_does_not_swing_for_leaves_the_arm_still`.

/// The common case: aiming at open air (or space, past reach) while throwing.
/// Vanilla's `hitResult == null` path skips the block/entity switch and still
/// reaches the unconditional generic-use fallback that actually throws the
/// snowball and swings.
#[test]
fn throwing_a_snowball_with_no_target_swings_the_arm() {
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);
    give_main_hand_item(&mut sim, "minecraft:snowball");
    assert!(sim.target().is_none(), "precondition: no block targeted");
    assert!(sim.entity_target().is_none(), "precondition: no entity targeted");

    sim.use_item_live();

    let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
    assert!(
        sent.iter()
            .any(|a| matches!(a, ClientAction::SwingArm { hand: Hand::Main })),
        "throwing a snowball must queue a main-hand SwingArm for the wire, got {sent:?}"
    );
    let peak = peak_swing_over(&mut sim, 10);
    assert!(
        peak > 0.4,
        "throwing a snowball must swing the local arm, progress peaked at {peak} \
         — this is the bug report: the wire action can be present while the local \
         arm still reads as rested"
    );
}

/// The other common case: aiming at ordinary terrain (dirt, stone — nothing
/// interactable) while throwing. A snowball is not a block, so it can never
/// take the placement path, and vanilla's `case BLOCK` falls through to the
/// same generic use as the no-target case whenever the block declines the
/// click (`Minecraft.java`'s `useResult instanceof InteractionResult.Fail`
/// `return`s, but a plain block's `useItemOn` is `PASS`, not `Fail`).
#[test]
fn throwing_a_snowball_at_a_plain_block_still_swings_the_arm() {
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);
    give_main_hand_item(&mut sim, "minecraft:snowball");
    sim.set_ray_target_for_test(Some(RayHit::face_center([4, 64, 4], [0, 1, 0])));
    assert!(sim.target().is_some(), "precondition: a block is targeted");

    sim.use_item_live();

    let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
    assert!(
        sent.iter()
            .any(|a| matches!(a, ClientAction::SwingArm { hand: Hand::Main })),
        "throwing a snowball at a plain block must still queue a SwingArm, got {sent:?}"
    );
    let peak = peak_swing_over(&mut sim, 10);
    assert!(
        peak > 0.4,
        "throwing a snowball at a plain block must swing the local arm, \
         progress peaked at {peak}"
    );
}

/// The same gate for the other three throwables the report names, collected
/// rather than asserted one-at-a-time inside a loop — CLAUDE.md's own
/// warning about an `assert!` inside a `for` loop stopping at the first
/// failure and hiding the rest.
#[test]
fn every_named_throwable_swings_the_arm_with_no_target() {
    let items = [
        "minecraft:egg",
        "minecraft:ender_pearl",
        "minecraft:splash_potion",
    ];
    let mut failures = Vec::new();
    for item in items {
        let (net, _actions, _feed) = NetClient::loopback_with_feed();
        let mut sim = Sim::new(test_config());
        sim.drain_all_meshes();
        sim.attach_net(net);
        give_main_hand_item(&mut sim, item);
        sim.use_item_live();
        let peak = peak_swing_over(&mut sim, 10);
        if peak <= 0.4 {
            failures.push(format!("{item}: peak={peak}"));
        }
    }
    assert!(
        failures.is_empty(),
        "these throwables did not swing the local arm: {failures:?}"
    );
}

/// Negative control for the three tests above: an **empty** main hand must
/// swing nothing, matching vanilla's own `!heldItem.isEmpty()` guard —
/// without this, "swing unconditionally" would satisfy the throw tests
/// vacuously.
#[test]
fn use_item_live_with_an_empty_hand_does_not_swing() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    assert!(sim.target().is_none());
    assert!(sim.entity_target().is_none());

    sim.use_item_live();

    let peak = peak_swing_over(&mut sim, 10);
    assert_eq!(
        peak, 0.0,
        "an empty main hand has nothing to throw and must not swing, got {peak}"
    );
}

// -----------------------------------------------------------------------
// ... and a use vanilla is silent for must not swing
// -----------------------------------------------------------------------
//
// The owner's report: *"right clicking with (i think) any item makes me swing
// my arm, which is wrong."* It did — `use_item_generic` swung for every
// non-empty main hand, and `use_item_live`'s block path swung for every
// click that reached it.
//
// Vanilla's rule is one condition, applied identically at all three of
// `Minecraft.startUseItem`'s call sites: swing only when the result is an
// `InteractionResult.Success` whose `swingSource()` is `CLIENT`.
// `InteractionResult.CONSUME` is `SwingSource.NONE`, and it is what a drawn
// bow, a raised shield, a spyglass and a bite of food all return; `PASS` is
// what an idle sword or pickaxe returns. All five were swinging here.

/// The negative half of the throwable table above, and the report itself.
///
/// Each item is paired with the vanilla method and result that decides it, so
/// a failure names *why* the expectation is what it is rather than only that
/// it was missed. Mismatches are collected rather than asserted inside the
/// loop — an `assert!` in the body would stop at the first item and leave the
/// rest as arguments instead of observations.
///
/// The `use_item_sent` column is the control: without it "no swing" is
/// satisfied by a `Sim` that never reached `use_item_generic` at all, which
/// would make the whole gate vacuous. Vanilla sends the `USE_ITEM` packet for
/// every non-empty hand regardless of what the result turns out to be — the
/// swing is the only thing gated.
#[test]
fn a_use_that_vanilla_does_not_swing_for_leaves_the_arm_still() {
    let silent = [
        ("minecraft:bow", "BowItem.use -> CONSUME (SwingSource::NONE)"),
        ("minecraft:crossbow", "CrossbowItem.use -> CONSUME"),
        ("minecraft:trident", "TridentItem.use -> CONSUME"),
        (
            "minecraft:spyglass",
            "SpyglassItem.use -> ItemUtils.startUsingInstantly -> CONSUME",
        ),
        (
            "minecraft:shield",
            "Item.use's minecraft:blocks_attacks arm -> CONSUME",
        ),
        (
            "minecraft:bread",
            "Item.use -> Consumable.startConsuming -> CONSUME",
        ),
        ("minecraft:diamond_sword", "Item.use -> PASS"),
        ("minecraft:diamond_pickaxe", "Item.use -> PASS"),
    ];
    let mut failures = Vec::new();
    for (item, vanilla) in silent {
        let (net, actions, _feed) = NetClient::loopback_with_feed();
        let mut sim = Sim::new(test_config());
        sim.drain_all_meshes();
        sim.attach_net(net);
        give_main_hand_item(&mut sim, item);

        sim.use_item_live();

        let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
        let wire_swing = sent
            .iter()
            .any(|a| matches!(a, ClientAction::SwingArm { .. }));
        let use_item_sent = sent
            .iter()
            .any(|a| matches!(a, ClientAction::UseItem { hand: Hand::Main, .. }));
        let peak = peak_swing_over(&mut sim, 10);
        if wire_swing || peak > 0.0 || !use_item_sent {
            failures.push(format!(
                "{item} ({vanilla}): wire_swing={wire_swing} arm_peak={peak} \
                 use_item_sent={use_item_sent}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "these uses must reach the wire without swinging the arm — a swing here \
         is both a wrong local animation and a SwingArm every other player sees: \
         {failures:?}"
    );
}

/// The same rule on the **block** path, which has its own swing site.
///
/// `MultiPlayerGameMode.performUseItemOn` returns the base `Item.useOn`'s
/// `PASS` for a sword against plain stone, so `Minecraft.startUseItem`'s
/// `case BLOCK` swings nothing and falls through to the generic use — which,
/// for a sword, is also `PASS`. Before the fix this swung twice over: once
/// unconditionally after the `USE_ITEM_ON` send and once in the fall-through.
///
/// The `UseItemOn` assertion is the control — the click must genuinely have
/// taken the block branch, or the absent swing proves only that nothing ran.
#[test]
fn right_clicking_a_plain_block_with_a_sword_does_not_swing() {
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);
    give_main_hand_item(&mut sim, "minecraft:diamond_sword");
    sim.set_ray_target_for_test(Some(RayHit::face_center([4, 64, 4], [0, 1, 0])));
    assert!(sim.target().is_some(), "precondition: a block is targeted");

    sim.use_item_live();

    let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
    assert!(
        sent.iter()
            .any(|a| matches!(a, ClientAction::UseItemOn { .. })),
        "control: the block branch must have been taken, got {sent:?}"
    );
    assert!(
        !sent
            .iter()
            .any(|a| matches!(a, ClientAction::SwingArm { .. })),
        "a sword against plain stone is vanilla's PASS at both the block and \
         the generic step, so no SwingArm may reach the wire — got {sent:?}"
    );
    let peak = peak_swing_over(&mut sim, 10);
    assert_eq!(
        peak, 0.0,
        "...and the local arm must stay at rest too, got {peak}"
    );
}

/// The discriminating positive for the block path's `Nothing` arm, and the
/// reason it is not simply "never swing on `Nothing`".
///
/// `UseOnDecision::Nothing` collapses two vanilla outcomes: the base
/// `Item.useOn`'s `PASS` (the sword above) and the overrides that act on the
/// block. `FlintAndSteelItem.useOn` returns `InteractionResult.SUCCESS`, so
/// vanilla swings **and returns** — it never reaches `gameMode.useItem`. Both
/// halves are asserted here, because a gate that only checked the swing would
/// pass against a version that also sent a spurious generic use.
#[test]
fn flint_and_steel_on_a_block_swings_and_stops_there() {
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);
    give_main_hand_item(&mut sim, "minecraft:flint_and_steel");
    sim.set_ray_target_for_test(Some(RayHit::face_center([4, 64, 4], [0, 1, 0])));

    sim.use_item_live();

    let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
    assert!(
        sent.iter()
            .any(|a| matches!(a, ClientAction::UseItemOn { .. })),
        "control: the block branch must have been taken, got {sent:?}"
    );
    assert!(
        sent.iter()
            .any(|a| matches!(a, ClientAction::SwingArm { hand: Hand::Main })),
        "flint and steel lights the block — vanilla's SUCCESS — so it must \
         swing, got {sent:?}"
    );
    assert!(
        !sent
            .iter()
            .any(|a| matches!(a, ClientAction::UseItem { .. })),
        "vanilla's `case BLOCK` returns on SUCCESS and never reaches \
         `gameMode.useItem`, so no generic use may follow — got {sent:?}"
    );
    let peak = peak_swing_over(&mut sim, 10);
    assert!(
        peak > 0.4,
        "the local arm must swing too, progress peaked at {peak}"
    );
}

/// One swing per click, on the entity path.
///
/// `Sim::interact_entity` swings unconditionally (see its doc: this client
/// models no `Entity.interact`, so there is no local result to gate on), and
/// the fall-through to `use_item_generic` used to swing again — two
/// `SwingArm` packets for one right-click, where vanilla's `case ENTITY`
/// swings at most once because it *returns* when it swings.
///
/// A snowball is the subject deliberately: it is the one item whose generic
/// use really is `SUCCESS`, so it would swing a second time on its own merits
/// if `already_swung` were not threaded through. With any silent item the
/// gate would pass for the wrong reason.
#[test]
fn an_entity_right_click_swings_exactly_once() {
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);
    give_main_hand_item(&mut sim, "minecraft:snowball");
    sim.write(|w| w.resource_mut::<EntityRayTarget>().0 = Some(42));

    sim.use_item_live();

    let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
    let swings = sent
        .iter()
        .filter(|a| matches!(a, ClientAction::SwingArm { .. }))
        .count();
    assert_eq!(
        swings, 1,
        "one right-click on an entity must put exactly one SwingArm on the \
         wire, got {swings} in {sent:?}"
    );
    assert!(
        sent.iter()
            .any(|a| matches!(a, ClientAction::UseItem { hand: Hand::Main, .. })),
        "control: the generic use must still follow the interact, or the count \
         above is one only because the fall-through never happened — got {sent:?}"
    );
}

/// Finding 1: [`Sim::end_use_live`] must send `ReleaseUseItem` when a use
/// was actually in progress — the packet that was a serverbound island
/// (encoded by all four protocol adapters, zero producers anywhere in
/// this shell). Bow, crossbow and shield are all `useOnRelease() ==
/// true` (`LivingEntity.java`) and cannot complete a
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

/// `ItemUseEffects`'s real writer (issue #671): before this, the component
/// existed, was read by `compute_movement_intent`, and had zero production
/// writers — every query resolved `None` and the use-item slowdown/sprint
/// veto were both permanently inert. `use_item_live` must now resolve the
/// held item and write a **non-default** value through, not just leave the
/// component at whatever `Default` already gave it — a gate that only
/// checked "is it `Some`" would still pass a producer that always writes
/// `UseEffects::DEFAULT` regardless of what is actually held.
#[test]
fn use_item_live_writes_the_held_items_use_effects_not_a_constant() {
    let (net, _actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);

    let read_effects = |sim: &mut Sim| -> Option<UseEffects> {
        let local = sim.local;
        sim.write(|w| w.get::<ItemUseEffects>(local).and_then(|e| e.0))
    };

    // Precondition: nothing in progress yet.
    assert_eq!(read_effects(&mut sim), None, "no use in progress yet");

    // A spear must resolve to the SPEAR override (sprint allowed, no slow).
    give_main_hand_item(&mut sim, "minecraft:wooden_spear");
    sim.use_item_live();
    assert_eq!(
        read_effects(&mut sim),
        Some(UseEffects::SPEAR),
        "charging a spear must write UseEffects::SPEAR, not a constant DEFAULT"
    );
    sim.end_use_live();
    assert_eq!(
        read_effects(&mut sim),
        None,
        "releasing must clear the component back to None"
    );

    // An ordinary item (a bow) must resolve to DEFAULT, distinguishing this
    // from a producer that always writes SPEAR.
    give_main_hand_item(&mut sim, "minecraft:bow");
    sim.use_item_live();
    assert_eq!(
        read_effects(&mut sim),
        Some(UseEffects::DEFAULT),
        "drawing a bow must write UseEffects::DEFAULT"
    );
    sim.end_use_live();
    assert_eq!(read_effects(&mut sim), None);
}

/// The owner's report, end to end: *"when i right click in the air it slows
/// me down as if im eating... it should only slow me down if im actually
/// using an item like food, bow, etc."* Before [`item_has_use_animation`]'s
/// gate, `use_item_live` armed [`ItemUseEffects`] for *any* held item —
/// including an empty hand aimed at open air — so every right-click cut
/// ground speed by `UseEffects::DEFAULT`'s fifth-input scale regardless of
/// what, if anything, was in hand. Measures real displacement over a real
/// tick run (via [`Sim::step`]) against a no-click control walking the same
/// ticks with the same input, rather than merely asserting the resource
/// stayed unset.
#[test]
fn right_clicking_with_an_empty_hand_does_not_slow_movement() {
    let walk = |click: bool| -> f64 {
        let mut sim = Sim::new(test_config());
        // Player spawns at (0.5, feet, 0.5) facing north (-Z, yaw 180). Lay a
        // solid floor and clear head-room along -Z, the same unobstructed
        // straight-line setup `sprint_vs_walk`-shaped gates in this file use,
        // so the measured distance reflects the speed multiplier and not
        // terrain the default demo world happens to put in the way.
        let feet_y = sim.player().position.y.floor() as i32;
        for dz in -25..=1 {
            for dx in -1..=1 {
                sim.set_block_world([dx, feet_y - 1, dz], id::STONE);
                sim.set_block_world([dx, feet_y, dz], id::AIR);
                sim.set_block_world([dx, feet_y + 1, dz], id::AIR);
                sim.set_block_world([dx, feet_y + 2, dz], id::AIR);
            }
        }
        // Settle onto the fresh floor first so the measured window is pure
        // ground-friction walking, not still-falling noise.
        for _ in 0..20 {
            sim.step(1.0 / 20.0);
        }
        let start = sim.player().position;
        if click {
            assert!(sim.target().is_none(), "precondition: no block targeted");
            assert!(sim.entity_target().is_none(), "precondition: no entity targeted");
            sim.use_item_live();
        }
        sim.input_mut(|i| i.set(lodestone_controller::Action::Forward, true));
        for _ in 0..20 {
            sim.step(1.0 / 20.0);
        }
        sim.player().position.subtract(start).length()
    };

    let plain = walk(false);
    let clicked = walk(true);
    assert!(
        (clicked - plain).abs() < 1e-6,
        "an empty-hand right-click in open air must not change ground speed \
         at all: plain={plain} clicked={clicked}"
    );
}

/// The positive control for the gate above: a **genuine** use item (food)
/// held through the same click *does* cut ground speed — proving the
/// harness can actually tell the two cases apart. Without this, the test
/// above would pass just as well against a version that never applies any
/// use-item slowdown at all, which is exactly the vacuous shape
/// `CLAUDE.md` warns a negative-only assertion can take.
#[test]
fn right_clicking_with_a_food_item_does_slow_movement() {
    let walk = |eating: bool| -> f64 {
        let mut sim = Sim::new(test_config());
        // Same unobstructed straight-line floor as the negative control above.
        let feet_y = sim.player().position.y.floor() as i32;
        for dz in -25..=1 {
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
        if eating {
            give_main_hand_item(&mut sim, "minecraft:bread");
        }
        let start = sim.player().position;
        assert!(sim.target().is_none(), "precondition: no block targeted");
        assert!(sim.entity_target().is_none(), "precondition: no entity targeted");
        sim.use_item_live();
        sim.input_mut(|i| i.set(lodestone_controller::Action::Forward, true));
        for _ in 0..20 {
            sim.step(1.0 / 20.0);
        }
        sim.player().position.subtract(start).length()
    };

    let plain = walk(false);
    let eating = walk(true);
    assert!(
        eating < plain * 0.5,
        "eating bread must cut ground speed sharply (UseEffects::DEFAULT scales \
         input to a fifth): plain={plain} eating={eating}"
    );
}

/// A food use refused by the authoritative hunger gate must not arm the local
/// use state. `ConsumeState::resolve` already rejects the animation/particles,
/// but the press edge has its own movement-effect writer and used to arm it for
/// every consumable before the server could refuse a full-bar bread use.
#[test]
fn right_clicking_food_at_a_full_hunger_bar_does_not_slow_movement() {
    let mut sim = Sim::new(test_config());
    let local = sim.local;
    sim.write(|world| {
        world
            .get_mut::<Vitals>(local)
            .expect("the local player has vitals")
            .food = Some(lodestone_game::food::MAX_FOOD);
    });
    give_main_hand_item(&mut sim, "minecraft:bread");

    sim.use_item_live();

    assert!(!sim.read(|world| world.resource::<UsingItem>().0));
    assert_eq!(
        sim.read(|world| world.get::<ItemUseEffects>(local).and_then(|e| e.0)),
        None,
        "a full-bar bread use is refused and must not arm movement slowdown"
    );
}

/// Holding `key.use` through a completed food use must begin the next use,
/// matching `Minecraft.handleKeybinds` polling the held key rather than only
/// reacting to the original OS press edge.  The loopback action stream is the
/// client/server seam: a second `UseItem` is what lets the authoritative server
/// start the second bite.
#[test]
fn holding_use_restarts_food_after_its_consume_duration() {
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);
    give_main_hand_item(&mut sim, "minecraft:bread");

    // `Mode::Headless` has no stitched vanilla atlas, so `Sim::use_item`
    // intentionally takes the demo-world placement path. Exercise the live
    // press transition directly, as the nearby outbound-use seam tests do.
    sim.use_item_live();
    for _ in 0..32 {
        sim.step(1.0 / 20.0);
    }

    let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
    let uses = sent
        .iter()
        .filter(|action| matches!(action, ClientAction::UseItem { hand: Hand::Main, .. }))
        .count();
    assert_eq!(
        uses, 2,
        "holding use through bread's 32-tick duration must send a fresh UseItem, got {sent:?}"
    );
}

/// Vanilla's `getCurrentItemAttackStrengthDelay`/`getAttackStrengthScale`
/// (`Player.java`): with no [`Attributes`] component at all (the
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
/// (`MultiPlayerGameMode.java`).
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
/// `canCriticalAttack` (`Player.java`) is satisfied on every
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
/// (`Player.java`'s `entity instanceof LivingEntity` clause) —
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

/// Spawn one entity of `entity_type` two blocks in front of the player and
/// return what [`Sim::update_entity_target`] resolves the view ray to.
///
/// The ray is the same one the two tests above use — horizontal, `+x`, from
/// just above the player's feet — so an item's 0.25-block box and a pig's
/// 0.9-block one are both crossed, and the only thing that can differ between
/// two calls is the entity type. That is the point: this helper exists so the
/// exclusion test below can be run with a *pickable* type as its control and
/// have nothing else move.
fn ray_target_for_type(entity_type: &str, entity_id: i32) -> Option<i32> {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    let feet = sim.player().position;
    ingest(
        &mut sim,
        lodestone_client::ClientEvent::EntitySpawned {
            entity_id,
            uuid: None,
            entity_type: entity_type.parse().expect("valid entity type key"),
            pos: lodestone_model::Vec3::new(feet.x + 2.0, feet.y, feet.z),
            rotation: Rotation::new(0.0, 0.0),
            velocity: None,
        },
    );
    let origin = [feet.x, feet.y + 0.1, feet.z];
    let dir = [1.0, 0.0, 0.0];
    sim.update_entity_target(origin, dir, None);
    sim.entity_target()
}

/// The owner's live kick, at the layer that caused it: the view ray must not
/// resolve to a dropped item or an experience orb.
///
/// Killing a mob spawns its drops and its orbs inside the hitbox the mob just
/// vacated, so before this fix the next left-click picked one of them and sent
/// an attack naming it. `ServerGamePacketListenerImpl.handleAttack` treats an
/// `ItemEntity` or `ExperienceOrb` target as a protocol violation and
/// disconnects with `multiplayer.disconnect.invalid_entity_attacked` — the
/// reported "Attempting to attack an invalid entity". Vanilla never sends it
/// because `Entity.isPickable()` is `false` and neither class overrides it.
///
/// The pig arm is the control and it is load-bearing: it proves this ray does
/// cross a box at that position, so a `None` from the other two arms is the
/// type predicate doing its job rather than a mis-aimed fixture. Both arms are
/// collected before asserting, so a failure reports every type rather than
/// stopping at the first.
#[test]
fn the_view_ray_never_picks_a_dropped_item_or_an_experience_orb() {
    assert_eq!(
        ray_target_for_type("minecraft:pig", 51),
        Some(51),
        "control: a pig at this exact position must be targetable, or the two \
         exclusions below prove nothing"
    );

    let picked: Vec<&str> = ["minecraft:item", "minecraft:experience_orb"]
        .into_iter()
        .filter(|kind| ray_target_for_type(kind, 52).is_some())
        .collect();
    assert!(
        picked.is_empty(),
        "these types must never be picked — the server kicks the session for \
         attacking one: {picked:?}"
    );
}

/// [`entity_type_can_be_picked`] against the whole 26.2 entity-type census,
/// rather than against the handful of names the bug happened to involve.
///
/// The count is the drift guard. The predicate is a reduction over ten vanilla
/// `isPickable()` declaring classes, and a version bump that adds an entity type
/// lands it in exactly one of two buckets — the census's `is_living` column, or
/// this module's explicit non-living lists. A new *living* type moves this total
/// and the test names it; a new non-living one does not move it and stays
/// unpickable, which is vanilla's own default and cannot cause a kick.
///
/// The named rows are the ones that decide something the count cannot: two the
/// server kicks for, three arrow types whose exclusion comes from a tag rather
/// than from their own override, and a dragon that is living and still not
/// pickable.
#[test]
fn the_pick_predicate_matches_the_vanilla_entity_census() {
    use crate::interact::entity_type_can_be_picked;

    let key = |name: &str| -> lodestone_model::ResourceKey {
        name.parse().expect("valid entity type key")
    };

    let mut wrong = Vec::new();
    for (name, expected) in [
        // Rejected by `handleAttack` — these two are the reported kick.
        ("minecraft:item", false),
        ("minecraft:experience_orb", false),
        // `AbstractArrow.isPickable()` is `super.isPickable() && !isInGround()`,
        // and that `super` is `Projectile`'s `redirectable_projectile` tag test,
        // which no arrow type is in. So arrows are never pickable at all.
        ("minecraft:arrow", false),
        ("minecraft:spectral_arrow", false),
        ("minecraft:trident", false),
        // Non-redirectable projectiles fall to the same tag test.
        ("minecraft:snowball", false),
        ("minecraft:egg", false),
        // The three tag members that *are* redirectable.
        ("minecraft:fireball", true),
        ("minecraft:wind_charge", true),
        ("minecraft:breeze_wind_charge", true),
        // Living, plus the one living type that overrides back to `false`.
        ("minecraft:pig", true),
        ("minecraft:zombie", true),
        ("minecraft:player", true),
        ("minecraft:armor_stand", true),
        ("minecraft:ender_dragon", false),
        // One from each non-living pickable family.
        ("minecraft:oak_boat", true),
        ("minecraft:bamboo_raft", true),
        ("minecraft:hopper_minecart", true),
        ("minecraft:painting", true),
        ("minecraft:item_frame", true),
        ("minecraft:end_crystal", true),
        ("minecraft:interaction", true),
        ("minecraft:falling_block", true),
        ("minecraft:tnt", true),
        ("minecraft:shulker_bullet", true),
        // The `Entity` default, and a namespace the census cannot speak for.
        ("minecraft:area_effect_cloud", false),
        ("minecraft:text_display", false),
        ("minecraft:marker", false),
        ("someplugin:custom_mob", false),
    ] {
        if entity_type_can_be_picked(&key(name)) != expected {
            wrong.push(name);
        }
    }
    assert!(wrong.is_empty(), "misclassified entity types: {wrong:?}");

    let pickable = (0..lodestone_data::entity_types::TYPE_COUNT)
        .filter_map(|id| lodestone_data::entity_types::entity_type_name(id as i32))
        .filter(|name| entity_type_can_be_picked(&key(name)))
        .count();
    assert_eq!(
        pickable, 131,
        "the 26.2 census has 131 pickable entity types of {}; a change here \
         means a type moved between the living column and the explicit lists",
        lodestone_data::entity_types::TYPE_COUNT
    );
}

/// Issue #12's knockback half: a `ClientboundSetEntityMotionPacket`
/// (`ClientEvent::EntityVelocity`) naming the local player's own server
/// entity id must overwrite `PlayerState.velocity` outright — vanilla's
/// `Entity.lerpMotion` is `setDeltaMovement(movement)`, an unconditional
/// replace, and `LocalPlayer` declares no override (`Entity.java`).
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

/// The local self-avatar is a synthetic draw rather than a tracked entity, so
/// its held stack must preserve the modern client-only `minecraft:item_model`
/// component while it is narrowed to the renderer's visual id.
#[test]
fn third_person_body_uses_the_selected_stack_item_model_for_its_main_hand() {
    let mut sim = Sim::new(test_config());
    sim.cycle_camera_type();
    let mut stack = lodestone_model::ItemStack::new(
        "minecraft:diamond_sword".parse().expect("valid gameplay item id"),
        1,
    );
    stack.components.item_model = Some("server:gun".parse().expect("valid visual item id"));
    let local = sim.local;
    sim.write(|world| {
        world
            .get_mut::<lodestone_ecs::SessionMenus>(local)
            .expect("local player has menus")
            .0
            .apply(&lodestone_model::ClientEvent::InventorySlotChanged {
                slot: 0,
                item: Some(stack),
            });
    });

    let body = sim
        .third_person_body_state()
        .expect("third-person body is enabled");
    assert_eq!(
        body.equipment
            .iter()
            .find(|(slot, _)| *slot == EquipmentSlot::MainHand)
            .map(|(_, id)| id.to_string())
            .as_deref(),
        Some("server:gun"),
        "the local avatar's hand resolves the item definition, not its vanilla gameplay id"
    );
}

/// The local avatar becomes a synthetic [`EntityDraw`](crate::entities::EntityDraw)
/// only after this state is made. Its held player head must therefore retain the
/// profile URL beside the visual item id; otherwise the third-person special-item
/// pass can only bind its static Steve fallback.
#[test]
fn third_person_body_retains_the_selected_heads_profile_skin_for_its_main_hand() {
    const URL: &str = "https://example.invalid/custom-head.png";
    const TEXTURES: &str =
        "eyJ0ZXh0dXJlcyI6eyJTS0lOIjp7InVybCI6Imh0dHBzOi8vZXhhbXBsZS5pbnZhbGlkL2N1c3RvbS1oZWFkLnBuZyJ9fX0=";

    let mut sim = Sim::new(test_config());
    sim.cycle_camera_type();
    let mut stack = lodestone_model::ItemStack::new(
        "minecraft:player_head".parse().expect("valid player-head item id"),
        1,
    );
    stack.components.profile = Some(lodestone_model::ItemProfile {
        name: Some("custom head".to_owned()),
        id: None,
        properties: vec![lodestone_model::ProfileProperty {
            name: "textures".to_owned(),
            value: TEXTURES.to_owned(),
            signature: None,
        }],
    });
    let local = sim.local;
    sim.write(|world| {
        world
            .get_mut::<lodestone_ecs::SessionMenus>(local)
            .expect("local player has menus")
            .0
            .apply(&lodestone_model::ClientEvent::InventorySlotChanged {
                slot: 0,
                item: Some(stack),
            });
    });

    let body = sim
        .third_person_body_state()
        .expect("third-person body is enabled");
    assert_eq!(
        body.equipment_skin
            .iter()
            .find(|(slot, _)| *slot == EquipmentSlot::MainHand)
            .map(|(_, skin)| skin.as_ref()),
        Some(URL),
        "the synthetic local-avatar draw must retain the held head's profile URL"
    );
}

/// A cached profile skin belongs to its UUID. A sessionless preview has no
/// active UUID and must not consume whichever account happened to publish
/// most recently — that was the cross-account skin flash on join.
#[test]
fn a_sessionless_body_does_not_consume_another_accounts_cached_model() {
    fn sheet() -> lodestone_assets::Image {
        lodestone_assets::Image {
            width: 64,
            height: 64,
            rgba: vec![0u8; 64 * 64 * 4],
        }
    }

    let mut sim = Sim::new(test_config());
    sim.cycle_camera_type();
    assert!(sim.local_uuid().is_none(), "control: this sim has no account identity");
    let foreign = uuid::Uuid::from_u128(0xF0_12_E1_6E);

    crate::skin_fetch::publish(foreign, lodestone_assets::PlayerModelType::Slim, sheet());
    let body = sim
        .third_person_body_state()
        .expect("third person is on");
    assert!(
        !body.slim,
        "a profile cache with no matching live UUID must not select its slim rig"
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
/// (`GameRenderer.java`), only `bobView` is gated on the option.
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

/// **Production-path control for the `text_display` island**: a real
/// `text_display` folded through the same `ClientEvent` -> `IngestQueue` ->
/// `NetIngest` path production uses (`ingest`, exactly like
/// [`end_session_clears_the_entity_tracks`] above), then through the same
/// `Extract` schedule [`crate::sim::step::Sim::step`] runs every frame, must
/// reach [`Sim::display_draws`] with no hand-installed draw anywhere in this
/// test.
///
/// This is deliberately **not** a test that calls
/// `crate::display_entities::extracted_display_draws`/`set_display_draws`
/// itself — a GPU pixel gate already proved those two functions individually
/// correct, by installing a draw it built by hand and rendering it. That
/// proves the *renderer*, not that anything in production ever calls the
/// installer: `RenderState::set_display_draws` had zero production callers
/// until `app::redraw` was wired to call `Sim::display_draws()`, so a
/// `text_display` was resolved all the way to a draw-ready snapshot and then
/// dropped on the floor. This test's job is to fail if that hop goes missing
/// again, which a hand-installed-draw gate structurally cannot do.
#[test]
fn a_real_text_display_folded_through_ingest_and_extract_reaches_sim_display_draws() {
    let mut sim = Sim::with_demo_world(test_config());
    ingest(
        &mut sim,
        lodestone_client::ClientEvent::EntitySpawned {
            entity_id: 9,
            uuid: None,
            entity_type: "minecraft:text_display".parse().expect("valid entity type key"),
            pos: lodestone_model::Vec3::new(1.0, 64.0, 1.0),
            rotation: Rotation::new(0.0, 0.0),
            velocity: None,
        },
    );
    ingest(
        &mut sim,
        lodestone_client::ClientEvent::EntityMetadataUpdated {
            entity_id: 9,
            metadata: lodestone_model::EntityMetadataUpdate {
                display_text: lodestone_client::Reported::Reported(Some(
                    lodestone_model::Text::literal("hello"),
                )),
                ..Default::default()
            },
        },
    );
    // The same two-step production sequence `Sim::step` runs every frame
    // (`sim/step.rs`): fold, then the `Extract` schedule that populates
    // `ExtractedDisplayDraws` (`display_entities::DisplayEntityPlugin`,
    // installed in `Sim::client_app`).
    sim.fold_entities();
    sim.write(|w| w.run_schedule(Extract));

    let draws = sim.display_draws();
    let draw = draws
        .iter()
        .find(|d| d.id == 9)
        .unwrap_or_else(|| panic!("entity 9 never reached Sim::display_draws: {draws:?}"));
    assert_eq!(draw.type_path, crate::display_entities::TEXT_DISPLAY_TYPE_PATH);
    assert_eq!(
        draw.text.as_ref().map(lodestone_model::Text::to_plain_string),
        Some("hello".to_string())
    );
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
/// `Hud.extractVignette` (`Hud.java`) on a *static* border reduces
/// to `warningDistance == warningBlocks` exactly, because
/// `StaticBorderExtent.getLerpSpeed()` returns `0.0`
/// (`WorldBorder.java`) and `max(warningBlocks, 0)` is
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
    // step later (`Hud.java`).
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

/// [`Sim::reload_resource_pack_atlas`]'s equality guard: a freshly built `Sim`
/// seeds `last_pack_generation` to whatever [`crate::resources::pack_generation`]
/// already was at construction (its own doc explains why — the `BlockResources`
/// that built this session already reflects that generation, so redoing the
/// reload on the very first frame would be pure waste). So the very first call,
/// with nothing having changed the selection since, must be a no-op.
#[test]
fn a_fresh_session_does_not_reload_on_its_first_poll() {
    let mut sim = Sim::with_demo_world(test_config());
    assert!(
        sim.reload_resource_pack_atlas().is_none(),
        "a fresh session's first poll must see no generation change and do nothing"
    );
}

/// The demo world has no server world to re-texture and never depends on a
/// resource pack (`BlockResources::load(false)` always yields the demo
/// palette), so even a *real* selection change must still be a no-op there —
/// this is the "no `net`" arm of the method's own doc. Bumping
/// `crate::resources::pack_generation` here is what makes this call reach
/// past the equality guard at all; without it this test would only be
/// re-checking the guard above under a different name.
#[test]
fn the_demo_world_never_reloads_even_after_a_real_selection_change() {
    let mut sim = Sim::with_demo_world(test_config());
    crate::resources::set_selected_packs(vec!["some-pack".to_string()]);
    assert!(
        sim.reload_resource_pack_atlas().is_none(),
        "the demo world has no vanilla atlas to reload and must stay a no-op \
         even once the pack selection has genuinely changed"
    );
    // And the guard is consumed even though nothing else happened: a second
    // call with no further selection change must also see no change, rather
    // than re-attempting the (still pointless, on the demo world) reload
    // every frame.
    assert!(
        sim.reload_resource_pack_atlas().is_none(),
        "the generation was already observed by the call above"
    );
}

/// A total-drop condition must not be silent: `TerrainMesh::mesh_column_inner`
/// drops **every** column once `MeshPolicy::id_spaces_agree` is false, and the
/// state that produces it — a live session with no vanilla atlas — is exactly
/// what `resources::asset_root` returning `None` (an unresolved pack root, the
/// owner's reported "launched from outside the repo" case) or any other
/// vanilla-load failure converges on. `with_demo_world` never even attempts a
/// vanilla load (`BlockResources::load(false)`), so attaching a live net to it
/// reproduces "a live session with no vanilla atlas" deterministically — no
/// filesystem or `LODESTONE_ASSETS` manipulation needed to hit the same `Sim`
/// state a real unresolved asset root produces.
#[test]
fn a_live_session_with_no_vanilla_atlas_fires_the_id_space_diagnostic_once() {
    let mut sim = Sim::with_demo_world(test_config());
    assert!(
        sim.vanilla_atlas().is_none(),
        "precondition: the demo-world build never attempts a vanilla load"
    );
    let (net, _actions, _feed) = NetClient::loopback_with_feed();
    sim.attach_net(net);
    sim.step(1.0 / 20.0);

    assert!(
        sim.warned_id_space_mismatch,
        "a live session with no vanilla atlas must fire the one-time id-space \
         diagnostic — every column is about to be dropped unmeshed"
    );
    assert!(
        sim.stats.status.contains("TERRAIN NOT LOADING"),
        "the debug overlay's own status line must name the total-drop \
         condition instead of staying on whatever it last said, got {:?}",
        sim.stats.status
    );

    // One-time, not per-frame: further polls must not re-trip anything (there
    // is nothing to re-observe — `warned_id_space_mismatch` stays latched for
    // the rest of this session, which is what keeps the log from repeating
    // every frame the way the per-column warning already does).
    for _ in 0..5 {
        sim.step(1.0 / 20.0);
    }
    assert!(
        sim.warned_id_space_mismatch,
        "the latch must not un-set itself mid-session"
    );
}

/// Control for the diagnostic above: an always-on warning would be exactly as
/// useless as a silent one, so a session that actually resolves the vanilla
/// pack must stay quiet. `client_config`'s `Mode::Window` is what reaches the
/// real `BlockResources::load(true)` path (`with_demo_world`/`Mode::Headless`
/// never attempts it), so this is the same live-load path
/// `a_client_session_holds_only_the_live_world_never_offline_terrain` and its
/// neighbours already depend on succeeding in this checkout.
#[test]
// The control's premise is "this checkout resolves a real vanilla pack", and
// it asserts that precondition loudly rather than skipping — correct, and it
// makes the gate unrunnable on a runner with no `.cache/mc/<version>/`. The
// measurement it controls (the warning *does* fire when the pack is missing)
// needs no jar and keeps running everywhere; only this half is gated.
#[ignore = "requires a fetched vanilla client.jar + blocks.json under .cache/mc/<version>/"]
fn a_healthy_live_session_never_fires_the_id_space_diagnostic() {
    use crate::net::NetUpdate;

    let mut sim = Sim::new(client_config());
    assert!(
        sim.vanilla_atlas().is_some(),
        "precondition: this checkout must resolve a real vanilla pack under \
         .cache/mc/<ver> for the control to mean anything — banner: {:?}",
        sim.asset_banner()
    );
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
    sim.poll_net();
    sim.step(1.0 / 20.0);

    assert!(
        !sim.warned_id_space_mismatch,
        "a session with a real vanilla atlas must never trip the id-space \
         mismatch diagnostic"
    );
    assert!(
        !sim.stats.status.contains("TERRAIN NOT LOADING"),
        "the healthy control must not show the mismatch banner, got {:?}",
        sim.stats.status
    );
}

/// The discriminating gate for the vanilla `yBodyRot`/`yHeadRot` split
/// (`LivingEntity.tickHeadTurn`): looking around while standing still must
/// **not** turn the third-person body until the head exceeds `50°` relative
/// to it (`Player.getMaxHeadRotationRelativeToBody`'s default), and once it
/// does, the body must snap so the head sits at *exactly* that clamp — not
/// at the raw look yaw, which is what this engine did before (the body
/// always equalled the head, unconditionally). Three arms in one input space
/// — inside, exactly at, and beyond the clamp — collected and asserted
/// together, per this repo's own rule against an `assert!` inside a `for`
/// loop hiding every failure but the first.
///
/// A head yaw of `0` relative to the body is the coincident input where the
/// clamped and unclamped readings agree; every arm here uses a nonzero
/// offset so the two hypotheses can actually disagree.
#[test]
fn body_yaw_holds_still_within_the_clamp_and_snaps_once_it_is_exceeded() {
    struct Case {
        name: &'static str,
        offset: f32,
        // Expected body yaw delta from the base yaw after one tick.
        expected_body_delta: f32,
        // Expected |head yaw relative to body| after one tick.
        expected_head_relative: f32,
    }
    let cases = [
        Case {
            name: "inside the clamp (30 of 50)",
            offset: 30.0,
            expected_body_delta: 0.0,
            expected_head_relative: 30.0,
        },
        Case {
            name: "exactly at the clamp (50 of 50)",
            offset: 50.0,
            expected_body_delta: 0.0,
            expected_head_relative: 50.0,
        },
        Case {
            name: "beyond the clamp (90 of 50)",
            offset: 90.0,
            // tick_head_turn: no movement candidate, so the 0.3 catch-up
            // term is a no-op (target == body_yaw); the clamp then bumps
            // body_yaw by (90 - 50) = 40 so the head sits exactly at the
            // 50° boundary. Derived from `LivingEntity.tickHeadTurn`, not
            // guessed.
            expected_body_delta: 40.0,
            expected_head_relative: 50.0,
        },
    ];

    let mut mismatches = Vec::new();
    for case in cases {
        let mut sim = Sim::new(test_config());
        sim.cycle_camera_type();
        let base_yaw = sim.player().yaw;
        sim.player_mut(|p| p.yaw = base_yaw + case.offset);
        sim.step(lodestone_ecs::TICK_PERIOD);
        // Force full-tick interpolation so this reads the tick that just
        // ran rather than easing from the previous one.
        sim.clock_mut(|c| c.interp_alpha = 1.0);
        let state = sim
            .third_person_body_state()
            .expect("third person is on");
        let body_delta = wrap_degrees(state.body_yaw_deg - base_yaw);
        let head_relative = wrap_degrees(state.anim.head_yaw_deg).abs();
        if (body_delta - case.expected_body_delta).abs() > 1.0 {
            mismatches.push(format!(
                "{}: body yaw moved {body_delta:.2}°, want {:.2}°",
                case.name, case.expected_body_delta
            ));
        }
        if (head_relative - case.expected_head_relative).abs() > 1.0 {
            mismatches.push(format!(
                "{}: head relative to body {head_relative:.2}°, want {:.2}°",
                case.name, case.expected_head_relative
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "body/head clamp mismatches: {mismatches:#?}"
    );
}

/// The movement-direction clause `LivingEntity.tick` feeds `tickHeadTurn`'s
/// candidate: while the feet are moving, the body eases toward the
/// *walking* direction rather than the look direction. Strafing
/// perpendicular to a fixed look yaw is the case that discriminates this
/// from the head-clamp gate above, which never moves the feet: sustained
/// strafing must turn the body away from the look yaw at all (rejecting
/// "the body never turns from movement alone"), and — because the movement
/// direction here is durably ~90° from the look yaw — the head-relative-to-
/// body offset converges to *exactly* the clamp regardless of walk speed,
/// the same fixed point the clamp gate's "beyond" arm measures directly.
/// That convergence value is the clamp constant itself, not a guessed round
/// number: it falls out of `tickHeadTurn`'s recursion once the pull toward
/// the movement direction outpaces what the clamp allows each tick.
#[test]
fn body_yaw_eases_toward_the_movement_direction_while_strafing() {
    let mut sim = Sim::new(test_config());
    sim.cycle_camera_type();
    let base_yaw = sim.player().yaw;
    sim.input_mut(|i| i.set(lodestone_controller::Action::Left, true));

    let mut moved = false;
    for _ in 0..40 {
        let before = sim.player().position;
        sim.step(lodestone_ecs::TICK_PERIOD);
        let after = sim.player().position;
        if (after.x - before.x).abs() > 1.0e-6 || (after.z - before.z).abs() > 1.0e-6 {
            moved = true;
        }
    }
    assert!(
        moved,
        "precondition: strafing must actually move the player, or this gate \
         measures nothing"
    );
    assert!(
        (sim.player().yaw - base_yaw).abs() < 1.0e-6,
        "precondition: no mouse input was fed, so the look yaw must not have \
         drifted on its own"
    );

    sim.clock_mut(|c| c.interp_alpha = 1.0);
    let state = sim
        .third_person_body_state()
        .expect("third person is on");
    let body_delta = wrap_degrees(state.body_yaw_deg - base_yaw).abs();
    assert!(
        body_delta > 1.0,
        "the body never turned toward the movement direction; it is still \
         at the look yaw (body_delta {body_delta:.2}°) — this is the exact \
         shape of feeding the body the raw look yaw every tick"
    );
    let head_relative = wrap_degrees(state.anim.head_yaw_deg).abs();
    assert!(
        (30.0..=55.0).contains(&head_relative),
        "sustained strafing perpendicular to a fixed look direction must \
         settle the head-relative-to-body offset near the 50° clamp \
         (`Player.getMaxHeadRotationRelativeToBody`), got {head_relative:.2}°"
    );
}

/// Rejects a snap-at-threshold implementation: a mid-frame sample of a tick
/// that moved the body must land strictly *between* the tick's start and end
/// body yaw, not jump straight to the endpoint early. Uses the clamp gate's
/// own "beyond" arm (90° offset -> a real 40° body move) so there is a
/// nonzero span to interpolate across.
#[test]
fn body_yaw_interpolates_between_ticks_rather_than_snapping() {
    let mut sim = Sim::new(test_config());
    sim.cycle_camera_type();
    let base_yaw = sim.player().yaw;
    sim.player_mut(|p| p.yaw = base_yaw + 90.0);
    sim.step(lodestone_ecs::TICK_PERIOD);

    sim.clock_mut(|c| c.interp_alpha = 0.0);
    let start = sim
        .third_person_body_state()
        .expect("third person is on")
        .body_yaw_deg;
    sim.clock_mut(|c| c.interp_alpha = 1.0);
    let end = sim
        .third_person_body_state()
        .expect("third person is on")
        .body_yaw_deg;
    sim.clock_mut(|c| c.interp_alpha = 0.5);
    let mid = sim
        .third_person_body_state()
        .expect("third person is on")
        .body_yaw_deg;

    assert!(
        (start - base_yaw).abs() < 1.0,
        "start of tick should still read the pre-tick body yaw, got {start} \
         (base {base_yaw})"
    );
    assert!(
        (end - base_yaw - 40.0).abs() < 1.0,
        "end of tick should read the new, clamped body yaw, got {end} (base \
         {base_yaw})"
    );
    let (lo, hi) = (start.min(end), start.max(end));
    assert!(
        mid > lo + 1.0 && mid < hi - 1.0,
        "a mid-frame sample must land strictly between the tick's start \
         ({start}) and end ({end}) body yaw rather than snapping to either \
         endpoint; got {mid}"
    );
}

/// Issue #649's real gate: a hex chat colour reaching a **drawn vertex**
/// through the actual per-frame wiring, not through hand-authored spans.
///
/// `hud.rs`'s own `chat_spans_carry_hex_named_and_inline_legacy_colour_to_distinct_vertices`
/// proves `HudGeometry::build` draws a hex colour when handed a
/// `Vec<TextSpan>` directly — it never touches `ChatLog`/`Sim` at all, so it
/// could not have caught this issue: the bug was entirely upstream, in
/// `Session::recent_chat`/`app/redraw.rs` calling the *legacy*
/// `String`-flattening accessor instead of the span-carrying one. This test
/// drives the real production entry point instead: a `NetUpdate::Chat`
/// folded by the real `Sim::poll_net` (exactly like
/// `end_session_tears_down_and_a_fresh_connect_afterward_starts_clean` above
/// feeds one), read back through `Sim::recent_chat_spans` — the exact
/// accessor `app/redraw.rs` calls every frame — windowed with the same
/// slicing arithmetic `app/redraw.rs` applies, and drawn through the same
/// `HudGeometry::build` the live `HudRenderer` calls.
#[test]
fn hex_chat_colour_reaches_a_vertex_through_the_real_session_and_redraw_wiring() {
    use crate::hud::{DebugStats, HudFrame, HudGeometry};
    use lodestone_model::text::{Text, TextColor, TextContent, TextSpan, TextStyle};

    // The discriminating fixture from issue #649: a hex `TextColor::Rgb`
    // component style, a named component style, and a literal carrying an
    // *inline* `§c` code — the owner's own report was entirely the third
    // convention. A fixture using only named colours cannot tell "hex is
    // dropped" from "everything works", because a named colour survives
    // legacy flattening intact.
    let hex = Text {
        content: TextContent::Literal("Hex".to_string()),
        style: TextStyle {
            font: None,
            color: Some(TextColor::Rgb(0x1a_2b3c)),
            ..TextStyle::default()
        },
        ..Text::default()
    };
    let inline_legacy = Text::literal("\u{00a7}cRed");
    let named = Text {
        content: TextContent::Literal("Gray".to_string()),
        style: TextStyle {
            font: None,
            color: Some(TextColor::Gray),
            ..TextStyle::default()
        },
        ..Text::default()
    };
    let root = Text {
        extra: vec![hex, inline_legacy, named],
        ..Text::default()
    };

    // The real production entry point: a server chat line arriving on the
    // net thread's `NetUpdate` channel, folded by the real `Sim::poll_net`
    // (`NetUpdate::Chat`'s handler in `sim/net_apply.rs` stores the full
    // `Text`, spans and all, in `ChatLog` — nothing flattens it at this
    // point).
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    feed.send(NetUpdate::Chat {
        text: root,
        player: false,
        sender: None,
        // A system message carries no signature to check, so the driver's
        // verdict is `false` — see `NetUpdate::Chat::verified`, which is
        // "unproven", not "forged".
        verified: false,
    })
    .unwrap();
    sim.poll_net();

    // The real accessor `app/redraw.rs` calls every frame.
    let chat_spans_owned = sim.recent_chat_spans(10);
    assert_eq!(
        chat_spans_owned.len(),
        1,
        "setup: exactly one chat line must have arrived"
    );

    // The same windowing/slicing `app/redraw.rs` applies before filling
    // `HudFrame::chat_spans` — closed-chat window is the whole vec, i.e.
    // `(0, chat_spans_owned.len())`, reproduced verbatim here.
    let chat_spans_lines: Vec<(&[TextSpan], f32)> = chat_spans_owned
        .iter()
        .map(|(spans, age)| (spans.as_slice(), *age))
        .collect();

    let stats = DebugStats::default();
    let geo = HudGeometry::build(
        &HudFrame {
            crosshair: false,
            show_debug: false,
            chat_spans: &chat_spans_lines,
            ..HudFrame::new(&stats)
        },
        640,
        480,
    );
    assert!(
        geo.vertex_count() > 0,
        "sanity: the line must draw something at all"
    );

    let byte = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
    let has_colour = |rgb: (u8, u8, u8)| {
        geo.verts
            .chunks_exact(6)
            .any(|v| (byte(v[2]), byte(v[3]), byte(v[4])) == rgb)
    };
    let expected = [
        ("hex", (0x1a_u8, 0x2b_u8, 0x3c_u8)),
        ("inline §c", (0xff_u8, 0x55_u8, 0x55_u8)),
        ("named gray", (0xaa_u8, 0xaa_u8, 0xaa_u8)),
    ];
    let missing: Vec<&str> = expected
        .iter()
        .filter(|(_, rgb)| !has_colour(*rgb))
        .map(|(name, _)| *name)
        .collect();
    assert!(
        missing.is_empty(),
        "these colours never reached a vertex through the real Sim/redraw \
         wiring: {missing:?} (full expected set: {expected:?})"
    );

    // Control: the sibling *legacy* accessor on the exact same stored line,
    // fed into `HudFrame::chat` instead of `chat_spans` — proves the
    // detector can actually fail, and localises the loss to the accessor
    // seam this issue names rather than to `HudGeometry::build` itself
    // (which is shared by both branches).
    let chat_owned = sim.recent_chat(10);
    assert_eq!(chat_owned.len(), 1, "setup: same one line, legacy accessor");
    let chat_legacy: Vec<(&str, f32)> =
        chat_owned.iter().map(|(l, a)| (l.as_str(), *a)).collect();
    let legacy_geo = HudGeometry::build(
        &HudFrame {
            crosshair: false,
            show_debug: false,
            chat: &chat_legacy,
            ..HudFrame::new(&stats)
        },
        640,
        480,
    );
    let legacy_has_colour = |rgb: (u8, u8, u8)| {
        legacy_geo
            .verts
            .chunks_exact(6)
            .any(|v| (byte(v[2]), byte(v[3]), byte(v[4])) == rgb)
    };
    assert!(
        !legacy_has_colour((0x1a, 0x2b, 0x3c)),
        "control failed: the legacy accessor was expected to lose the hex \
         colour (that is the bug issue #649 names) but drew it anyway — this \
         test's premise is wrong"
    );
}

/// The anti-island companion to the gate above: a grep control on
/// `app/redraw.rs`'s own source, in the same spirit as
/// `menu::nav::tests::app_rs_still_threads_every_chat_option_into_the_hud_frame`.
/// The gate above proves the wiring works when it is exercised *this* way,
/// but a unit test cannot run `App::redraw` itself (it is the frame loop, and
/// needs a live GPU context) — so if the real call site quietly reverted to
/// filling the legacy `HudFrame::chat` field instead, this test would keep
/// passing while the screen went back to hex-blind. This is one grep wide,
/// which is why it is checked by reading the source rather than by driving
/// the widget.
#[test]
fn app_rs_fills_hud_frame_chat_spans_not_the_legacy_chat_field() {
    let src = include_str!("../app/redraw.rs");
    assert!(
        src.contains("hud_frame.chat_spans = &chat_spans_lines"),
        "app/redraw.rs must fill `HudFrame::chat_spans` from the real \
         `Sim::recent_chat_spans` wiring — see issue #649"
    );
    // The control: the detector must be able to report an absence. The old,
    // hex-blind line this issue's fix replaced.
    assert!(
        !src.contains("hud_frame.chat = &chat_lines"),
        "app/redraw.rs must not go back to filling the legacy, hex-blind \
         `HudFrame::chat` field from the per-frame chat wiring — that is \
         exactly the regression this test exists to catch"
    );
}
