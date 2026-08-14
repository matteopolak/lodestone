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

/// The `App` a caller composes and [`Sim::from_app`] adopts. Re-exported from
/// `lodestone-app` rather than named through `lodestone_ecs::app` so the type a
/// consumer sees comes from the same crate as the function that builds it.
pub use lodestone_app::App;

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

    /// The rendered client's plugin set, composed into an [`App`] that is **not**
    /// finalised — `add_plugins` on the result, then hand it to
    /// [`Sim::from_app`].
    ///
    /// This is the seam milestone zero of `docs/plans/runtime-plugin-loading.md`
    /// exists to create, and the paragraph that used to sit at the bottom of
    /// `build` explaining why there wasn't one is deleted rather than amended.
    ///
    /// It is [`lodestone_app::client_app`] — the six version-free plugins, shared
    /// with every headless consumer — plus the shell's own three, added through
    /// the identical `add_plugins` call a consumer makes. There is no privileged
    /// composition path: `Sim::new` is literally `client_app()` + `from_app`, so
    /// the shell cannot drift away from what it hands out.
    ///
    /// **Why the shell's three do not live in `lodestone-app`.** Not because they
    /// are render-shaped — none of `mesher.rs`, `interact.rs` or `entities.rs`
    /// names a `wgpu` type, and the dependency arrow already points render→sim
    /// (`sim/render_sources.rs` is "what the renderer pulls out of `Sim` each
    /// frame"). They stay because of *shell-internal* coupling: `mesher.rs` needs
    /// `crate::blocks` and `crate::net`, and `interact.rs` imports fourteen items
    /// from `crate::sim` — a cycle with this very type. `EntityInterpPlugin` alone
    /// is movable and stays only because moving it in isolation buys no gate.
    ///
    /// A headless consumer therefore gets no terrain mesher, no pick target and
    /// no render-side interpolation, which is correct: all three exist to feed a
    /// renderer.
    #[must_use]
    pub fn client_app() -> App {
        let mut app = lodestone_app::client_app();
        app.add_plugins((
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
            // `ControllerPlugin` — which `lodestone_app::client_app` installs —
            // because it asserts that plugin is present rather than adding it
            // itself, and `add_systems` does not deduplicate.
            InteractPlugin,
            // Issue #148: the recipe corpus becomes a resource, so a plugin can
            // register a recipe into the same book the container screen matches
            // against. Installed here rather than in `lodestone_app::client_app`
            // only because the *shell* is what loads `client.jar`'s corpus and
            // adopts it (`WindowApp::adopt_recipe_corpus`); a headless consumer
            // that registers a recipe gets the resource on demand from
            // `RecipeRegistryExt::add_recipe`, so nothing depends on this line
            // for correctness — it guarantees the shell can *read* the registry
            // even when no plugin registered anything.
            lodestone_ecs::RecipeRegistryPlugin,
            // Issue #467: without this line the whole command path is an
            // island. `CHAT_COMMAND` decodes (#464), crosses the host-installed
            // `CommandSink` seam and reaches `dispatch` — but `dispatch` reads a
            // `CommandRegistry` that only `PluginCommandsPlugin` inserts, so
            // with **zero** production registrations no player could run a
            // command however correct the wire was.
            //
            // It goes in `client_app()` specifically, not at the `net.rs` call
            // site where a `World` and an `IntegratedServer` are both in scope:
            // the `App` *value* never reaches there, being consumed by
            // `Sim::from_app` below.
            lodestone_ecs::commands::PluginCommandsPlugin,
        ));
        app
    }

    /// Build a simulation **around an [`App`] the caller composed**, so a
    /// downstream crate can register its own plugins into the *rendered* client.
    ///
    /// ```no_run
    /// # use lodestone::{config::Config, sim::Sim};
    /// let mut app = Sim::client_app();
    /// // app.add_plugins(lodestone_autopilot::AutopilotPlugin);
    /// let sim = Sim::from_app(app, Config::default());
    /// ```
    ///
    /// The `App` must carry at least what [`Sim::client_app`] installs; the
    /// straightforward way to guarantee that is to start from it. Everything the
    /// shell adds after this point is *resources* and one entity, neither of
    /// which needs `Plugin::build` — which is why adoption can happen this late
    /// and why a consumer's plugins are built before any of it.
    ///
    /// Honours `config.mode` exactly as [`Sim::new`] does — `Mode::Headless` gets
    /// the offline demo world, everything else gets the live one — so this is a
    /// drop-in for `new` and not a third construction path with its own rules.
    #[must_use]
    pub fn from_app(app: App, config: Config) -> Self {
        let demo_world = config.mode == Mode::Headless;
        Self::adopt(app, config, demo_world)
    }

    /// The shared constructor. `demo_world` picks between the two mutually
    /// exclusive block-id worlds *and* whether any offline terrain exists at all;
    /// the two must agree, which is why this is one function and not two.
    fn build(config: Config, demo_world: bool) -> Self {
        Self::adopt(Self::client_app(), config, demo_world)
    }

    /// The body of the constructor, given an already-composed [`App`]: build the
    /// session-scoped resources, take the `World`, insert them, spawn the one
    /// entity everything hangs off.
    fn adopt(mut app: App, config: Config, demo_world: bool) -> Self {
        let (world, feet) = if demo_world {
            let radius = (config.render_distance as i32).clamp(1, MAX_WORLD_RADIUS);
            (worldgen::generate(radius), worldgen::spawn_feet())
        } else {
            (World::new(), PRE_SESSION_FEET)
        };

        let mut player = PlayerState::at(Vec3d::new(feet[0], feet[1], feet[2]), 180.0);
        player.pitch = 10.0;

        // Mesh is CPU-bound; use all cores. The saturating_sub(1) was for
        // the old mutex-contended implementation, where an extra worker
        // would just contend harder. With crossbeam MPMC, more workers =
        // strictly more throughput until saturating memory bandwidth.
        let workers = std::thread::available_parallelism()
            .map(|n| n.get().max(1))
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
        // Issue #423: build the write handle and derive the read handle from it,
        // so the resource this session installs pairs two halves of one `Arc`.
        let write_handle = ChunkWorldWrite::new(world);
        let chunk_world = write_handle.read_handle();

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

        // Take the `World` and drop the `App` — azalea's own shape
        // (`azalea-client/src/client.rs:143`), and why nothing here ever calls
        // `App::update`. **This used to be where the plugin boundary closed**, and
        // is not any more: composition moved up into [`Sim::client_app`], so every
        // plugin is already built by the time this line runs and a caller who
        // wants their own has already added it. `Sim` still stores only an
        // `EcsHandle`, which is fine — the `App` was never the thing that needed
        // to survive, only the thing that needed to be *reachable*.
        //
        // **`lodestone_autopilot::AutopilotPlugin` used to be the last entry in
        // that tuple and was removed on purpose** (issue #38, and #77's plugin
        // boundary). The shipped client does not navigate itself: the autopilot is
        // a pre-implemented *external* plugin, so the shell does not depend on it
        // at all — not even optionally behind a feature. `Cargo.toml`'s own note
        // where the dependency line was says the same thing, and
        // `docs/autonomous-navigation.md`'s "Not wired into the shell" section
        // carries the routes a user has to get it back — which now include
        // `Sim::client_app()` + `add_plugins` + `Sim::from_app`, on the rendered
        // client, rather than headless only.
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
        ecs.insert_resource(write_handle);
        ecs.insert_resource(chunk_world);
        ecs.insert_resource(terrain);
        // Config-scoped, not session-scoped — see `AudioEngine`'s own doc and
        // `Sim::end_session`'s hand-written reset list, which must never gain a
        // line for this: a reconnect must not silence (or re-probe) the audio
        // device.
        ecs.insert_resource(AudioEngine(ShellAudio::from_env()));
        // Beside the device, for the same config-scoped reason. Time-seeded to
        // match vanilla, whose `MusicManager` draws from `RandomSource.create()`
        // (itself time-seeded) — a fixed constant here would give every run of the
        // client the identical sequence of gaps between songs. Determinism is not
        // lost where it matters: every gate constructs `ShellMusic::new` with its
        // own explicit seed rather than going through this resource.
        // `crate::platform::epoch_duration`, not `SystemTime::now()`: the latter
        // compiles for wasm32 and TRAPS at runtime, and this runs during `Sim`
        // construction — so it would kill a browser tab before the first frame.
        let audio_seed = crate::platform::epoch_duration().as_nanos() as i64;
        ecs.insert_resource(MusicState(Some(crate::audio::music::ShellMusic::new(
            audio_seed,
        ))));
        // Ambience shares the music's reasoning wholesale — see `AmbienceState`.
        ecs.insert_resource(AmbienceState(Some(
            crate::audio::ambient::ShellAmbience::new(audio_seed),
        )));
        // Physics-walk is the default everywhere, including live: the shell
        // collides against the live client-owned world (see `LiveCollision` /
        // `Sim::tick_collision`), so the player stands on the server's ground.
        // While a column is still streaming in, `PlayerCollision::Pending` holds
        // the player in place rather than letting them fall.
        // The one entity everything hangs off: local-player, HUD and session
        // component sets, three separate inserts because they belong to three
        // different plugins and a plugin a harness leaves out must not leave a
        // component its systems never look at behind. §4.1(c) is why the
        // shared-fold half goes on the *same* entity rather than
        // `lodestone_client::state::SharedState::default` spawning a second
        // `LocalPlayer` in a `World` of its own; this is the entity `attach_net`
        // names to `ClientBuilder::ecs`.
        //
        // Through `lodestone_app` rather than the three calls inline, so the
        // rendered client and every headless consumer spawn the *same* entity
        // shape — a divergence here would be invisible until a fold wrote a
        // component nothing had.
        let local = lodestone_app::spawn_session_in(&mut ecs, player);

        // Issue #443: read before the literal, because `config` moves into the
        // `config` field below and struct-literal fields evaluate in written
        // order.
        let seed_sensitivity = config.sensitivity;

        let mut sim = Self {
            config,
            stats,
            ecs: std::sync::Arc::new(lodestone_ecs::parking_lot::RwLock::new(ecs)),
            local,
            net: None,
            adopted_live_world: false,
            status,
            connect_phase: crate::menu::loading::ConnectPhase::default(),
            expected_view_columns: None,
            expected_view_radius: None,
            terrain_wait_started: None,
            vanilla_atlas: resources.vanilla_atlas,
            particle_atlas,
            language: resources.language,
            // Seeded to the *current* generation, not `0`: `resources` above
            // already reflects whatever pack selection was live the moment
            // this session was built, so treating that as "already seen"
            // is what stops the very first frame from redundantly redoing
            // the reload `Sim::reload_resource_pack_atlas` exists for.
            last_pack_generation: crate::resources::pack_generation(),
            teleport_count: 0,
            collide_against_live_world: true,
            asset_banner: resources.banner,
            warned_id_space_mismatch: false,
            recover_from_death: true,
            death_message: None,
            won: false,
            lan_published: false,
            // No dimension until a server names one. `None` is what makes the first
            // `Respawned` of a session a *baseline* rather than a change — see
            // `Sim::apply_respawn`.
            applied_dimension: None,
            portal_effect_intensity: 0.0,
            prev_portal_effect_intensity: 0.0,
            // `CameraType::FIRST_PERSON`, vanilla's own `Options` default.
            camera_type: crate::camera_rig::CameraType::default(),
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
            // Vanilla's defaults (all six options default `false` — see
            // `docs/input-options.md`); a caller that forgets the setters gets
            // vanilla's own behaviour, not a silently-inverted or
            // silently-toggling one.
            invert_mouse_x: false,
            invert_mouse_y: false,
            // Issue #443: seeded from the argv-derived `Config` so a caller
            // that never calls `set_sensitivity` — a headless bot, a test —
            // behaves exactly as it did before the field existed. The menu
            // layer overwrites it every frame once there is a menu.
            sensitivity: seed_sensitivity,
            // Issues #202/#444: hold-vs-toggle and auto-jump all default off
            // in vanilla; the sprint window boots at vanilla's shipped 7
            // (`Options.java`), because a derived `0` would silently
            // disable double-tap sprint for any caller that never calls
            // `set_sprint_window_ticks`.
            toggle_sneak: false,
            toggle_sprint: false,
            toggle_attack: false,
            toggle_use: false,
            auto_jump: false,
            sprint_window_ticks: lodestone_controller::SPRINT_TRIGGER_WINDOW_TICKS,
            first_chunk_at: None,
            chest_lids: crate::block_entities::ChestLids::new(),
            bell_shakes: crate::block_entities::BellShakes::new(),
            enchanting_table_books: crate::block_entities::EnchantingTableBooks::new(),
            moving_pistons: crate::block_entities::PistonMoves::new(),
            // Vanilla's default, not `0.0` — see the field's doc.
            damage_tilt_strength: 1.0,
            // Vanilla's own `70`, so a `Sim` nobody pushes an FOV into builds the
            // same projection `build_camera` used to hardcode.
            fov_y_degrees: crate::camera_rig::FOV_Y_DEGREES,
            pickups: lodestone_game::mining::PickupFeed::new(),
            pending_sign_edit: None,
        };
        sim.refresh_mesh_policy();
        sim
    }
}
