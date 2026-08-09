//! **What the renderer pulls out of `Sim` each frame**: the four block-entity
//! `'static` sampler closures, the entity draws, the crack overlays and the
//! particle tick/extract pair -- seam 13 of the sim.rs decomposition
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
//! Every item here is read by `crate::gpu` or by `app.rs`'s per-frame render
//! reconciliation, and they share one shape worth keeping in one file: each
//! either hands out a closure that must be `'static + Send + Sync` (so it
//! captures an owned `SharedHandle` and re-samples per frame, never `&self`)
//! or hands out owned data (so no `World` guard escapes into a GPU upload).
//! `block_entity_source`'s "two snapshots, both deliberate" note and
//! `particle_instances`'s "owned rather than borrowed" note are the same
//! constraint stated twice; `extract_particles`'s long comment records what
//! happened when it was violated -- the longest `World` guard hold in the
//! process, scaling with particle count.
//!
//! `tick_particles` sits here rather than in `sim/step.rs` because it is the
//! producer half of the same emitter `extract_particles` consumes, and the
//! two share the non-obvious `with_particles_unlocked` lock discipline: move
//! the emitter *out* of the `World`, work with no guard held at all, move it
//! back. Splitting them would leave that rule documented on one side of a
//! file boundary and relied on from the other.
//!
//! # What widened
//!
//! `tick_particles` alone, private -> `pub(crate)`: [`Sim::step`] calls it
//! once per tick from `sim/step.rs`, a sibling. Everything else here was
//! already `pub`, since the renderer is outside `sim` entirely.
//!
//! `use super::*;` for the same reason every earlier seam file uses it: this
//! module is a *descendant* of `sim`, so it already has the same visibility
//! into `Sim`'s private fields, into `sim.rs`'s remaining private helpers and
//! into everything `sim.rs` re-exports that `sim::tests` has always had, with
//! no need to enumerate any of it.

use super::*;

impl Sim {
    /// A `'static` sampler of the **outline** boxes of the block at a world
    /// position, for `RenderState::set_outline_shape_source`.
    ///
    /// `None` when this session cannot answer: no live connection, no vanilla
    /// atlas (the demo palette has no outline census and is all full cubes, which
    /// is what an empty result already means), or no version family compiled in
    /// for the configured protocol.
    ///
    /// # Why this is not `CollisionSource`, which is what the plan expected
    ///
    /// Stage 2's [`CollisionSource`] hands out a `CollisionView`, whose geometry
    /// is the **collision** shape. The selection box needs the **outline** shape,
    /// and those are a different vanilla shape family: kelp has an outline and no
    /// collision, cobweb's outline is a full cube while its collision is empty, and
    /// **half of all 26.2 block states have an outline that differs from their
    /// collision shape** (`VersionAdapter::block_outline`'s docs). Wiring
    /// `CollisionSource` here would replace one wrong box with a differently wrong
    /// box in half of all cases, which is worse than a unit cube because it would
    /// look right.
    ///
    /// # Why this did not need Stage 4 either
    ///
    /// The brief listed the selection box as blocked on the chunk-world
    /// unification. It was not: everything the closure needs was already `'static`
    /// and `Send + Sync` — `NetClient::shared_handle` is an
    /// `Arc<OnceLock<Arc<ClientHandle>>>`, `ClientHandle::block_at` is public, and
    /// `VersionAdapter` is declared `Send + Sync + Debug` at
    /// `lodestone-model/src/adapter.rs:391`. Capturing the *handle* rather than the
    /// store is also what makes this installable before login, when there is no
    /// store to capture yet.
    ///
    /// A second boxed adapter is minted rather than sharing
    /// [`Self::version_data`]: adapters are stateless value types, so the copy
    /// costs a `Box` and answers identically — the same reasoning `version_data`'s
    /// own doc records for why it is already a second instance.
    #[must_use]
    pub fn outline_shape_source(
        &self,
    ) -> Option<impl Fn([i32; 3]) -> Vec<lodestone_physics::Aabb> + Send + Sync + 'static> {
        self.vanilla_atlas.as_ref()?;
        let handle = self.net.as_ref()?.shared_handle();
        let adapter = lodestone_registry::adapter_for_protocol(self.config.protocol)?;
        Some(move |block: [i32; 3]| {
            let Some(client) = handle.get() else {
                return Vec::new();
            };
            let Some(state) = client.block_at(BlockPos {
                x: block[0],
                y: block[1],
                z: block[2],
            }) else {
                return Vec::new();
            };
            let Some(boxes) = adapter.block_outline(state) else {
                return Vec::new();
            };
            // The census is block-local `0..1`; the renderer wants world space.
            boxes
                .iter()
                .map(|b| {
                    lodestone_physics::Aabb::new(
                        f64::from(block[0]) + f64::from(b.min[0]),
                        f64::from(block[1]) + f64::from(b.min[1]),
                        f64::from(block[2]) + f64::from(b.min[2]),
                        f64::from(block[0]) + f64::from(b.max[0]),
                        f64::from(block[1]) + f64::from(b.max[1]),
                        f64::from(block[2]) + f64::from(b.max[2]),
                    )
                })
                .collect()
        })
    }

    /// This frame's block entities (chests, issue #23) as a `'static` closure for
    /// [`RenderState::set_block_entity_source`].
    ///
    /// `None` without a live session — the offline demo world has no chests, and a
    /// closure that always returned an empty vec would look installed while
    /// carrying nothing.
    ///
    /// # Why this is not gated on `vanilla_atlas`
    ///
    /// [`Self::outline_shape_source`] is, because an outline is only meaningful
    /// against real block states. This is not, and the difference matters: the
    /// chest sheets are loaded by the *renderer* from its own jar lookup, so a
    /// session with a live world but no stitched atlas still draws chests
    /// correctly. Copying the atlas gate here would silently switch chests off in
    /// exactly the configuration that most needs them visible.
    ///
    /// # Two snapshots, both deliberate
    ///
    /// The lid map is **cloned** and the partial tick **sampled** rather than
    /// borrowed, because the closure outlives this call (`RenderState` owns it)
    /// and must not hold `&self`. The clone is one small `HashMap` — it holds only
    /// chests that are open or moving, since
    /// [`ChestLids::tick`](crate::block_entities::ChestLids::tick) drops the
    /// settled-shut — and re-taking it every frame is what makes the animation
    /// move at all. Installing this once at connect freezes every lid at the
    /// fraction of a tick it was installed on.
    ///
    /// [`RenderState::set_block_entity_source`]: crate::gpu::RenderState::set_block_entity_source
    #[must_use]
    pub fn block_entity_source(
        &self,
    ) -> Option<impl Fn(glam::Vec3) -> Vec<lodestone_render::ChestSpawn> + Send + Sync + 'static>
    {
        let handle = self.net.as_ref()?.shared_handle();
        let lids = self.chest_lids.clone();
        let partial_tick = self.clock().interp_alpha;
        Some(move |eye: glam::Vec3| {
            crate::block_entities::chest_spawns(&handle, &lids, eye, partial_tick)
        })
    }

    /// The skull/head sibling of [`Self::block_entity_source`], for
    /// [`RenderState::set_skull_source`](crate::gpu::RenderState::set_skull_source).
    ///
    /// Unlike the chest source this captures **no partial tick and no animation
    /// state**: none of the five ported skull types animate, so there is nothing
    /// whose interpolation could freeze at the fraction of a tick the closure was
    /// installed on. That asymmetry is the whole reason these are two sources
    /// rather than one closure returning a pair.
    #[must_use]
    pub fn skull_source(
        &self,
    ) -> Option<impl Fn(glam::Vec3) -> Vec<lodestone_render::SkullSpawn> + Send + Sync + 'static>
    {
        let handle = self.net.as_ref()?.shared_handle();
        Some(move |eye: glam::Vec3| crate::block_entities::skull_spawns(&handle, eye))
    }

    /// The sign sibling of [`Self::skull_source`] — see
    /// `crate::block_entities::sign_spawns`. Captures no partial tick and no
    /// animation state, for the same reason skulls do not: sign text does not
    /// animate.
    #[must_use]
    pub fn sign_source(
        &self,
    ) -> Option<impl Fn(glam::Vec3) -> Vec<lodestone_render::SignSpawn> + Send + Sync + 'static>
    {
        let handle = self.net.as_ref()?.shared_handle();
        Some(move |eye: glam::Vec3| crate::block_entities::sign_spawns(&handle, eye))
    }

    /// The bell sibling of [`Self::block_entity_source`] — see
    /// `crate::block_entities::bell_spawns`.
    ///
    /// Like the chest source and **unlike** skull/sign, this captures the shake
    /// tracker *and* the partial tick, because a bell animates. The same warning
    /// applies: this must be re-installed every frame, or every shake freezes at
    /// the fraction of a tick the closure was built on.
    #[must_use]
    pub fn bell_source(
        &self,
    ) -> Option<impl Fn(glam::Vec3) -> Vec<lodestone_render::BellSpawn> + Send + Sync + 'static>
    {
        let handle = self.net.as_ref()?.shared_handle();
        let shakes = self.bell_shakes.clone();
        let partial_tick = self.clock().interp_alpha;
        Some(move |eye: glam::Vec3| {
            crate::block_entities::bell_spawns(&handle, &shakes, eye, partial_tick)
        })
    }

    /// This frame's shulker boxes, for
    /// [`RenderState::set_shulker_source`](crate::gpu::RenderState::set_shulker_source).
    ///
    /// The simplest source of the family: no clock, no per-frame animation state,
    /// and no partial tick — a shulker box's facing and dye colour both come off
    /// its block state, and its lid animation is not wired (see
    /// [`lodestone_render::ShulkerSpawn::progress`]). Installed per frame anyway,
    /// for [`Self::skull_source`]'s reason: a source that outlived a disconnect
    /// would hand out spawns from a dead world's handle.
    pub fn shulker_source(
        &self,
    ) -> Option<impl Fn(glam::Vec3) -> Vec<lodestone_render::ShulkerSpawn> + Send + Sync + 'static>
    {
        let handle = self.net.as_ref()?.shared_handle();
        Some(move |eye: glam::Vec3| crate::block_entities::shulker_spawns(&handle, eye))
    }

    /// This frame's lectern books, for
    /// [`RenderState::set_lectern_source`](crate::gpu::RenderState::set_lectern_source).
    ///
    /// As thin as [`Self::shulker_source`], and for a stronger reason: a lectern
    /// book's pose is a compile-time constant in the jar, so there is nothing a
    /// clock could even feed it. Installed per frame anyway, for
    /// [`Self::skull_source`]'s reason — a source that outlived a disconnect
    /// would hand out spawns from a dead world's handle.
    #[must_use]
    pub fn lectern_source(
        &self,
    ) -> Option<impl Fn(glam::Vec3) -> Vec<lodestone_render::LecternSpawn> + Send + Sync + 'static>
    {
        let handle = self.net.as_ref()?.shared_handle();
        Some(move |eye: glam::Vec3| crate::block_entities::lectern_spawns(&handle, eye))
    }

    /// This frame's campfire cooking items, for
    /// [`RenderState::set_campfire_source`](crate::gpu::RenderState::set_campfire_source).
    ///
    /// Clock-free like [`Self::lectern_source`]: `CampfireRenderer` has no
    /// animation of any kind, so there is nothing a partial tick could feed. The
    /// per-frame install is [`Self::skull_source`]'s reason only — a source that
    /// outlived a disconnect would hand out spawns from a dead world's handle.
    #[must_use]
    pub fn campfire_source(
        &self,
    ) -> Option<
        impl Fn(glam::Vec3) -> Vec<lodestone_render::CampfireItemSpawn> + Send + Sync + 'static,
    > {
        let handle = self.net.as_ref()?.shared_handle();
        Some(move |eye: glam::Vec3| crate::block_entities::campfire_spawns(&handle, eye))
    }

    /// This frame's banners, for
    /// [`RenderState::set_banner_source`](crate::gpu::RenderState::set_banner_source).
    ///
    /// Captures the game tick *and* the partial tick, like `bell_source` and unlike
    /// `shulker_source`, because `banner_phase` mixes both into the sway — so this
    /// must be re-installed every frame or every banner freezes.
    #[must_use]
    pub fn banner_source(
        &self,
    ) -> Option<impl Fn(glam::Vec3) -> Vec<lodestone_render::BannerSpawn> + Send + Sync + 'static>
    {
        let net = self.net.as_ref()?;
        let handle = net.shared_handle();
        // `.0` is `game_time`, vanilla's `level.getGameTime()`; `.1` is
        // `time_of_day`, which wraps every day and would make the sway jump at
        // dawn.
        let game_time = handle.get().map_or(0, |c| c.world_time().0);
        let partial_tick = self.clock().interp_alpha;
        Some(move |eye: glam::Vec3| {
            crate::block_entities::banner_spawns(&handle, eye, game_time, partial_tick)
        })
    }

    /// This frame's filled-map picture, for
    /// [`RenderState::set_map_source`](crate::gpu::RenderState::set_map_source)
    /// (issue #184).
    ///
    /// Takes an optional map id and yields that map's raw 128×128 packed colour
    /// grid, cloned out of [`SessionMaps`](lodestone_ecs::session::SessionMaps).
    ///
    /// # Why the id is optional, and what to change when it stops being
    ///
    /// **`minecraft:map_id` is not decoded**, so a `filled_map` in hand or in a
    /// frame carries no id we can read: `ItemComponents` has no field for it and
    /// `read_component_patch`'s `other =>` arm cannot skip its payload, so the
    /// component actually truncates the rest of that packet. Until it is modelled
    /// alongside `minecraft:trim`, `None` means "the lowest-numbered map the
    /// server has sent" — exactly right in the overwhelmingly common
    /// one-map-in-inventory case, and the wrong picture when a player carries two.
    /// The moment the component decodes, callers pass `Some(id)` and this needs no
    /// change at all.
    #[must_use]
    pub fn map_source(
        &self,
    ) -> Option<impl Fn(Option<i32>) -> Option<Vec<u8>> + Send + Sync + 'static> {
        // Off a live server the store is always empty, so a source would only
        // ever answer `None`; skip it as the block-entity sources do.
        self.net.as_ref()?;
        let store = self.maps();
        Some(move |id: Option<i32>| {
            let id = id.or_else(|| store.ids().next())?;
            Some(store.get(id)?.colors.clone())
        })
    }

    /// How many chest lids are currently animating or open — for the debug
    /// overlay and for the live gate, which needs to distinguish "the block event
    /// never arrived" from "the lid is drawn shut".
    #[must_use]
    pub fn chest_lid_count(&self) -> usize {
        self.chest_lids.len()
    }

    /// How many bells are currently shaking — the bell sibling of
    /// [`Self::chest_lid_count`], and for the same reason: it distinguishes "the
    /// block event never arrived" from "the bell is drawn at rest".
    #[must_use]
    pub fn bell_shake_count(&self) -> usize {
        self.bell_shakes.len()
    }

    /// The interpolated entities to draw this frame, resolved by the renderer
    /// into instanced draws. Empty off a live server.
    #[must_use]
    pub fn entity_draws(&self) -> Vec<EntityDraw> {
        self.read(crate::entities::extracted_entity_draws)
    }

    /// The progressive-mining crack to draw on the targeted block this frame, or
    /// `None` when no dig is in progress.
    ///
    /// The stage is the client predictor's own `getDestroyStage` (`0..=9`); the
    /// block state id must be in the *same* id space the model atlas was built
    /// from, so on a live server it is read from the client-owned world
    /// (`NetClient::block_at`) — not [`block_at_world`](Self::block_at_world),
    /// which reads the offline demo world and would return air on a live join,
    /// leaving the resolver with no faces and drawing no crack. Progressive
    /// mining only runs on the live path (demo attack is a one-shot break that
    /// never drives the predictor), so `mining.destroy_stage()` is `-1` off a
    /// server and this returns `None` there regardless.
    ///
    /// The stage advances at the block's *own* rate: the predictor is fed the
    /// version's real per-state hardness (see
    /// [`drive_mining`](Self::drive_mining)), so the ten stages fill smoothly
    /// over the true break time and obsidian visibly crawls where dirt flickers
    /// past. An unbreakable block (`hardness == -1.0`, bedrock/barrier) has
    /// `progress_per_tick() == 0.0`, so progress never leaves `0.0`,
    /// `destroy_stage()` stays `-1` and this returns `None` — no crack is drawn
    /// at all, matching vanilla.
    #[must_use]
    pub fn crack_target(&self) -> Option<crate::gpu::CrackTarget> {
        let stage = self.mining(Mining::destroy_stage);
        if stage < 0 {
            return None;
        }
        let block = self.target()?.block;
        let state_id = if self.is_live() {
            let pos = BlockPos::new(block[0], block[1], block[2]);
            self.net.as_ref()?.block_at(pos)?
        } else {
            self.block_at_world(block)
        };
        Some(crate::gpu::CrackTarget {
            block,
            state_id,
            stage: (stage as u8).min(9),
        })
    }

    /// Every crack overlay to draw this frame (issue #410): the local
    /// player's own dig via [`Self::crack_target`], plus one
    /// [`crate::gpu::CrackTarget`] for every *other* player's active overlay
    /// in [`SessionBlockDestruction`], folded from `ClientEvent::BlockDestruction`
    /// by `lodestone_ecs::session::apply_block_destruction`.
    ///
    /// This is the accessor the #410 report's own gate (`gathers_local_plus_
    /// every_other_players_overlay` in `gpu/outline.rs`) proved the pipeline
    /// side of but that nothing in production called: `crate::gpu::
    /// gather_crack_targets` and `BlockDestructionOverlays::iter` both landed
    /// closing the issue, but the issue closed with only the local target
    /// ever reaching `app.rs`'s `cracks` vec — this is the missing hop.
    ///
    /// `overlays` is cloned out of the read guard before resolving each
    /// position's state id: `resolve` below takes `self.net`/`self.
    /// block_at_world` reads of its own, and holding the `SessionBlockDestruction`
    /// guard across those is the same nested-lock hazard [`Self::fold_entities`]'s
    /// doc warns about for entity snapshots.
    #[must_use]
    pub fn crack_targets(&self) -> Vec<crate::gpu::CrackTarget> {
        let overlays = self.read(|w| {
            w.get::<SessionBlockDestruction>(self.local)
                .expect("the local player always carries SessionBlockDestruction")
                .0
                .clone()
        });
        let is_live = self.is_live();
        crate::gpu::gather_crack_targets(self.crack_target(), overlays.iter(), |pos| {
            if is_live {
                self.net.as_ref()?.block_at(pos)
            } else {
                Some(self.block_at_world([pos.x, pos.y, pos.z]))
            }
        })
    }

    /// Advance the particle simulation one 20 Hz tick.
    ///
    /// Particles collide against the same view the player does, so debris rests
    /// on the terrain it fell onto rather than sinking through it. On the live
    /// path the column may not have streamed in; vanilla ticks particles
    /// regardless, so an absent view falls back to the offline world rather than
    /// freezing them.
    pub(crate) fn tick_particles(&mut self) {
        // The eye, for the ambient scan below. Read before either guard is taken.
        let player = self.player();
        let eye = [player.position.x, player.position.y, player.position.z];
        if self.vanilla_atlas.is_some() && self.net.is_some() && self.collide_against_live_world {
            if let Some(view) = self.live_collision() {
                // `O(live particles)`, so the emitter comes out of the `World`
                // first — the same reason `extract_particles` does it.
                //
                // The ambient scan (issue #178) rides the *same* snapshot rather
                // than taking a second lock: it is a bounded number of block
                // probes, and this is the one place per tick that already holds a
                // block view with no `World` guard over it.
                self.with_particles_unlocked(|p| {
                    p.tick(&view);
                    p.ambient_tick(eye, &mut |b| view.block_at(b[0], b[1], b[2]));
                });
                return;
            }
        }
        // The chunk guard is taken *inside* `f`, i.e. with no `World` guard held,
        // so the two are never held simultaneously and there is no order to get
        // wrong. This used to be written inside-out (`World` guard outside, chunk
        // guard inside) to obey `EcsHandle`'s rule 3, because the obvious spelling
        // — take the chunk read guard, then reach for the emitter — was
        // `chunks → World`, the one order that can ABBA against the net thread.
        // Holding neither across the other retires that hazard rather than
        // navigating it.
        let store = self.chunk_world();
        self.with_particles_unlocked(|p| {
            let world = store.read();
            p.tick(&WorldCollision::new(&world));
            p.ambient_tick(eye, &mut |b| {
                let pos = lodestone_world::ChunkPos {
                    x: b[0].div_euclid(16),
                    z: b[2].div_euclid(16),
                };
                world
                    .get(pos)
                    .filter(|c| b[1] >= c.column.min_y() && b[1] < c.column.max_y())
                    .map_or(0, |c| {
                        lodestone_world::BlockVolume::block(
                            &c.column,
                            b[0].rem_euclid(16) as usize,
                            b[1],
                            b[2].rem_euclid(16) as usize,
                        )
                    })
            });
        });
    }

    /// Rebuild this frame's particle instances for `camera` and report what
    /// happened, so a silent "simulating fine, drawing nothing" is visible in
    /// the HUD rather than invisible.
    pub fn extract_particles(&mut self, camera: &Camera) -> ParticleFrame {
        // The same alpha every other interpolated draw uses, rather than a
        // second computation of it -- two frame alphas that drift apart show up
        // as particles lagging the terrain by a fraction of a tick.
        let partial = self.clock().interp_alpha;
        // Light is sampled from the live world when there is one. A `None` here
        // is not darkness: `ParticleEngine::extract` substitutes full sky light,
        // matching how the demo terrain is meshed.
        let light: Box<dyn Fn(i32, i32, i32) -> Option<u32>> = match self.net.as_ref() {
            Some(net) => {
                let dims = net.world_dimensions();
                // An **owned** `SharedHandle` (an `Arc<OnceLock<_>>`), not a borrow
                // of `self.net`. That is what lets the whole extract go through
                // `with_particles_unlocked`: a closure borrowing `self` cannot be
                // passed to a `&mut self` method, which is exactly why this
                // function used to take the write guard by hand and hold it across
                // every per-particle light lookup.
                let handle = net.shared_handle();
                // The dimension's absent-sky-light policy, read per sample from the
                // cell `refresh_mesh_policy` publishes into. Same reason
                // `net::entity_light_at` takes one: `sky_at` resolves
                // `LightData::Missing` to **0**, so a particle in open air above the
                // top of the lit column used to come out unlit and near-black. A
                // captured value would go stale on a portal.
                let sky_policy = net.shared_sky_default();
                Box::new(move |x, y, z| {
                    let dims = dims?;
                    let section = (y - dims.min_y).div_euclid(16);
                    if section < 0 || section >= dims.section_count() as i32 {
                        return None;
                    }
                    // `sections_and_light_at` takes `lodestone_client::ChunkPos`,
                    // which is a *different type* from the `lodestone_world`
                    // one imported at the top of this file (see mesher.rs:224).
                    let pos = lodestone_client::ChunkPos {
                        x: x.div_euclid(16),
                        z: z.div_euclid(16),
                    };
                    // Light section `i` covers block section `i-1`, so a caller
                    // for block section `n` asks for light section `n+1`. This
                    // offset is deliberate, not a bug to "align".
                    let got = handle.get()?.sections_and_light_at(&[(
                        pos,
                        section as usize,
                        section as usize + 1,
                    )]);
                    let (_, light) = got.into_iter().next()?;
                    let light = light?;
                    let ly = (y - dims.min_y).rem_euclid(16) as usize;
                    let lx = x.rem_euclid(16) as usize;
                    let lz = z.rem_euclid(16) as usize;
                    // Through the same adapter the terrain draw uses, so absent sky
                    // data gets the dimension's default rather than `sky_at`'s bare
                    // `0`. Not a second `match` restating 15 — one expression.
                    let resolved =
                        lodestone_render::WorldSectionLight::new(&light, sky_policy.get());
                    // Vanilla's `LightTexture.pack`: block light at bit 4, sky
                    // light at bit 20. The particle shader reproduces the
                    // terrain term `0.2 + 0.8 * max(sky, block)` from these.
                    Some(
                        u32::from(resolved.block_light(lx, ly, lz)) << 4
                            | u32::from(resolved.sky_light(lx, ly, lz)) << 20,
                    )
                })
            }
            None => Box::new(|_, _, _| None),
        };
        // **This used to be the longest `World` guard hold in the process.** It
        // took the write guard by hand and held it across the whole extract *and*
        // every per-particle invocation of `light` above — one chunk-store lock
        // acquisition per live particle, with the `World` write-locked throughout.
        // That was order-legal (`World → chunks`, rule 3) and unbounded: the hold
        // grew with particle volume, i.e. precisely during rain and mass block
        // breaks, and per `lodestone_ecs::EcsHandle` an ingest write waits behind
        // it while the driver task that owns the socket is blocked.
        //
        // Now the emitter leaves the `World` first, so `light` is called with no
        // guard held and the hold is two resource moves regardless of particle
        // count. Measured, not argued —
        // `extract_particles_does_not_hold_the_world_guard_across_the_per_particle_work`
        // bounds it against the call's own wall time, and its negative control
        // reproduces the shape above and fails that bound.
        self.with_particles_unlocked(|p| p.extract(camera, partial, &light))
    }

    /// This frame's particle instances, ready for upload.
    ///
    /// Owned rather than borrowed since §4.1(c). The alternative — handing back a
    /// mapped read guard — would keep the one `World` read-locked for the whole GPU
    /// upload, which is exactly the "ingest stalls the frame" failure this change
    /// has to avoid, only inverted: the frame would stall ingest. A `memcpy` of a
    /// few thousand POD instances is the cheaper end of that trade.
    #[must_use]
    pub fn particle_instances(&self) -> Vec<ParticleInstance> {
        self.read(|w| w.resource::<ParticleSim>().0.instances().to_vec())
    }
}
