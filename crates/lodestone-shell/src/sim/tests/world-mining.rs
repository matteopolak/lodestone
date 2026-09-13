use super::*;
#[test]
fn recipe_snapshot_reuses_the_cache_until_the_source_revision_moves() {
    let mut sim = Sim::new(client_config());
    assert_eq!(sim.known_recipes().revision(), 0);
    assert_eq!(sim.recipe_book_cache_revision, Some(0));

    ingest(
        &mut sim,
        lodestone_client::ClientEvent::RecipeBookAdded {
            entries: vec![lodestone_model::event::RecipeBookEntry {
                display_id: 7,
                result_items: vec![lodestone_model::ItemId::protocol_local(10)],
                station_items: vec![lodestone_model::ItemId::protocol_local(11)],
                group: None,
                category: 0,
                crafting_requirements: None,
                notification: true,
                highlight: true,
            }],
            replace: true,
        },
    );
    assert_eq!(sim.recipe_book_cache_revision, Some(0));
    assert_eq!(sim.known_recipes().known().len(), 1);
    assert_eq!(sim.recipe_book_cache_revision, Some(1));

    sim.reset_for_server_transfer();
    assert!(sim.recipe_book_cache_revision.is_none());
    assert!(!sim.known_recipes().has_data());
    assert!(sim.known_recipes().known().is_empty());
}

/// A `ClientEvent::Login` for `entity_id`, creative in the overworld — the
/// event that seeds `ServerEntityId` **and** the local player's `EntityIndex`
/// entry.
pub(crate) fn login_event(entity_id: i32) -> lodestone_client::ClientEvent {
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
pub(crate) fn displayed_sidebar(sim: &Sim) -> Option<String> {
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
pub(crate) fn client_config() -> Config {
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
    // The bare builder has no local HUD component to consult, so potion
    // fields stay at their defaults here. Live mining uses
    // `dig_break_inputs_with_effects` after reading `HudEffects`.
    let inputs = dry_ground(census::STONE);
    assert_eq!(inputs.tool_speed, 1.0);
    assert_eq!(inputs.mining_efficiency, 0.0);
    assert_eq!(inputs.haste_amplifier, None);
    assert_eq!(inputs.mining_fatigue, None);
    assert_eq!(inputs.block_break_speed, 1.0);
}

#[test]
fn mining_effect_amplifiers_choose_the_stronger_haste_source_and_keep_fatigue() {
    use lodestone_ecs::session::HudEffects;
    use lodestone_game::effect::{ActiveEffects, StatusEffect};

    let mut active = ActiveEffects::new();
    active.apply(StatusEffect::new(
        "minecraft:haste".parse().expect("valid id"),
        0,
        200,
    ));
    active.apply(StatusEffect::new(
        "minecraft:conduit_power".parse().expect("valid id"),
        2,
        200,
    ));
    active.apply(StatusEffect::new(
        "minecraft:mining_fatigue".parse().expect("valid id"),
        1,
        200,
    ));
    let (haste, fatigue) = mining_effect_amplifiers(Some(&HudEffects(active)));
    assert_eq!(haste, Some(2), "Haste and Conduit Power do not stack");
    assert_eq!(fatigue, Some(1));

    let inputs = dig_break_inputs_with_effects(
        census::STONE,
        lodestone_model::ToolMining { speed: 8.0, correct_tool: true, damage_per_block: 1 },
        false,
        true,
        false,
        false,
        haste,
        fatigue,
    );
    assert_eq!(inputs.haste_amplifier, Some(2));
    assert_eq!(inputs.mining_fatigue, Some(1));
    assert!(
        inputs.dig_speed() < 8.0,
        "Mining Fatigue II must win over the faster Conduit Power multiplier"
    );
}

#[test]
fn mining_break_attributes_reach_the_timing_inputs() {
    let key = |path: &str| {
        lodestone_model::Identifier::new("minecraft", path).expect("valid attribute id")
    };
    let attributes = Attributes(vec![
        lodestone_model::EntityAttributeSnapshot {
            attribute: key("mining_efficiency"),
            base: 26.0,
            modifiers: Vec::new(),
        },
        lodestone_model::EntityAttributeSnapshot {
            attribute: key("block_break_speed"),
            base: 1.25,
            modifiers: Vec::new(),
        },
        lodestone_model::EntityAttributeSnapshot {
            attribute: key("submerged_mining_speed"),
            base: 1.0,
            modifiers: Vec::new(),
        },
    ]);
    assert_eq!(
        mining_break_attributes(Some(&attributes)),
        (26.0, 1.25, 1.0),
        "the server-fed snapshots must reach the mining builder unchanged"
    );

    let (efficiency, break_speed, submerged_speed) = mining_break_attributes(Some(&attributes));
    let mut priced = dig_break_inputs_with_effects(
        census::STONE,
        lodestone_model::ToolMining {
            speed: 8.0,
            correct_tool: true,
            damage_per_block: 1,
        },
        false,
        true,
        true,
        false,
        None,
        None,
    );
    priced.mining_efficiency = efficiency;
    priced.block_break_speed = break_speed;
    priced.submerged_mining_speed = submerged_speed;
    assert_eq!(
        priced.ticks_to_break(),
        Some(2),
        "efficiency, break speed, and Aqua Affinity's submerged speed must all affect timing"
    );

    let defaults = mining_break_attributes(None);
    assert_eq!(defaults, (0.0, 1.0, 0.2));
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
        .block_hardness(
            lodestone_data::block_states::StateId::new(id::AIR)
                .expect("air is in the block-state census"),
        )
        .expect("air must resolve through the seam");
    assert_eq!(air.hardness, 0.0);

    // Find the census entries the unit tests above assume, by value rather
    // than by id (ids renumber every data bump).
    let entries: Vec<_> = (0..40_000)
        .filter_map(lodestone_data::block_states::StateId::new)
        .filter_map(|state_id| version.block_hardness(state_id))
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
    assert!(lodestone_data::block_states::StateId::new(u32::MAX).is_none());
}

/// Live break-timing gate for the shell's own mining inputs, against the
/// survival oracle (`lodestone-survival`, game :25565, RCON :25566).
///
/// The hermetic tests above prove the *arithmetic*. What they cannot prove is
/// the reason the fixed-hardness baseline is unsafe: feeding a real
/// hardness moves the client's `STOP_DESTROY` from ~5 ticks to the block's
/// true completion tick, which is a change in **protocol interaction**, not
/// just in a number. The server has two branches on `STOP` and the hardness
/// value selects which one runs, so it has to be measured rather than reasoned about.
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
