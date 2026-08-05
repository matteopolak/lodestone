//! `Sim`'s **construction**: [`Sim::new`], [`Sim::with_demo_world`] and the
//! shared `build` both delegate to -- seam 9 of the sim.rs decomposition
//! sequence. Seam 1 was the test module, `sim/tests.rs`; 2 placement
//! prediction, `sim/placement.rs`; 3 the interaction/combat cluster,
//! `sim/actions.rs`; 4 the per-tick net-apply fold, `sim/net_apply.rs`; 5 the
//! audio cluster, `sim/audio.rs`; 6 the camera cluster, `sim/camera.rs`; 7
//! chunk/mesh streaming, `sim/meshing.rs`; 8 the `audio` *field* out of the
//! struct into the `AudioEngine` resource -- a field dissolution rather than a
//! file split, but `docs/sim-dissolution.md` numbers it in the same sequence,
//! so these five are 9-13. Seams 9-13 landed together.
//!
//! **`sim/meshing.rs`'s own module doc calls seam 7 "the last of the sim.rs
//! decomposition sequence".** That was true when it was written and is not now.
//! It is left exactly as it stands, because this split is a pure move and
//! editing a neighbour's prose is not part of one -- recorded here instead so a
//! reader who arrives through that file is not misled, and in
//! `docs/sim-dissolution.md`, which carries the authoritative seam list.
//!
//! This is the single most contended function in the whole file, which is why
//! 270 lines earn their own seam: every new plugin, resource, worker pool or
//! spawn-time component set adds a line *here* and nowhere else, so before the
//! split an agent registering a plugin serialised against an agent touching
//! the session lifecycle or the per-frame driver. Now it does not.
//!
//! Nothing widened. `build` is private and stays private: its only two callers
//! are `new` and `with_demo_world` in this same file, so no call crosses a
//! module boundary. `build`'s own trailing `sim.refresh_mesh_policy()` reaches
//! a method that stayed *private* in `sim.rs` and needs nothing either --
//! privacy cascades **downward**, so a parent's private item is visible to
//! every descendant. That rule is the reason the root kept the whole
//! lock-scoped accessor layer rather than moving it out and widening it.
//!
//! `use super::*;` for the same reason every earlier seam file uses it: this
//! module is a *descendant* of `sim`, so it already has the same visibility
//! into `Sim`'s private fields, into `sim.rs`'s remaining private helpers and
//! into everything `sim.rs` re-exports that `sim::tests` has always had, with
//! no need to enumerate any of it.

use super::*;

impl Sim {
    /// Build the simulation for a **real client session**: no offline world.
    ///
    /// The client renders exactly one world — the server's. Nothing is generated,
    /// meshed or uploaded here; terrain appears only as the live session's chunks
    /// arrive (`mark_column_dirty`), and the player's position comes from the
    /// login teleport.
    ///
    /// # Why there is no offline world any more
    ///
    /// There used to be one, generated unconditionally and meshed whenever the
    /// vanilla atlas was absent — which was *every windowed run that did not pass
    /// `--live`*, because the atlas choice was keyed off `config.connect_in_window`
    /// (see the report). Joining a server from the main menu then left the demo
    /// world resident and drawn around the origin while the player stood at the
    /// server's real spawn, with the live columns never meshed at all (the live
    /// branch of `mark_column_dirty` is gated on the vanilla atlas, which that
    /// session did not have). Two candidate worlds, one of them wrong, is a defect
    /// class rather than a bug: the fix is that the client only ever has one.
    ///
    /// `Mode::Headless` is the single remaining exception and delegates to
    /// [`Sim::with_demo_world`]: it is the offline, GPU-only evidence path
    /// (`app::run_headless` renders one offscreen frame and *fails* below 5%
    /// terrain coverage), so it needs a world that exists without a server.
    #[must_use]
    pub fn new(config: Config) -> Self {
        if config.mode == Mode::Headless {
            return Self::with_demo_world(config);
        }
        Self::build(config, false)
    }

    /// Build the simulation **around the offline demo world** — a fixture, not a
    /// product path.
    ///
    /// Generates `worldgen`'s world on the demo palette and schedules every
    /// non-empty section, i.e. exactly what [`Sim::new`] used to do for any run
    /// without `--live`. Two callers, both deliberate:
    ///
    /// * every hermetic gate that needs terrain without a server — this crate's
    ///   own unit tests (via `test_config`, which is `Mode::Headless`) and
    ///   `tests/break_particles_pixels.rs`;
    /// * `--headless`, through [`Sim::new`]'s `Mode::Headless` delegation.
    ///
    /// **Do not call this from an interactive path.** The demo palette and the
    /// vanilla registry are disjoint block-id spaces, so a session holding this
    /// world cannot mesh a server's chunks (see `mark_column_dirty`).
    #[must_use]
    pub fn with_demo_world(config: Config) -> Self {
        Self::build(config, true)
    }

    /// The shared constructor. `demo_world` picks between the two mutually
    /// exclusive block-id worlds *and* whether any offline terrain exists at all;
    /// the two must agree, which is why this is one function and not two.
    fn build(config: Config, demo_world: bool) -> Self {
        let (world, feet) = if demo_world {
            let radius = (config.render_distance as i32).clamp(1, MAX_WORLD_RADIUS);
            (worldgen::generate(radius), worldgen::spawn_feet())
        } else {
            (World::new(), PRE_SESSION_FEET)
        };

        let mut player = PlayerState::at(Vec3d::new(feet[0], feet[1], feet[2]), 180.0);
        player.pitch = 10.0;

        let workers = std::thread::available_parallelism()
            .map(|n| n.get().saturating_sub(1).max(1))
            .unwrap_or(2);

        // Pick the block-id world once. A client session wants the vanilla atlas
        // (the server's world streams vanilla ids); the demo-world fixture uses
        // the demo palette. A vanilla load failure falls back to the demo palette
        // and records a banner — see `mark_column_dirty`, which counts and logs
        // the live chunks such a session cannot mesh instead of dropping them
        // silently.
        let resources = BlockResources::load(!demo_world);
        let render_live = resources.vanilla_atlas.is_some();
        let mut terrain = TerrainMesh::new(MeshScheduler::new(workers, resources.classifier));
        let chunk_world = ChunkWorld::new(world);

        // `BlockResources::load(false)` always yields the demo palette, so this
        // never schedules demo ids under the vanilla atlas.
        debug_assert!(
            !(demo_world && render_live),
            "the demo world must never be meshed with the vanilla classifier"
        );
        if demo_world {
            for (cx, cz) in chunk_world
                .read()
                .iter()
                .map(|(pos, _)| (pos.x, pos.z))
                .collect::<Vec<_>>()
            {
                terrain.mesh_column(&chunk_world, cx, cz);
            }
        }

        let status = if render_live {
            "live world (vanilla atlas)".to_string()
        } else if let Some(banner) = &resources.banner {
            format!("demo palette — {banner}")
        } else {
            "local world".to_string()
        };
        let mut stats = DebugStats {
            status: status.clone(),
            ..Default::default()
        };
        stats.chunk_count = chunk_world.len();

        // The particle sprite table is indexed by whatever id the emitter will
        // be handed, so it must be built from the *same* palette the world uses.
        // With the vanilla atlas that is a baked-model state id; on the demo
        // world it is the shell's own small block table. Binding the wrong one
        // does not fail — it draws correctly-shaped debris in some other block's
        // colours, which reads as an art bug rather than a wiring bug.
        // Sheet particles (smoke, flame, crits, splashes) live in their own
        // stitch — they are unreachable from any blockstate, so the block atlas
        // above never contains them. Without this the emitter still runs and
        // every sheet quad is counted into `ParticleFrame::unresolved` rather
        // than drawn, which is why the HUD reports `0/0+Nunres` on a jar-less
        // run instead of silently showing nothing.
        // The sheet stitch is *also* kept on `Sim` (see `Sim::particle_atlas`):
        // the emitter needs its UV rects and the GPU needs its pixels, and
        // issue #45 is what happens when those two come from different images.
        let particle_atlas = resources.particle_atlas;
        let particles = match resources.vanilla_atlas.as_ref() {
            Some(atlas) => Particles::new(atlas.models()),
            None => Particles::with_demo_palette(&crate::blocks::build_atlas().uv_table),
        }
        .with_particle_atlas(particle_atlas.as_deref());

        // Per-block-state data (hardness, for the mining predictor) comes from
        // whichever version family the registry has compiled in for the
        // configured protocol. Resolved once here rather than per dig tick: the
        // lookup itself is a table index, but minting a boxed adapter 20× a
        // second to perform it would not be.
        let version_data = lodestone_registry::adapter_for_protocol(config.protocol);

        // The local player's `World`. Built through an `App` because `Plugin::build`
        // is the only way to register schedules and systems, then the `World` is
        // taken and the `App` dropped — azalea's own shape
        // (`azalea-client/src/client.rs:143`), and `crate::entities` does the same,
        // which is why nothing here ever calls `App::update`.
        //
        // `LocalPlayerPlugin` owns `TickSet::Physics`; `ControllerPlugin` owns
        // `TickSet::Input` and `TickSet::Send`; `SessionHudPlugin` owns
        // `TickSet::Animate` (ageing the title/action-bar/effect overlays at the
        // fixed 20 Hz their durations are counted in). All three are needed for a
        // player that is driven, reported *and* drawn, and they are separate
        // plugins so a harness can take one without the others.
        let mut app = lodestone_ecs::app::App::new();
        app.add_plugins((
            CorePlugin,
            LocalPlayerPlugin,
            ControllerPlugin,
            SessionHudPlugin,
            // §4.1(c). `IngestPlugin` + `SessionPlugin` are the *net thread's*
            // folds — the systems `lodestone_client::state::SharedState` runs — and
            // they are installed here because there is now one `World` and this is
            // it. Exactly once: `SessionPlugin` guards the shared
            // `drain_ingest_queue` with `is_plugin_added`, because `add_systems`
            // does not deduplicate and a second copy blanks every batch the first
            // one filled (Stage 3 shipped that as a total ingest blackout).
            lodestone_ecs::ingest::IngestPlugin,
            lodestone_ecs::SessionPlugin,
            // §4.1(c). The render-side entity interpolation, which used to own a
            // second `World` and therefore a second 20 Hz accumulator.
            crate::entities::EntityInterpPlugin,
            // Stage 4: the chunk store and the terrain-mesh queues become
            // resources, and `heal_dirty_columns` becomes an `Update` system in
            // `FrameSet::Terrain`.
            TerrainPlugin,
            // Stage 5: the pick target, the two interaction predictors and the
            // particle emitter become resources, and the sprint edge and the
            // hold-to-mine loop become `TickSet::Send` systems. Added *after*
            // `ControllerPlugin` because it asserts that plugin is present rather
            // than adding it itself — `add_systems` does not deduplicate.
            InteractPlugin,
            // Autonomous navigation (`docs/autonomous-navigation.md`, issue #38):
            // the M1 walk-only plugin. Registration order relative to the rest of
            // this tuple does not matter — its two systems are chained
            // `.after(TickSet::Intent).before(TickSet::Physics)` internally,
            // rather than `.in_set(TickSet::Intent)`, specifically so it never has
            // to be ordered against `compute_movement_intent` by name (see that
            // doc's "Why `.after(TickSet::Intent)`" section) — but that is a claim
            // about the plugin's own `.add_systems` calls, not proof this call
            // site actually reaches them, which is exactly the shape of bug
            // `CLAUDE.md`'s island rule warns about. Adds no systems that fire
            // without an `AutopilotGoal` set, so this is inert for every session
            // until something (a chat command, not yet built) sets one.
            lodestone_autopilot::AutopilotPlugin,
        ));
        let mut ecs = std::mem::take(app.world_mut());
        ecs.insert_resource(Profile(PhysicsProfile::mc_1_21()));
        // Stage 5. `ParticleSim` cannot come from `InteractPlugin`: like the mesh
        // worker pool, the emitter has to be built with the sprite table for
        // whichever block-id space this session's world holds.
        ecs.insert_resource(ParticleSim(particles));
        ecs.insert_resource(VersionData(version_data));
        // `FrameClock` and `WorldTime` come from `CorePlugin` now (§4.1(c) retired
        // the guard that refused to insert them), so there is nothing to seed here.
        // `TerrainPlugin` inserts a *default* (empty) store; this replaces it with
        // the one this session actually meshes. The worker pool cannot come from a
        // plugin at all: it has to be built with the classifier for whichever
        // block-id space that store holds.
        ecs.insert_resource(chunk_world);
        ecs.insert_resource(terrain);
        // Config-scoped, not session-scoped — see `AudioEngine`'s own doc and
        // `Sim::end_session`'s hand-written reset list, which must never gain a
        // line for this: a reconnect must not silence (or re-probe) the audio
        // device.
        ecs.insert_resource(AudioEngine(ShellAudio::from_env()));
        // Physics-walk is the default everywhere, including live: the shell
        // collides against the live client-owned world (see `LiveCollision` /
        // `Sim::tick_collision`), so the player stands on the server's ground.
        // While a column is still streaming in, `PlayerCollision::Pending` holds
        // the player in place rather than letting them fall.
        let local = spawn_local_player(&mut ecs, player);
        // Stage 3's session/HUD half goes on the same entity. Separate from
        // `spawn_local_player` because the two component sets belong to different
        // plugins, and a plugin a harness leaves out must not leave a component
        // its systems never look at behind.
        insert_hud_components(&mut ecs, local);
        // §4.1(c): the shared-fold half goes on the *same* entity too, instead of
        // `lodestone_client::state::SharedState::default` spawning a second
        // `LocalPlayer` in a `World` of its own. This is the entity
        // `attach_net` names to `ClientBuilder::ecs`.
        lodestone_ecs::session::insert_session_components(&mut ecs, local);

        let mut sim = Self {
            config,
            stats,
            ecs: std::sync::Arc::new(lodestone_ecs::parking_lot::RwLock::new(ecs)),
            local,
            net: None,
            adopted_live_world: false,
            status,
            vanilla_atlas: resources.vanilla_atlas,
            particle_atlas,
            language: resources.language,
            teleport_count: 0,
            collide_against_live_world: true,
            asset_banner: resources.banner,
            recover_from_death: true,
            death_message: None,
            won: false,
            third_person: false,
            body_pose: EntityPose::new(feet[0], feet[2], player.yaw, false),
            // Seeded from the spawn pose so the very first frame does not ease up
            // from zero — vanilla's `Camera` is likewise aligned before its first
            // tick, not zero-initialised.
            eye_height_smoother: crate::camera_rig::EyeHeightSmoother::new(player.eye_height),
            view_bob: ViewBob::new(),
            // Vanilla's default. A fresh `Sim` bobs until told otherwise, so a
            // caller that forgets `set_view_bobbing` gets the vanilla behaviour
            // rather than a silently disabled feature.
            view_bobbing: true,
            // Vanilla's defaults (both options default `false` — see
            // `docs/input-options.md`); a caller that forgets the setters gets
            // vanilla's own behaviour, not a silently-inverted or
            // silently-toggling one.
            invert_mouse_x: false,
            invert_mouse_y: false,
            toggle_sneak: false,
            toggle_sprint: false,
            chest_lids: crate::block_entities::ChestLids::new(),
            pickups: lodestone_game::mining::PickupFeed::new(),
        };
        sim.refresh_mesh_policy();
        sim
    }
}
