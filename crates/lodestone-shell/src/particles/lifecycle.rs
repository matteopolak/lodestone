//! Particle engine construction, ticking, sprite resolution, and frame extraction.

use super::*;
impl Particles {
    /// Build the simulation. `models`, when present, supplies each block state's
    /// `#particle` sprite; without it terrain particles still *simulate* but
    /// resolve to nothing and are counted as unresolved.
    ///
    /// Sheet-sourced particles (smoke, flame, crits, splashes, …) start
    /// unresolved regardless — attach a stitched atlas with
    /// [`Self::with_particle_atlas`] to resolve those too.
    ///
    /// `models` also supplies each state's **particle tint**
    /// ([`BlockModels::particle_tint`]). Without it every state is untinted,
    /// which is correct for the demo palette (no colormaps) and wrong for the
    /// vanilla one — untinted foliage debris renders white.
    #[must_use]
    pub fn new(models: Option<&BlockModels>) -> Self {
        let (state_uv, state_tint) = match models {
            Some(m) => (
                (0..m.state_count() as u32)
                    .map(|raw| StateId::new(raw).and_then(|state| m.particle_uv(state)))
                    .collect(),
                (0..m.state_count() as u32)
                    .map(|raw| {
                        StateId::new(raw)
                            .and_then(|state| m.particle_tint(state))
                            .unwrap_or([1.0; 3])
                    })
                    .collect(),
            ),
            None => (Vec::new(), Vec::new()),
        };
        Self {
            // `seeded` from `crate::platform::epoch_duration`, NOT `ParticleEngine::new()`.
            //
            // `new()` seeds itself with `JavaRandom::from_entropy()`, which calls
            // `SystemTime::now()` — and that **traps** on wasm32. This was the first
            // thing to kill the browser tab once the shell actually booted: the console
            // read "assets ready … starting the shell …" and then `time not implemented
            // on this platform`, from three crates down. `cargo check --target
            // wasm32-unknown-unknown` was exit 0 the whole time.
            //
            // Seeding from the caller keeps `lodestone-particle` clock-free and
            // dependency-free rather than giving a leaf crate a portable-time
            // dependency — the same shape `sim::build` already uses for the audio seed.
            engine: ParticleEngine::seeded(crate::platform::epoch_duration().as_nanos() as i64),
            state_uv: Arc::new(state_uv),
            state_tint: Arc::new(state_tint),
            item_uv: Arc::new(models.map(item_uv_table).unwrap_or_default()),
            sheet_uv: Arc::new(HashMap::new()),
            quads: Vec::new(),
            instances: Vec::new(),
            last: ParticleFrame::default(),
        }
    }

    /// Attaches (or clears, with `None`) the stitched particle-sheet atlas
    /// that resolves [`SpriteSource::Sheet`] particles — smoke, flame, crits,
    /// splashes, and the rest of `lodestone_particle::Sheet`.
    ///
    /// Every `(Sheet, frame)` UV rect is precomputed here rather than looked
    /// up per-particle per-frame, mirroring how `state_uv` precomputes one
    /// entry per block state at construction: `Sheet::all()` names every sheet
    /// this crate can ever emit, so the whole table is small (one entry per
    /// physical frame across all ten sheets) and static once built.
    ///
    /// Building the atlas itself (reading `client.jar`, discovering
    /// `particles/*.json`) is the caller's job — this module only consumes an
    /// already-built [`ParticleAtlas`], the same separation `BlockModels`
    /// gets in [`Self::new`].
    #[must_use]
    pub fn with_particle_atlas(mut self, atlas: Option<&ParticleAtlas>) -> Self {
        self.sheet_uv = Arc::new(atlas.map(sheet_uv_table).unwrap_or_default());
        self
    }

    /// Build the simulation over the offline demo palette, whose sprites are
    /// indexed per block rather than per baked model.
    ///
    /// The demo block table has no `#particle` variable, so the closest faithful
    /// stand-in is the **bottom** face sprite. That is not an arbitrary pick: it
    /// reproduces vanilla's answer for the one block where the choice is
    /// visible, since `grass_block` declares `"particle": "block/dirt"` and its
    /// bottom face is dirt. For a uniformly-textured block every face agrees, so
    /// the rule is right there too.
    ///
    /// `uv_table` is [`crate::blocks::AtlasData::uv_table`], whose entries are
    /// `[u_min, v_min, u_size, v_size]` — an origin-plus-size form, unlike the
    /// baked models' min/max corners, so it is converted here rather than at the
    /// sample site.
    #[must_use]
    pub fn with_demo_palette(uv_table: &[[f32; 4]]) -> Self {
        let mut state_uv: Vec<Option<[f32; 4]>> = Vec::new();
        for id in 0..64u32 {
            let uv = crate::blocks::block(id)
                .and_then(|b| uv_table.get(b.sprites[2] as usize))
                .map(|r| [r[0], r[1], r[0] + r[2], r[1] + r[3]]);
            state_uv.push(uv);
        }
        Self {
            // `seeded` from `crate::platform::epoch_duration`, NOT `ParticleEngine::new()`.
            //
            // `new()` seeds itself with `JavaRandom::from_entropy()`, which calls
            // `SystemTime::now()` — and that **traps** on wasm32. This was the first
            // thing to kill the browser tab once the shell actually booted: the console
            // read "assets ready … starting the shell …" and then `time not implemented
            // on this platform`, from three crates down. `cargo check --target
            // wasm32-unknown-unknown` was exit 0 the whole time.
            //
            // Seeding from the caller keeps `lodestone-particle` clock-free and
            // dependency-free rather than giving a leaf crate a portable-time
            // dependency — the same shape `sim::build` already uses for the audio seed.
            engine: ParticleEngine::seeded(crate::platform::epoch_duration().as_nanos() as i64),
            state_uv: Arc::new(state_uv),
            // The demo palette has no colormaps and no tinted blocks, so every
            // demo id is genuinely untinted — an empty table, which
            // `state_tint_of` reads as `[1.0; 3]`.
            state_tint: Arc::new(Vec::new()),
            // The demo palette has no item models at all, so an item crumb is
            // counted as unresolved rather than drawing a block texel.
            item_uv: Arc::new(Vec::new()),
            sheet_uv: Arc::new(HashMap::new()),
            quads: Vec::new(),
            instances: Vec::new(),
            last: ParticleFrame::default(),
        }
    }

    /// The engine, for emitters that need direct access.
    pub fn engine_mut(&mut self) -> &mut ParticleEngine {
        &mut self.engine
    }

    /// The last frame's extraction report.
    #[must_use]
    pub fn frame(&self) -> ParticleFrame {
        self.last
    }

    pub fn tick(&mut self, view: &dyn CollisionView) {
        self.engine.tick(view);
    }

    /// This frame's extracted instances, ready for upload.
    #[must_use]
    pub fn instances(&self) -> &[ParticleInstance] {
        &self.instances
    }

    /// Rebuild the GPU instance list for this frame. `light` returns packed
    /// block/sky light coords at a block position, matching
    /// [`ParticleEngine::extract`].
    pub fn extract(
        &mut self,
        camera: &Camera,
        partial_tick: f32,
        light: &dyn Fn(i32, i32, i32) -> Option<u32>,
    ) -> ParticleFrame {
        self.quads.clear();
        self.instances.clear();
        let eye = Vec3d::new(
            f64::from(camera.position.x),
            f64::from(camera.position.y),
            f64::from(camera.position.z),
        );
        self.engine
            .extract(eye, partial_tick, light, &mut self.quads);

        let mut unresolved = 0usize;
        let mut sheet_drawn = 0usize;
        // The first sprite that failed to resolve this frame, so the debug line
        // below can *name* it. An `unresolved` count alone says a particle was
        // discarded and not which sheet or block state did it, and those have
        // completely different causes: a `Sheet` miss means the atlas was never
        // installed or the frame names a texture the pack lacks, while a
        // `BlockState` miss means the model set has no `#particle` for that id.
        let mut first_unresolved: Option<SpriteSource> = None;
        for q in &self.quads {
            let Some((rect, atlas)) = self.sprite_rect(q.sprite) else {
                unresolved += 1;
                if first_unresolved.is_none() {
                    first_unresolved = Some(q.sprite);
                }
                continue;
            };
            if atlas == SpriteAtlas::Sheet {
                sheet_drawn += 1;
            }
            // Sprite-local UVs -> absolute atlas UVs.
            let (u0, v0) = (rect[0], rect[1]);
            let (du, dv) = (rect[2] - rect[0], rect[3] - rect[1]);
            let uv = [
                q.uv[0].mul_add(du, u0),
                q.uv[2].mul_add(dv, v0),
                q.uv[1].mul_add(du, u0),
                q.uv[3].mul_add(dv, v0),
            ];
            // Match the model shader exactly — vanilla's own lightmap curve, via
            // the one Rust mirror of it (`lodestone_render::light`). A particle
            // lit on a different curve from the block it came from reads as a
            // rendering bug in the terrain. Vanilla packs block light at bit 4
            // and sky light at bit 20.
            //
            // The *value* goes to the shader untouched; it is applied there, in
            // gamma space. See `ParticleInstance::colour`.
            //
            // Two gaps remain, both narrower than the space bug and both
            // separate from the curve. `sky_darken` is `1.0`: `Particles` has no
            // clock, so a particle does not yet dim at night. And this is the
            // *scalar* model, i.e. exactly vanilla's blue channel, where
            // `model.wgsl`/`fluid.wgsl` sample the three-channel
            // `light_color_from_levels` with its warm block tint and additive
            // sky/block combine — so a torch-lit particle is the right
            // brightness and slightly the wrong hue.
            let block = ((q.light >> 4) & 15) as f32 / 15.0;
            let sky = ((q.light >> 20) & 15) as f32 / 15.0;
            let shade = lodestone_render::light_term_from_levels(sky, block, 1.0);
            self.instances.push(ParticleInstance {
                centre_size: [q.position[0], q.position[1], q.position[2], q.size],
                uv,
                colour: q.colour,
                roll_light: [q.roll, shade, 0.0, 0.0],
                atlas: atlas as u32,
                translucent: u32::from(matches!(q.layer, Layer::Translucent)),
            });
        }

        let frame = ParticleFrame {
            alive: self.engine.particles().len(),
            drawn: self.instances.len(),
            unresolved,
            sheet_drawn,
            campfire_smoke_alive: self
                .engine
                .particles()
                .iter()
                .filter(|particle| {
                    particle.behaviour == lodestone_particle::Behaviour::CampfireSmoke
                })
                .count(),
        };
        // Whatever declines to draw a particle says why. A discarded particle
        // and one that never spawned look identical from outside, and the only
        // thing that separates them is a line like this — the counter alone
        // reads as health right up until you notice the screen is empty.
        //
        // Logged only when something was actually dropped, and naming the
        // *first* offending sprite rather than every one: this runs once per
        // frame, and a burst that fails to resolve fails identically 64 times.
        if unresolved > 0 {
            tracing::debug!(
                target: "particles",
                unresolved,
                total = frame.drawn + unresolved,
                sprite = ?first_unresolved,
                sheet_uv_entries = self.sheet_uv.len(),
                state_uv_entries = self.state_uv.len(),
                "particles simulated but not drawn: their sprite resolved to no atlas rect"
            );
        }
        self.last = frame;
        frame
    }

    /// A sprite's absolute UV rect **and the atlas that rect belongs to**.
    ///
    /// The two are returned together on purpose. `state_uv` and `sheet_uv` are
    /// keyed into two independent stitches with different dimensions and
    /// different packings, so a rect alone does not identify a texel — which is
    /// precisely how that fix happened: the renderer bound the block-model
    /// atlas for both and flame drew fragments of arbitrary block textures
    /// while `ParticleFrame::unresolved` stayed at zero.
    fn sprite_rect(&self, sprite: SpriteSource) -> Option<([f32; 4], SpriteAtlas)> {
        match sprite {
            SpriteSource::BlockState(id) => self
                .state_uv
                .get(id.raw() as usize)
                .copied()
                .flatten()
                .map(|rect| (rect, SpriteAtlas::Block)),
            // `SpriteAtlas::Block`, not a third selector: `BlockModels` bakes item
            // geometry against the *same* stitch as block states.
            SpriteSource::Item(item) => self
                .item_uv
                .get(item.registry_id() as usize)
                .copied()
                .flatten()
                .map(|rect| (rect, SpriteAtlas::Block)),
            SpriteSource::Sheet { sheet, frame } => self
                .sheet_uv
                .get(&(sheet, frame))
                .copied()
                .map(|rect| (rect, SpriteAtlas::Sheet)),
        }
    }
}

/// Builds the built-in-item-id → UV rect table [`Particles::new`] installs, by
/// walking [`Item`]s in registry order and asking `models` for each
/// item's `BreakingItemParticle` sprite.
///
/// Keyed by the built-in registry id, which the validated
/// [`SpriteSource::Item`](lodestone_particle::SpriteSource::Item) exposes only at
/// this indexed lookup. An item with no baked GUI geometry (a `special` renderer,
/// or one missing from a stripped pack) has no entry and its crumbs count as
/// unresolved — the same visible-gap discipline `state_uv` gets.
fn item_uv_table(models: &BlockModels) -> Vec<Option<[f32; 4]>> {
    let mut table =
        Vec::with_capacity(lodestone_data::item_prototypes::ITEM_COUNT as usize);
    for item in Item::all() {
        table.push(
            ResourceLocation::parse(item.name())
                .ok()
                .and_then(|loc| models.item_particle_uv(&loc)),
        );
    }
    table
}

/// Builds the `(Sheet, frame) -> UV rect` table [`Particles::with_particle_atlas`]
/// installs, by walking every physical frame of every sheet
/// `lodestone_particle` can emit ([`Sheet::all`]) and looking each one up in
/// `atlas` by the same location [`Sheet::texture_name`] would resolve through
/// vanilla's own `textures/particle/<name>.png` convention — see the module
/// docs on why the atlas keys sprites that way. A sheet whose texture is
/// missing from the atlas (a stripped-down or corrupt pack) simply has no
/// entry and falls back to counting as unresolved, the same as an absent
/// atlas entirely.
fn sheet_uv_table(atlas: &ParticleAtlas) -> HashMap<(Sheet, u16), [f32; 4]> {
    let mut table = HashMap::new();
    for &sheet in Sheet::all() {
        for frame in 0..sheet.frame_count() {
            let Ok(loc) = ResourceLocation::new("minecraft", sheet.texture_name(frame)) else {
                continue;
            };
            if let Some(sprite) = atlas.sprite(&loc) {
                table.insert(
                    (sheet, frame),
                    [
                        sprite.uv_min[0],
                        sprite.uv_min[1],
                        sprite.uv_max[0],
                        sprite.uv_max[1],
                    ],
                );
            }
        }
    }
    table
}
