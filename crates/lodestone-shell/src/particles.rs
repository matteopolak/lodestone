//! Particles: simulation ownership plus the billboard render pass.
//!
//! [`lodestone_particle`] reproduces vanilla's per-tick particle physics but has
//! no opinion about pixels — it emits [`ParticleQuad`]s in camera-relative space
//! with *sprite-local* UVs. This module is the other half: it owns the live
//! [`ParticleEngine`], resolves each quad's sprite into absolute atlas UVs, and
//! draws the result as camera-facing billboards.
//!
//! # Why the shell owns sprite resolution
//!
//! A [`SpriteSource::BlockState`] names a block state, not a texture. Turning it
//! into UVs needs the baked model set — vanilla's `BakedModel.particleIcon()`,
//! which is the model's `#particle` variable and is emphatically **not** the
//! texture of any of its faces (`grass_block` declares `block/dirt`). Only the
//! shell holds both the engine and the atlas, so the join happens here.
//!
//! # `SpriteSource::Sheet` resolution
//!
//! Smoke, flame, crits, splashes and the rest of [`SpriteSource::Sheet`] are
//! resolved against a stitched [`ParticleAtlas`], the same way
//! [`SpriteSource::BlockState`] is resolved against the baked model set:
//! [`Particles::with_particle_atlas`] precomputes a `(Sheet, frame) -> UV
//! rect` table at construction, mirroring the `state_uv` table below. Vanilla
//! has no pre-baked `particles.png` on disk either — it stitches loose
//! `textures/particle/*.png` sprites at load time — so [`ParticleAtlas`]
//! reuses [`lodestone_assets`]'s [`AtlasBuilder`](lodestone_assets::AtlasBuilder)
//! exactly as the block and item atlases do, rather than a second stitcher.
//!
//! Nothing in this crate loads `client.jar` itself (that needs a resource
//! root, which is resolved elsewhere in the shell); a session that never
//! calls [`Particles::with_particle_atlas`] simply keeps every sheet particle
//! unresolved, same as before. [`Particles::extract`] counts whatever is
//! unresolved into [`ParticleFrame::unresolved`] so the gap — full, partial,
//! or none — is always visible rather than looking like a working system that
//! quietly emits nothing.
//!
//! # The atlas a UV belongs to is part of the UV
//!
//! The sheet stitch and the block-model stitch are **different textures with
//! different packings**, so a UV rect on its own does not identify a texel.
//! For months this pass bound one texture — the block-model atlas — and
//! resolved sheet UVs against it, so `/particle minecraft:flame` drew
//! fragments of arbitrary block textures. `unresolved` stayed at zero the
//! whole time, correctly: the UVs *did* resolve, just against the wrong
//! image. Since then every [`ParticleInstance`] carries a [`SpriteAtlas`]
//! selector decided by the same [`Particles::sprite_rect`] match that
//! produced its rect, and [`ParticleRenderer`] binds both stitches. See
//! `docs/break-particles.md`.

use std::collections::HashMap;
use std::sync::Arc;

use lodestone_assets::{ParticleAtlas, ResourceLocation};
use lodestone_model::event::ParticleOptions;
use lodestone_particle::{Layer, ParticleEngine, ParticleQuad, Sheet, SpriteSource, emit};
use lodestone_physics::{CollisionView, Vec3d};
use lodestone_render::{BlockModels, Camera};
use wgpu::util::DeviceExt;

/// `DripParticle.WaterHangProvider`'s tint — vanilla sets water drips to
/// `0.2F, 0.3F, 1.0F` rather than the biome water colour, so a cave drip reads
/// blue everywhere including in swamp water.
const WATER_DRIP_COLOUR: [f32; 3] = [0.2, 0.3, 1.0];

/// `DripParticle.createLavaHang`'s tint, `1.0F, 0.2857F, 0.083F`.
const LAVA_DRIP_COLOUR: [f32; 3] = [1.0, 0.2857, 0.083];

/// Which stitched texture a [`ParticleInstance`]'s UVs address.
///
/// This travels *with* the UVs, decided by the same
/// [`Particles::sprite_rect`] match that produced them, because a UV rect
/// without its atlas is meaningless and was for months exactly that: the
/// renderer bound one texture — the block-model atlas — and every
/// [`SpriteSource::Sheet`] particle sampled block texels at particle-sheet
/// coordinates. Making the pair inseparable is the point; a
/// future emitter cannot forget to say which atlas it meant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpriteAtlas {
    /// The block-model atlas the terrain pass samples. `SpriteSource::BlockState`.
    Block = 0,
    /// The stitched [`ParticleAtlas`] — its own packing, its own dimensions.
    Sheet = 1,
}

/// One particle's GPU instance. Four vertices are generated per instance from
/// `vertex_index`, so there is no vertex or index buffer.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ParticleInstance {
    /// Camera-relative centre, `w` = half-extent in blocks.
    centre_size: [f32; 4],
    /// Absolute atlas UVs `[u0, v0, u1, v1]`, in the space of [`Self::atlas`].
    uv: [f32; 4],
    /// Vanilla's own RGBA tint (`rCol`/`gCol`/`bCol`/`alpha`), and **not**
    /// premultiplied by the light term.
    ///
    /// It used to be. Folding the lightmap value in here multiplied it against
    /// a *linear* texel in the shader, and per `CLAUDE.md` vanilla is not
    /// colour-managed: shade and tint multiply in **gamma** space. A linear
    /// multiply pulls every factor toward 1.0 — an unlit particle's `0.0935`
    /// re-encodes to `0.34`, which is why particles read as permanently
    /// full-bright even though the light plumbing behind them was correct all
    /// along. The multiply now happens in `particles.wgsl` between a
    /// `linear_to_srgb` and an `srgb_to_linear`, exactly as `model.wgsl` does
    /// it, so both this tint and [`Self::roll_light`]'s shade land in the
    /// space vanilla applies them in.
    colour: [f32; 4],
    /// `x` = roll about the view axis in radians; `y` = the lightmap term for
    /// this particle's own block position (`lodestone_render::light`'s scalar
    /// model, the one Rust mirror of `lightmap.fsh` the model and fluid
    /// shaders duplicate); `zw` padding.
    roll_light: [f32; 4],
    /// [`SpriteAtlas`] as `u32` — which of the fragment shader's two bound
    /// textures [`Self::uv`] addresses. A separate vertex attribute rather
    /// than a spare lane of `roll` so that reading the struct tells you the
    /// UVs are atlas-relative; the 68-byte stride is deliberate and harmless
    /// (`u32` needs 4-byte alignment, so there is no padding and `Pod` still
    /// derives).
    atlas: u32,
    /// `1` for [`Layer::Translucent`], `0` for [`Layer::Opaque`].
    ///
    /// The fragment shader never reads this — [`ParticleRenderer::prepare`]
    /// does, to partition the upload into the two draws vanilla splits
    /// particles across (`SubmitNodeCollection::submitQuadParticleGroup`
    /// submits the same group twice, once into the `solid` phase and once into
    /// `afterTerrain`, and `QuadParticleFeatureRenderer` keeps only the layers
    /// whose `translucent()` matches). It rides in the instance rather than
    /// being passed alongside it because `RenderState::prepare_particles`'s
    /// signature is fixed by callers outside this module, and because deriving
    /// the split from the bytes actually uploaded is the same reasoning
    /// [`Self::atlas`] records: a count plumbed separately can disagree with
    /// them.
    translucent: u32,
}

/// The particle camera uniform. Positions are camera-relative, so the matrix is
/// the view-projection pre-translated by the camera position — that keeps the
/// f32 precision win of camera-relative extraction instead of undoing it by
/// adding the world position back in the shader.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ParticleUniform {
    view_proj: [[f32; 4]; 4],
    /// World-space camera right vector (`w` unused).
    right: [f32; 4],
    /// World-space camera up vector (`w` unused).
    up: [f32; 4],
}

/// What one frame's extraction produced. Reported so a frame that draws nothing
/// says *why*.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ParticleFrame {
    /// Live particles in the engine.
    pub alive: usize,
    /// Quads that resolved to a sprite and were uploaded.
    pub drawn: usize,
    /// Quads dropped because their sprite could not be resolved — a
    /// sheet-based particle when no [`ParticleAtlas`] was attached (see
    /// [`Particles::with_particle_atlas`]), or a block state with no
    /// `#particle`.
    pub unresolved: usize,
    /// Of [`Self::drawn`], how many address the **particle sheet** rather than
    /// the block-model atlas.
    ///
    /// This is an **anti-vacuity counter**, not a game value, and it exists
    /// because the pre-fix renderer bound only one texture, so
    /// `unresolved == 0` was satisfied by flame/smoke/crit UVs that resolved
    /// perfectly and then sampled *block* texels. A gate on sheet particles
    /// has to be able to prove the sheet path was exercised at all —
    /// `drawn > 0` alone is satisfied by terrain debris.
    pub sheet_drawn: usize,
}

/// The live particle simulation plus its per-frame extraction scratch.
///
/// Sprite resolution is precomputed into a per-state table at construction: the
/// alternative is a `BlockModels` borrow held across the frame, and the models
/// live inside the renderer while the engine ticks in the simulation.
#[derive(Debug)]
pub struct Particles {
    engine: ParticleEngine,
    /// Per-block-state atlas UV rect, indexed by state id. Empty when no vanilla
    /// model set is loaded (the offline demo world), which is why
    /// [`ParticleFrame::unresolved`] exists rather than a silent no-op.
    state_uv: Arc<Vec<Option<[f32; 4]>>>,
    /// Per-block-state particle **tint** multiplier, indexed by state id and
    /// aligned with `state_uv`. `[1.0; 3]` for an untinted state.
    ///
    /// This exists because vanilla's `TerrainParticle` does not multiply its
    /// `0.6` grey by white — it multiplies by
    /// `blockColors.getTintSource(state, 0).colorAsTerrainParticle(…)`. The
    /// blocks that have such a source are exactly the ones whose sprites are
    /// **greyscale in the atlas** (`grass`, `fern`, the leaves, `sugar_cane`,
    /// `redstone_dust_*`), so dropping the tint does not merely desaturate their
    /// debris — it renders it near-**white**. See `docs/break-particles.md`.
    state_tint: Arc<Vec<[f32; 3]>>,
    /// Per-**item** atlas UV rect, indexed by network item registry id —
    /// `SpriteSource::Item`, i.e. `BreakingItemParticle`'s sprite.
    ///
    /// Indexed by id rather than keyed by name for the same reason `state_uv` is:
    /// the engine ticks in the simulation while the models live in the renderer, so
    /// resolution is precomputed once instead of holding a `BlockModels` borrow
    /// across the frame. Items live in the **same** stitched atlas as block states
    /// (`BlockModels::build` bakes both against one), which is why these rects also
    /// carry [`SpriteAtlas::Block`] and not a third selector.
    item_uv: Arc<Vec<Option<[f32; 4]>>>,
    /// Per-`(Sheet, frame)` atlas UV rect. Empty when no [`ParticleAtlas`] has
    /// been attached via [`Self::with_particle_atlas`], in which case every
    /// [`SpriteSource::Sheet`] particle counts into
    /// [`ParticleFrame::unresolved`] rather than drawing nothing silently.
    sheet_uv: Arc<HashMap<(Sheet, u16), [f32; 4]>>,
    quads: Vec<ParticleQuad>,
    instances: Vec<ParticleInstance>,
    last: ParticleFrame,
}

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
                    .map(|id| m.particle_uv(id))
                    .collect(),
                (0..m.state_count() as u32)
                    .map(|id| m.particle_tint(id).unwrap_or([1.0; 3]))
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

    /// Emit vanilla's block-destruction burst — `ClientLevel.addDestroyBlockEffect`.
    ///
    /// The shape is passed in rather than queried because vanilla reads the
    /// block's *outline* shape, not its collision shape, and the two differ for
    /// exactly the blocks that matter: `short_grass` has an outline and no
    /// collision at all, so driving this from collision geometry would emit
    /// nothing when a player breaks grass.
    ///
    /// `tint` is an **extra** multiplier applied on top of the state's own
    /// particle tint, not a replacement for it — see
    /// [`state_tint_of`](Self::state_tint_of). Callers that have nothing special
    /// to say pass `[1.0; 3]`.
    pub fn destroy_block(&mut self, block: [i32; 3], state: u32, tint: [f32; 3]) {
        let tint = self.state_tint_of(state, tint);
        emit::destroy_block_effect(
            &mut self.engine,
            (block[0], block[1], block[2]),
            state,
            tint,
            &[emit::FULL_CUBE],
        );
    }

    /// Emit the single fragment vanilla throws each time a mining hit lands on a
    /// face — `ClientLevel.addBreakingBlockEffect`.
    ///
    /// `tint` is an extra multiplier on top of the state's own particle tint,
    /// exactly as in [`destroy_block`](Self::destroy_block): the two emitters
    /// both construct a `TerrainParticle`, so they must tint identically or a
    /// block's mining flecks and its final burst come out different colours.
    pub fn breaking_block(&mut self, block: [i32; 3], state: u32, tint: [f32; 3], face: emit::Face) {
        let tint = self.state_tint_of(state, tint);
        emit::breaking_block_effect(
            &mut self.engine,
            (block[0], block[1], block[2]),
            state,
            tint,
            face,
            emit::FULL_CUBE,
        );
    }

    /// `extra` multiplied by `state`'s own particle tint — the
    /// `rCol *= tintSource.colorAsTerrainParticle(state, level, pos)` step of
    /// vanilla's `TerrainParticle` constructor.
    ///
    /// # Why this is folded in here rather than passed by the caller
    ///
    /// It was passed by the caller, as a hardcoded `[1.0; 3]` at both emit
    /// sites, and that is the bug this method exists to close: a *plausible*
    /// constant. The tinted blocks are precisely the ones whose atlas sprites
    /// are greyscale, so the missing multiply did not read as "slightly wrong
    /// colour" — it rendered grass, fern, leaf, sugar-cane and redstone debris
    /// **white**. Deriving it from the state id means a new emit site cannot
    /// reintroduce the constant by omission.
    ///
    /// An out-of-range `state` returns `extra` unchanged rather than panicking:
    /// the same id is about to resolve to no sprite at all and be counted into
    /// [`ParticleFrame::unresolved`], which is the report that already covers
    /// it. Duplicating that as a second failure mode here would just be noise.
    fn state_tint_of(&self, state: u32, extra: [f32; 3]) -> [f32; 3] {
        let Some(t) = self.state_tint.get(state as usize) else {
            return extra;
        };
        [extra[0] * t[0], extra[1] * t[1], extra[2] * t[2]]
    }

    /// How many block states carry a non-white particle tint.
    ///
    /// This is an **anti-vacuity accessor**, not a game value: "no state's
    /// debris is the wrong colour" is satisfied by a table that resolved no
    /// tints at all, so a gate on particle tinting has to be able to prove the
    /// table is populated. Zero on the demo palette (correctly — it has no
    /// tinted blocks); in the thousands on a complete vanilla pack.
    #[must_use]
    pub fn tinted_state_count(&self) -> usize {
        self.state_tint
            .iter()
            .filter(|t| *t != &[1.0f32, 1.0, 1.0])
            .count()
    }

    /// Vanilla's `ClientPacketListener.handleParticleEvent` — the general
    /// `LEVEL_PARTICLES` packet path, as opposed to the `LevelEvent` 2001
    /// shortcut [`Self::destroy_block`] covers. Spawns `count` particles of
    /// `kind` (the particle type's namespace-stripped path, e.g. `"flame"`)
    /// at `pos`.
    ///
    /// # `count == 0` is not "spawn nothing"
    ///
    /// Confirmed against the 26.2 client sources
    /// (`ClientPacketListener.handleParticleEvent`,
    /// `.cache/mc/26.2/client-src/net/minecraft/client/multiplayer/ClientPacketListener.java`):
    /// when `count == 0` vanilla spawns exactly **one** particle at the
    /// *exact* `pos` (no positional jitter), whose velocity is
    /// `maxSpeed * offset` per axis rather than drawn from noise:
    ///
    /// ```text
    /// if (count == 0) {
    ///     xa = maxSpeed * xDist; ya = maxSpeed * yDist; za = maxSpeed * zDist;
    ///     addParticle(particle, x, y, z, xa, ya, za);
    /// } else {
    ///     for (i in 0..count) {
    ///         xVarience = nextGaussian() * xDist; // ditto y, z
    ///         xa = nextGaussian() * maxSpeed;      // ditto y, z — NOT scaled by offset
    ///         addParticle(particle, x + xVarience, y + yVarience, z + zVarience, xa, ya, za);
    ///     }
    /// }
    /// ```
    ///
    /// So `offset` means two different things depending on `count`: a raw
    /// velocity direction when `count == 0`, and a per-axis jitter *bound*
    /// (multiplied by an independent gaussian draw) otherwise — and in the
    /// `count > 0` branch the velocity draws are unrelated to `offset`
    /// entirely, only to `max_speed`.
    ///
    /// Particle-burst randomness does not need to replay bit-exact against
    /// vanilla — nothing observes it across the wire, the same call
    /// `lodestone_particle`'s own `JavaRandom` docs make for the emitters
    /// below — so the gaussian draws here are an ordinary Box-Muller
    /// transform over the engine's existing RNG stream rather than a second
    /// `java.util.Random` reimplementation.
    ///
    /// Only particle types this shell has a dedicated emitter for are
    /// spawned; an unrecognised `kind` is logged and dropped. The shape of a
    /// burst lives in the per-type emitter ([`lodestone_particle::emit`]),
    /// and guessing at one here would just be a worse copy of it.
    pub fn spawn_particles(
        &mut self,
        kind: &str,
        pos: [f64; 3],
        offset: [f32; 3],
        max_speed: f32,
        count: i32,
        options: ParticleOptions,
    ) {
        if count == 0 {
            let vel = [
                f64::from(max_speed) * f64::from(offset[0]),
                f64::from(max_speed) * f64::from(offset[1]),
                f64::from(max_speed) * f64::from(offset[2]),
            ];
            self.spawn_one(kind, pos, vel, options);
            return;
        }
        for _ in 0..count {
            let jittered = [
                pos[0] + self.gaussian() * f64::from(offset[0]),
                pos[1] + self.gaussian() * f64::from(offset[1]),
                pos[2] + self.gaussian() * f64::from(offset[2]),
            ];
            let vel = [
                self.gaussian() * f64::from(max_speed),
                self.gaussian() * f64::from(max_speed),
                self.gaussian() * f64::from(max_speed),
            ];
            self.spawn_one(kind, jittered, vel, options);
        }
    }

    /// Dispatches one particle to the emitter matching `kind`. Mirrors
    /// `Level.addParticle`'s per-type dispatch, narrowed to the sheet
    /// particles [`lodestone_particle::emit`] implements today.
    fn spawn_one(&mut self, kind: &str, pos: [f64; 3], vel: [f64; 3], options: ParticleOptions) {
        let [x, y, z] = pos;
        let [xa, ya, za] = vel;
        match kind {
            "flame" => emit::flame(&mut self.engine, x, y, z, xa, ya, za),
            "smoke" => emit::smoke(&mut self.engine, x, y, z, xa, ya, za, 1.0),
            // `LargeSmokeParticle extends SmokeParticle` with `scale = 2.5F`.
            "large_smoke" => emit::smoke(&mut self.engine, x, y, z, xa, ya, za, 2.5),
            "crit" => emit::crit(&mut self.engine, x, y, z, xa, ya, za),
            "splash" => emit::splash(&mut self.engine, x, y, z, xa, ya, za),
            "bubble" => emit::bubble(&mut self.engine, x, y, z, xa, ya, za),
            // The sweep-attack particle (that fix's split-out remainder — its own
            // issue now). `xa` doubles as the constructor's `size` parameter
            // here, per `AttackSweepParticle`'s own signature; see
            // `emit::sweep_attack`'s docs for why the one real vanilla call
            // site always sends `0.0` regardless of the swing direction.
            // The packet's own field is an f32; widened to f64 only for the
            // generic dispatch signature above, narrowed straight back here.
            "sweep_attack" => {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "narrowing back to the f32 the wire value started as"
                )]
                let size = xa as f32;
                emit::sweep_attack(&mut self.engine, x, y, z, size);
            }
            "note" => emit::note(&mut self.engine, x, y, z, xa),
            "heart" => emit::heart(&mut self.engine, x, y, z),
            "angry_villager" => emit::angry_villager(&mut self.engine, x, y, z),
            "happy_villager" => emit::happy_villager(&mut self.engine, x, y, z, xa, ya, za),
            "witch" => emit::witch(&mut self.engine, x, y, z, xa, ya, za),
            "totem_of_undying" => emit::totem_of_undying(&mut self.engine, x, y, z, xa, ya, za),
            // `ParticleTypes.EXPLOSION_EMITTER`/`EXPLOSION`.
            // Correction the doc for these two carried until this pass: they
            // are **not** blocked on the shared `ParticleOptions` decoder
            // (`docs/particle-catalogue.md`'s "explosion_emitter"/"explosion"
            // section) — both are argument-less `SimpleParticleType`s, and
            // `decode_explode` already recognises their registry ids. What
            // was missing was exactly this arm plus the `Sheet`/`Behaviour`
            // pair in `lodestone_particle`, not a decoder.
            //
            // `explosion_emitter` (the seed vanilla's own `explode` packet
            // actually names — `Level.java`) ignores every
            // positional argument here: `HugeExplosionSeedParticle`'s
            // constructor reads none. `explosion` reuses `xa` as the
            // constructor's `size` parameter, the same repurposing
            // `sweep_attack` above already does for its own `size`.
            "explosion_emitter" => emit::explosion_emitter(&mut self.engine, x, y, z),
            "explosion" => {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "narrowing back to the f32 the wire value started as"
                )]
                let size = xa as f32;
                emit::huge_explosion(&mut self.engine, x, y, z, size);
            }

            // -- Ambient and environmental types ----------------
            //
            // Every arm below is an argument-less `SimpleParticleType`, so the
            // three velocity words are exactly what the wire sent and nothing
            // needs the `ParticleOptions` decoder. Several *also* have a
            // client-predicted emitter — see `Sim::tick_ambient_particles` —
            // because vanilla spawns them from `Block.animateTick` rather than
            // over the network; a type can legitimately have both.
            "soul" => emit::soul(&mut self.engine, x, y, z, xa, ya, za),
            "soul_fire_flame" => emit::soul_fire_flame(&mut self.engine, x, y, z, xa, ya, za),
            // `reverse_portal` shares `PortalParticle` and differs only in the
            // sign the *caller* gives the offset, which the wire already carries.
            "portal" | "reverse_portal" => emit::portal(&mut self.engine, x, y, z, xa, ya, za),
            "campfire_cosy_smoke" => {
                emit::campfire_smoke(&mut self.engine, x, y, z, xa, ya, za, false);
            }
            "campfire_signal_smoke" => {
                emit::campfire_smoke(&mut self.engine, x, y, z, xa, ya, za, true);
            }
            "end_rod" => emit::end_rod(&mut self.engine, x, y, z, xa, ya, za),
            "electric_spark" | "glow" => emit::spark(&mut self.engine, x, y, z, xa, ya, za),
            // `FireworkParticles.SparkParticle` via `SparkProvider` -- the plain
            // wire particle a `LEVEL_PARTICLES` packet can name directly, not the
            // rocket-explosion burst a `Starter`/`NoRenderParticle` spawns
            // client-side (never sent over the wire at all). See
            // `docs/particle-catalogue.md`'s "Correction" entry for why this was
            // never blocked on the `ParticleOptions` decoder the way it first
            // looked: `ParticleTypes.FIREWORK` is a `SimpleParticleType`.
            "firework" => emit::firework(&mut self.engine, x, y, z, xa, ya, za),
            // Sheet, scale and lifetime are what separate these four; the tick
            // shape is identical. Lifetimes are each class's own constructor.
            "sculk_charge" => emit::animated_ambient(
                &mut self.engine, x, y, z, xa, ya, za, Sheet::SculkCharge, 1.0, 15,
            ),
            "gust" => {
                emit::animated_ambient(&mut self.engine, x, y, z, 0.0, 0.0, 0.0, Sheet::Gust, 3.0, 12)
            }
            "small_gust" => {
                emit::animated_ambient(&mut self.engine, x, y, z, 0.0, 0.0, 0.0, Sheet::Gust, 1.0, 12)
            }
            "sonic_boom" => emit::animated_ambient(
                &mut self.engine, x, y, z, 0.0, 0.0, 0.0, Sheet::SonicBoom, 3.0, 16,
            ),
            // The drip family. Three sheets for three phases, and the fluid's own
            // colour is the only thing telling a water drip from a lava one —
            // both use the same sprite. `DripParticle`'s hanging phase has no
            // gravity at all until it lets go; the falling phase does.
            "dripping_water" => {
                emit::drip(&mut self.engine, x, y, z, Sheet::DripHang, WATER_DRIP_COLOUR, 0.0);
            }
            "falling_water" => {
                emit::drip(&mut self.engine, x, y, z, Sheet::DripFall, WATER_DRIP_COLOUR, 1.0);
            }
            "dripping_lava" => {
                emit::drip(&mut self.engine, x, y, z, Sheet::DripHang, LAVA_DRIP_COLOUR, 0.0);
            }
            "falling_lava" => {
                emit::drip(&mut self.engine, x, y, z, Sheet::DripFall, LAVA_DRIP_COLOUR, 1.0);
            }
            "landing_lava" => {
                emit::drip(&mut self.engine, x, y, z, Sheet::DripLand, LAVA_DRIP_COLOUR, 0.0);
            }
            "spore_blossom_air" => {
                emit::drip(&mut self.engine, x, y, z, Sheet::DripFall, [0.32, 0.5, 0.22], 0.0);
            }
            // The two `ParticleOptions`-carrying types this shell decodes a
            // payload for today (`decode_particle_options` in the v770
            // adapter). `kind` and `options` both come from the same
            // `LEVEL_PARTICLES` packet by construction -- `net.rs`'s
            // `ClientEvent::Particles` arm carries both straight through to
            // `NetUpdate::Particles`, and `net_apply.rs`'s arm hands both to
            // this call unmodified -- so the two agreeing is the production
            // case; the `_` arm below is only reachable from a caller
            // (a test, or a future non-network producer) that passes a
            // mismatched or default `options` on purpose.
            "dust" => match options {
                ParticleOptions::Dust { color, scale } => {
                    emit::dust(&mut self.engine, x, y, z, xa, ya, za, color, scale);
                }
                _ => tracing::debug!(
                    target: "particles",
                    "dust particle with no DustParticleOptions payload; dropped"
                ),
            },
            "dust_color_transition" => match options {
                ParticleOptions::DustColorTransition { from_color, to_color, scale } => {
                    emit::dust_color_transition(
                        &mut self.engine,
                        x,
                        y,
                        z,
                        xa,
                        ya,
                        za,
                        from_color,
                        to_color,
                        scale,
                    );
                }
                _ => tracing::debug!(
                    target: "particles",
                    "dust_color_transition particle with no DustColorTransitionOptions \
                     payload; dropped"
                ),
            },
            other => tracing::debug!(
                target: "particles",
                "no emitter wired for particle type {other:?}; dropped"
            ),
        }
    }

    /// One standard-normal draw (Box-Muller), for the positional/velocity
    /// jitter [`Self::spawn_particles`] needs. See that method's docs for why
    /// this does not need to match `java.util.Random.nextGaussian()`
    /// bit-for-bit.
    fn gaussian(&mut self) -> f64 {
        let rng = self.engine.rng();
        let u1 = rng.next_double().max(1e-12);
        let u2 = rng.next_double();
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }

    /// Test-only seam: installs a `(Sheet, frame) -> UV rect` table directly,
    /// bypassing `ParticleAtlas`/jar I/O — mirrors the fixture this module's
    /// own tests use (see `sheet_particle_resolves_with_an_atlas`), exposed so
    /// `crate::sim`'s tests can assert a live `NetUpdate::Particles` resolves
    /// without needing the real vanilla jar.
    #[cfg(test)]
    pub(crate) fn install_test_sheet_uv(&mut self, table: HashMap<(Sheet, u16), [f32; 4]>) {
        self.sheet_uv = Arc::new(table);
    }

    /// Advance every live particle one tick against `view`.
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
        for q in &self.quads {
            let Some((rect, atlas)) = self.sprite_rect(q.sprite) else {
                unresolved += 1;
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
        };
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
                .get(id as usize)
                .copied()
                .flatten()
                .map(|rect| (rect, SpriteAtlas::Block)),
            // `SpriteAtlas::Block`, not a third selector: `BlockModels` bakes item
            // geometry against the *same* stitch as block states.
            SpriteSource::Item(id) => self
                .item_uv
                .get(id as usize)
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

/// Builds the network-item-id → UV rect table [`Particles::new`] installs, by
/// walking `lodestone_data::items`' registry order and asking `models` for each
/// item's `BreakingItemParticle` sprite.
///
/// Keyed by the **network registry id**, which is what
/// [`SpriteSource::Item`](lodestone_particle::SpriteSource::Item) carries, so the
/// emitter never has to hold a name. An item with no baked GUI geometry (a `special`
/// renderer, or one missing from a stripped pack) has no entry and its crumbs count
/// as unresolved — the same visible-gap discipline `state_uv` gets.
fn item_uv_table(models: &BlockModels) -> Vec<Option<[f32; 4]>> {
    let mut table =
        Vec::with_capacity(lodestone_data::item_prototypes::ITEM_COUNT as usize);
    for id in 0i32.. {
        let Some(name) = lodestone_data::items::item_name(id) else {
            break;
        };
        table.push(
            ResourceLocation::parse(name)
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

/// The billboard render pass: one pipeline, one growable instance buffer, one
/// camera uniform.
///
/// # Two atlases, one pass
///
/// Group 1 binds **both** stitches — the block-model atlas the terrain samples
/// *and* the stitched particle sheet — and each instance carries a
/// [`SpriteAtlas`] selector saying which of them its UVs address. Before that
/// this pass bound one texture and every sheet particle sampled
/// block texels at particle-sheet coordinates: `/particle minecraft:flame`
/// drew fragments of arbitrary block textures, and nothing observed it because
/// the UVs *did* resolve.
///
/// The alternative shape — a second bind group plus two draws, block-atlas
/// instances then sheet instances — was rejected because it makes correctness
/// depend on the instance list staying **partitioned by atlas**, an invariant
/// nothing in the type system holds and which any future sort (by depth, say)
/// would silently break, reintroducing exactly this bug. Sampling both
/// textures and selecting costs one extra tap per particle fragment and makes
/// a mis-pairing unrepresentable. Two bind groups total also keeps this pass
/// far below the 4-group floor `CLAUDE.md` warns about.
#[derive(Debug)]
pub struct ParticleRenderer {
    pipeline: wgpu::RenderPipeline,
    opaque_pipeline: wgpu::RenderPipeline,
    cam_layout: wgpu::BindGroupLayout,
    tex_layout: wgpu::BindGroupLayout,
    cam_buffer: wgpu::Buffer,
    cam_bind_group: wgpu::BindGroup,
    instances: wgpu::Buffer,
    /// The upload staging list, opaque-layer instances first. Held across
    /// frames so the partition is not a per-frame allocation.
    ordered: Vec<ParticleInstance>,
    capacity: u32,
    count: u32,
    /// How many of the leading [`Self::count`] instances are opaque-layer, i.e.
    /// the split point between [`ParticleRenderer::draw_opaque`] and
    /// [`ParticleRenderer::draw`].
    opaque_count: u32,
    /// Of [`Self::count`], how many address the particle sheet. Kept so a
    /// caller that never installed a sheet texture can *notice* it is
    /// submitting sheet instances instead of drawing nothing — see
    /// [`ParticleFrame::sheet_drawn`] for why that distinction is the whole
    /// point of that fix.
    sheet_count: u32,
}

/// Instances allocated up front; the buffer grows (never shrinks) past this.
const INITIAL_CAPACITY: u32 = 4096;

impl ParticleRenderer {
    /// Build the pipeline for a target of `color_format`.
    #[must_use]
    pub fn new(device: &wgpu::Device, color_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("lodestone-particle-shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });

        let cam_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lodestone-particle-camera-bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                // The uniform is read in the vertex stage only, but naming the
                // wrong stage set here fails at *bind* time rather than compile
                // time, so it is spelled out deliberately.
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        // Bindings 0/1 are the block-model atlas + its sampler; 2/3 are the
        // particle sheet + *its* sampler. Two samplers, not one: the two
        // stitches are separate textures with separate mip pyramids, and
        // sharing a sampler object across them would only work by accident.
        let texture_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let sampler_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        };
        let tex_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lodestone-particle-atlas-bgl"),
            entries: &[
                texture_entry(0),
                sampler_entry(1),
                texture_entry(2),
                sampler_entry(3),
            ],
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("lodestone-particle-pl"),
            bind_group_layouts: &[Some(&cam_layout), Some(&tex_layout)],
            immediate_size: 0,
        });

        // Two pipelines over one shader and one layout, differing **only** in
        // depth write. They are vanilla's `OPAQUE_PARTICLE` and
        // `TRANSLUCENT_PARTICLE` (`RenderPipelines.java`), both built from
        // the same `PARTICLE_SNIPPET`.
        //
        // One deliberate deviation: vanilla's opaque pipeline has no blending at
        // all, and this one keeps `ALPHA_BLENDING`. `Behaviour::layer()` assigns
        // every `Terrain` particle to `Layer::Opaque` unconditionally, where
        // vanilla's `Layer.bySprite` consults the sprite's own transparency and
        // sends a translucent block texture to `TRANSLUCENT_TERRAIN` instead. So
        // a broken glass or ice block reaches this pipeline here and would not
        // in vanilla, and a non-blending pipeline would draw it as opaque
        // squares. For a genuinely opaque texel the two are identical, so
        // blending is the strictly safer of the two until `layer()` learns about
        // sprite transparency.
        let make_pipeline = |label: &str, depth_write: bool| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<ParticleInstance>() as u64,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &wgpu::vertex_attr_array![
                            0 => Float32x4, 1 => Float32x4, 2 => Float32x4, 3 => Float32x4,
                            4 => Uint32, 5 => Uint32
                        ],
                    })],
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleStrip,
                    // A billboard is built from the camera basis, so its winding
                    // flips as the camera passes it. Culling would blink particles
                    // out; vanilla draws them double-sided too.
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: wgpu::TextureFormat::Depth32Float,
                    depth_write_enabled: Some(depth_write),
                    // `Less`, not vanilla's `GREATER_THAN_OR_EQUAL`: depth here
                    // is `[0,1]` DirectX-style rather than vanilla's reversed-Z,
                    // so every ported comparison flips (`CLAUDE.md`,
                    // "Rendering constraints").
                    depth_compare: Some(wgpu::CompareFunction::Less),
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState::default(),
                }),
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: color_format,
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        // Depth **write on**, which is the whole mechanism behind the water fix:
        // water draws with depth test on and depth write off, so it can only
        // blend over a submerged particle if that particle is already in the
        // depth buffer. Without the write, water passes against the sea floor
        // and the particle stays in the framebuffer untinted *and* a particle in
        // front of the surface gets tinted anyway.
        let opaque_pipeline = make_pipeline("lodestone-particle-pipeline-opaque", true);
        // Depth write off: these draw after translucent terrain, and overlapping
        // blended sprites would punch holes in each other in draw order.
        let pipeline = make_pipeline("lodestone-particle-pipeline", false);

        let cam_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("lodestone-particle-camera"),
            contents: bytemuck::bytes_of(&ParticleUniform {
                view_proj: glam::Mat4::IDENTITY.to_cols_array_2d(),
                right: [1.0, 0.0, 0.0, 0.0],
                up: [0.0, 1.0, 0.0, 0.0],
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let cam_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lodestone-particle-camera-bg"),
            layout: &cam_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: cam_buffer.as_entire_binding(),
            }],
        });

        let instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lodestone-particle-instances"),
            size: u64::from(INITIAL_CAPACITY) * std::mem::size_of::<ParticleInstance>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            opaque_pipeline,
            cam_layout,
            tex_layout,
            cam_buffer,
            cam_bind_group,
            instances,
            ordered: Vec::new(),
            capacity: INITIAL_CAPACITY,
            count: 0,
            opaque_count: 0,
            sheet_count: 0,
        }
    }

    /// Build the atlas bind group this pass samples.
    ///
    /// `block_*` must be the **same** atlas view the terrain pass binds, so a
    /// terrain fragment is textured from the same pixels as the block it came
    /// off. `sheet_*` is the stitched [`ParticleAtlas`] upload, which is a
    /// wholly separate texture with its own packing — passing the block atlas
    /// twice is what the renderer effectively did before this was fixed,
    /// and it draws block texels for flame and smoke. See
    /// [`crate::gpu::RenderState::install_particle_sheet_atlas`] for the
    /// jar-less fallback, which binds a 1×1 transparent texture instead so an
    /// unresolvable sheet particle draws *nothing* rather than garbage.
    #[must_use]
    pub fn atlas_bind_group(
        &self,
        device: &wgpu::Device,
        block_view: &wgpu::TextureView,
        block_sampler: &wgpu::Sampler,
        sheet_view: &wgpu::TextureView,
        sheet_sampler: &wgpu::Sampler,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lodestone-particle-atlas-bg"),
            layout: &self.tex_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(block_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(block_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(sheet_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(sheet_sampler),
                },
            ],
        })
    }

    /// Upload this frame's already-extracted instances. Must run **before** the
    /// render pass opens — buffers cannot be created mid-pass.
    ///
    /// Extraction deliberately happens in the simulation
    /// ([`Particles::extract`]), not here: resolving each particle's light needs
    /// the world, and taking `&mut Particles` alongside a world-reading closure
    /// would force the caller to hand out two borrows of the same owner.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        instances: &[ParticleInstance],
        camera: &Camera,
    ) {
        self.count = u32::try_from(instances.len()).unwrap_or(u32::MAX);
        // Counted here rather than plumbed down from `ParticleFrame` because
        // this is the last place that sees the bytes actually being uploaded:
        // a caller that extracted one list and uploaded another would make the
        // frame report a lie, and this counter is the thing `gpu.rs` uses to
        // warn about a missing sheet texture.
        self.sheet_count = u32::try_from(
            instances
                .iter()
                .filter(|i| i.atlas == SpriteAtlas::Sheet as u32)
                .count(),
        )
        .unwrap_or(u32::MAX);

        // Partition here rather than asking `Particles::extract` to emit the two
        // layers in order. Vanilla splits the same particle group across two
        // draws — the `solid` phase before translucent terrain and `afterTerrain`
        // after it — and this pass reproduces that with one buffer and two
        // instance ranges, which only works if the buffer is partitioned.
        //
        // Doing it at the last place that sees the uploaded bytes means the
        // invariant cannot be broken by a producer: the module doc above rejects
        // an atlas-partitioned instance list for exactly the reason that a
        // future sort would silently undo it, and the same objection would apply
        // to a layer-partitioned one built upstream. `atlas` stays per-instance,
        // so nothing about that fix depends on this ordering either.
        self.ordered.clear();
        self.ordered
            .extend(instances.iter().filter(|i| i.translucent == 0));
        self.opaque_count = u32::try_from(self.ordered.len()).unwrap_or(u32::MAX);
        self.ordered
            .extend(instances.iter().filter(|i| i.translucent != 0));

        if self.count == 0 {
            return;
        }

        if self.count > self.capacity {
            self.capacity = self.count.next_power_of_two();
            self.instances = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("lodestone-particle-instances"),
                size: u64::from(self.capacity) * std::mem::size_of::<ParticleInstance>() as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(&self.ordered));

        // Camera-relative positions, so fold the camera translation into the
        // matrix rather than adding it back per vertex.
        let view = camera.view_matrix();
        let uniform = ParticleUniform {
            view_proj: (camera.projection_matrix()
                * view
                * glam::Mat4::from_translation(camera.position))
            .to_cols_array_2d(),
            // The view matrix's rows are the camera basis in world space; in
            // glam's column-major `Mat4` that is one component from each column.
            right: [view.x_axis.x, view.y_axis.x, view.z_axis.x, 0.0],
            up: [view.x_axis.y, view.y_axis.y, view.z_axis.y, 0.0],
        };
        queue.write_buffer(&self.cam_buffer, 0, bytemuck::bytes_of(&uniform));
    }

    /// Billboards uploaded by the last [`prepare`](Self::prepare) — i.e. what
    /// [`draw`](Self::draw) will submit.
    pub fn count(&self) -> usize {
        self.count as usize
    }

    /// Of [`count`](Self::count), how many sample the **particle sheet**.
    ///
    /// Non-zero here with no sheet texture installed is a wiring defect, not a
    /// quiet frame — see [`ParticleFrame::sheet_drawn`].
    pub fn sheet_count(&self) -> usize {
        self.sheet_count as usize
    }

    /// Of [`count`](Self::count), how many are [`Layer::Opaque`] — i.e. what
    /// [`draw_opaque`](Self::draw_opaque) will submit. The remainder is
    /// [`draw`](Self::draw)'s.
    pub fn opaque_count(&self) -> usize {
        self.opaque_count as usize
    }

    /// Record the **opaque-layer** draw, which must run *before* translucent
    /// water. No-op when the last [`prepare`](Self::prepare) produced no opaque
    /// instances.
    ///
    /// This is vanilla's `solid` submission of the particle group: block-break
    /// debris, crits, flame, bubbles and the rest of [`Layer::Opaque`] go in
    /// here, with depth write on, so the water surface blends over the ones
    /// beneath it and depth-rejects over the ones in front of it. See
    /// [`ParticleRenderer::new`] on the pipelines and `gpu/frame.rs`'s module
    /// doc on the ordering rule.
    pub fn draw_opaque(&self, pass: &mut wgpu::RenderPass<'_>, atlas: &wgpu::BindGroup) {
        self.draw_range(pass, atlas, &self.opaque_pipeline, 0, self.opaque_count);
    }

    /// Record the **translucent-layer** draw, which runs after translucent
    /// water as vanilla's `afterTerrain` phase does. No-op when the last
    /// [`prepare`](Self::prepare) produced no translucent instances.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, atlas: &wgpu::BindGroup) {
        self.draw_range(pass, atlas, &self.pipeline, self.opaque_count, self.count);
    }

    /// The half of a draw both layers share.
    ///
    /// The instance range is expressed as a **vertex-buffer byte offset** rather
    /// than as a non-zero `first_instance`, because `first_instance` interacts
    /// with backend feature gates (`INDIRECT_FIRST_INSTANCE`) and an offset
    /// slice does not.
    fn draw_range(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        atlas: &wgpu::BindGroup,
        pipeline: &wgpu::RenderPipeline,
        first: u32,
        end: u32,
    ) {
        if end <= first {
            return;
        }
        let stride = std::mem::size_of::<ParticleInstance>() as u64;
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &self.cam_bind_group, &[]);
        pass.set_bind_group(1, atlas, &[]);
        pass.set_vertex_buffer(0, self.instances.slice(u64::from(first) * stride..));
        pass.draw(0..4, 0..(end - first));
    }

    /// The camera bind-group layout, exposed so a caller can rebuild the
    /// uniform binding if it owns the buffer.
    #[must_use]
    pub fn camera_layout(&self) -> &wgpu::BindGroupLayout {
        &self.cam_layout
    }
}

const SHADER: &str = include_str!("shaders/particles.wgsl");

#[cfg(test)]
mod tests {
    use super::*;

    /// Installs a `(Sheet, frame) -> UV` table for every sheet
    /// `spawn_particles` can dispatch to, mirroring
    /// `sheet_particle_resolves_with_an_atlas`'s single-sheet fixture but wide
    /// enough to resolve flame, smoke and crit in the same test.
    fn resolvable() -> Particles {
        let mut p = Particles::new(None);
        let rect = [0.0f32, 0.0, 0.0625, 0.0625];
        p.sheet_uv = Arc::new(HashMap::from([
            ((Sheet::Flame, 0u16), rect),
            ((Sheet::Generic, 0u16), rect),
            ((Sheet::CriticalHit, 0u16), rect),
            ((Sheet::SweepAttack, 0u16), rect),
            ((Sheet::SweepAttack, 2u16), rect),
            ((Sheet::Note, 0u16), rect),
            ((Sheet::Heart, 0u16), rect),
            ((Sheet::Angry, 0u16), rect),
            ((Sheet::Glint, 0u16), rect),
            ((Sheet::Spell, 0u16), rect),
            ((Sheet::Glitter, 0u16), rect),
            ((Sheet::Explosion, 0u16), rect),
            ((Sheet::Spark, 0u16), rect),
        ]));
        p
    }

    /// `count > 0` must spawn exactly `count` particles of a resolvable
    /// sheet-sourced type, and every one of them must draw (`unresolved ==
    /// 0`) — the hermetic proof that `NetUpdate::Particles`'s payload reaches
    /// the emitter and comes out the other side as live, drawable particles.
    #[test]
    fn spawn_particles_emits_exactly_count_flame_particles_all_resolved() {
        let mut p = resolvable();
        p.spawn_particles(
            "flame",
            [0.5, 65.0, 0.5],
            [0.1, 0.1, 0.1],
            0.02,
            7,
            ParticleOptions::None,
        );
        assert_eq!(
            p.engine.particles().len(),
            7,
            "count must be honoured exactly"
        );

        let frame = p.extract(&Camera::default(), 0.0, &|_, _, _| {
            Some(lodestone_particle::FULL_BRIGHT)
        });
        assert_eq!(frame.alive, 7);
        assert_eq!(frame.unresolved, 0, "flame's sheet is in the table");
        assert_eq!(frame.drawn, 7);
        assert_eq!(
            frame.sheet_drawn, 7,
            "every one of these addresses the particle sheet, not the block atlas"
        );
    }

    /// The particle batch plus the sweep-attack particle split
    /// out of that fix: proves each new `kind` string reaches its emitter through
    /// the *generic* dispatch (`spawn_particles` → `spawn_one`), the same
    /// path a `/particle` command or any datapack's `sendParticles` call
    /// uses — not merely that calling `emit::foo` directly produces a
    /// particle. Before this pass every one of these fell into `spawn_one`'s
    /// `other => debug!` catch-all and was silently dropped, exactly like the
    /// ~119 types that fix's issue body counted.
    #[test]
    fn every_newly_wired_kind_reaches_its_emitter_through_the_generic_dispatch() {
        let cases: &[(&str, [f32; 3])] = &[
            ("sweep_attack", [0.0, 0.0, 0.0]),
            ("note", [0.5, 0.0, 0.0]),
            ("heart", [0.0, 0.0, 0.0]),
            ("angry_villager", [0.0, 0.0, 0.0]),
            ("happy_villager", [0.0, 0.0, 0.0]),
            ("witch", [0.0, 0.0, 0.0]),
            ("totem_of_undying", [0.0, 0.2, 0.0]),
            // `explosion`. `count > 0` (every case in this loop
            // uses `count == 1`) draws velocity from `gaussian() * max_speed`
            // with `max_speed == 0.0`, so `xa` (this dispatch arm's `size`
            // parameter) is always exactly `0.0` here regardless of `offset`
            // — reachability is what this loop proves, not a specific
            // `size`; `huge_explosion_matches_the_exact_vanilla_formulas` in
            // `lodestone-particle` already pins the formula itself.
            // `explosion_emitter` is deliberately not in this shared loop: it
            // is a `NoRenderParticle` that produces zero quads on its own
            // (see `explosion_emitter_reaches_pixels_only_after_a_tick`
            // below), so it would fail this loop's `drawn == 1` assertion for
            // a reason that has nothing to do with dispatch being broken.
            ("explosion", [0.0, 0.0, 0.0]),
            // `firework` (`FireworkParticles.SparkParticle`/`SparkProvider`):
            // the dispatch arm this module was missing while `emit::firework`
            // itself already existed -- see `docs/particle-catalogue.md`'s
            // "Correction" entry. `count == 1` here (like every other case in
            // this loop) draws position jitter from `gaussian() * offset` and
            // velocity from `gaussian() * max_speed` with `max_speed == 0.0`,
            // so this proves reachability, not a specific spark velocity.
            ("firework", [0.3, 0.1, -0.2]),
        ];
        for &(kind, offset) in cases {
            let mut p = resolvable();
            p.spawn_particles(kind, [0.5, 65.0, 0.5], offset, 0.0, 1, ParticleOptions::None);
            assert_eq!(
                p.engine.particles().len(),
                1,
                "{kind:?} must spawn exactly one particle via the generic dispatch"
            );
            let frame = p.extract(&Camera::default(), 0.0, &|_, _, _| {
                Some(lodestone_particle::FULL_BRIGHT)
            });
            assert_eq!(frame.unresolved, 0, "{kind:?} must resolve against its sheet");
            assert_eq!(frame.drawn, 1, "{kind:?} must produce exactly one instance");
            assert_eq!(
                frame.sheet_drawn, 1,
                "{kind:?} must address the particle sheet, not the block atlas"
            );
        }
    }

    /// Negative control for the test above: an unrecognised kind must still
    /// fall into the catch-all rather than one of the new arms accidentally
    /// matching a substring or prefix.
    #[test]
    fn a_near_miss_kind_still_falls_into_the_catch_all() {
        let mut p = resolvable();
        for kind in [
            "sweep",
            "note_block",
            "heartbeat",
            "totem",
            "explosions",
            "explode",
            "fireworks",
            "firework_rocket",
        ] {
            p.spawn_particles(kind, [0.0, 64.0, 0.0], [0.0; 3], 0.0, 3, ParticleOptions::None);
        }
        assert!(
            p.engine.particles().is_empty(),
            "a near-miss kind must not match any of the new dispatch arms"
        );
    }

    /// The `dust` gap this pass closed: before it, `LEVEL_PARTICLES`'s option
    /// bytes were captured and then thrown away entirely, so even a wired
    /// `"dust"` dispatch arm would have had no colour to draw with. This pins
    /// the whole chain from a decoded `ParticleOptions::Dust` payload through
    /// to a resolved, drawn instance addressing `Sheet::Generic` (confirmed
    /// against the real `dust.json`, which lists the same eight
    /// `generic_0..generic_7` textures as `Sheet::Generic` itself) --
    /// pairwise-distinct RGB values so a channel transposition could not
    /// survive this test unnoticed.
    #[test]
    fn dust_with_a_decoded_payload_reaches_the_emitter_and_resolves() {
        let mut p = resolvable();
        p.spawn_particles(
            "dust",
            [0.5, 65.0, 0.5],
            [0.0; 3],
            0.0,
            1,
            ParticleOptions::Dust { color: [0.75, 0.25, 0.5], scale: 2.0 },
        );
        assert_eq!(p.engine.particles().len(), 1, "a decoded dust payload must dispatch");
        let particle = &p.engine.particles()[0];
        assert_ne!(
            particle.colour, [1.0, 1.0, 1.0],
            "the decoded colour must actually reach the particle, not the \
             SingleQuadParticle white default"
        );

        let frame = p.extract(&Camera::default(), 0.0, &|_, _, _| {
            Some(lodestone_particle::FULL_BRIGHT)
        });
        assert_eq!(frame.drawn, 1);
        assert_eq!(frame.unresolved, 0, "dust shares Sheet::Generic, already in the fixture");
        assert_eq!(
            frame.sheet_drawn, 1,
            "dust must address the particle sheet, not the block atlas"
        );
    }

    /// The sibling type, and the reason [`Behaviour::DustColorTransition`]
    /// exists separately from [`Behaviour::Dust`]: its colour must move
    /// between the two ends of the transition as it ages rather than staying
    /// fixed, which a shared-behaviour implementation could get away without
    /// ever doing.
    #[test]
    fn dust_color_transition_lerps_from_its_starting_colour_as_it_ages() {
        struct NoCollision;
        impl CollisionView for NoCollision {
            fn collision_boxes(&self, _x: i32, _y: i32, _z: i32, _out: &mut Vec<lodestone_physics::Aabb>) {}
        }

        let mut p = resolvable();
        p.spawn_particles(
            "dust_color_transition",
            [0.5, 65.0, 0.5],
            [0.0; 3],
            0.0,
            1,
            ParticleOptions::DustColorTransition {
                from_color: [1.0, 0.0, 0.0],
                to_color: [0.0, 0.0, 1.0],
                scale: 1.0,
            },
        );
        assert_eq!(p.engine.particles().len(), 1);
        let start_colour = p.engine.particles()[0].colour;
        assert!(
            start_colour[0] > start_colour[2],
            "at age 0 the lerp fraction is 0, so colour must still favour \
             from_color's red over to_color's blue, got {start_colour:?}"
        );

        // The randomised colour factors (`randomize_dust_channel`) differ per
        // channel, so a mid-transition sample cannot be compared safely --
        // tick to the particle's own `lifetime` (still alive: `tick_base`
        // only removes once `age > lifetime`) instead of a fixed guess, so
        // the lerp fraction lands close to 1 regardless of which lifetime the
        // engine's entropy-seeded RNG happened to draw. At that fraction
        // red's contribution is bounded above by `from_r * 1/(lifetime+1)`
        // and blue's below by `to_b * lifetime/(lifetime+1)`, which cannot
        // invert for any pair of per-channel random factors.
        let lifetime = p.engine.particles()[0].lifetime;
        for _ in 0..lifetime {
            p.tick(&NoCollision);
        }
        assert_eq!(p.engine.particles().len(), 1, "the particle must still be alive at age == lifetime");
        let later_colour = p.engine.particles()[0].colour;
        assert!(
            later_colour[2] > later_colour[0],
            "near the end of its life the lerp must have moved decisively \
             towards to_color's blue, got {later_colour:?} (started at \
             {start_colour:?}, lifetime {lifetime})"
        );
    }

    /// `explosion_emitter` is the one dispatch-reachable kind in
    /// this module that is invisible on its own — `HugeExplosionSeedParticle`
    /// is a `NoRenderParticle`, so `frame.drawn` must stay `0` immediately
    /// after dispatch even though the seed *is* live in the engine. Only
    /// after a real tick (`Particles::tick`, the same call `sim.rs`'s frame
    /// loop makes) does it seed its six `explosion` follow-ups, which must
    /// then resolve and draw. This is the reachability proof for a kind
    /// whose own particle produces zero pixels by design — `frame.drawn == 0`
    /// right after dispatch is the *correct* value, not evidence dispatch is
    /// broken, which is exactly why `every_newly_wired_kind_reaches_its_
    /// emitter_through_the_generic_dispatch` above excludes it rather than
    /// asserting the wrong thing.
    #[test]
    fn explosion_emitter_reaches_pixels_only_after_a_tick() {
        struct NoCollision;
        impl CollisionView for NoCollision {
            fn collision_boxes(&self, _x: i32, _y: i32, _z: i32, _out: &mut Vec<lodestone_physics::Aabb>) {}
        }

        let mut p = resolvable();
        p.spawn_particles(
            "explosion_emitter",
            [0.5, 65.0, 0.5],
            [0.0; 3],
            0.0,
            1,
            ParticleOptions::None,
        );
        assert_eq!(
            p.engine.particles().len(),
            1,
            "explosion_emitter must reach the dispatch and spawn its one seed particle"
        );

        let before = p.extract(&Camera::default(), 0.0, &|_, _, _| {
            Some(lodestone_particle::FULL_BRIGHT)
        });
        assert_eq!(
            before.drawn, 0,
            "the seed is a NoRenderParticle and must draw nothing on its own, \
             before any tick has run its spawn schedule"
        );

        p.tick(&NoCollision);
        let after = p.extract(&Camera::default(), 0.0, &|_, _, _| {
            Some(lodestone_particle::FULL_BRIGHT)
        });
        assert_eq!(
            after.drawn, 6,
            "one seed tick must produce six drawable HugeExplosion follow-ups \
             through the real Particles::tick path, not a direct emit:: call"
        );
        assert_eq!(after.unresolved, 0, "explosion must resolve against Sheet::Explosion");
        assert_eq!(
            after.sheet_drawn, 6,
            "every follow-up must address the particle sheet, not the block atlas"
        );
    }

    /// The atlas a resolved UV belongs to must reach the instance, and the two
    /// sources must land on **different** selectors.
    ///
    /// This is the hermetic half of `tests/sheet_particle_atlas_pixels.rs`
    /// (which judges the same thing in pixels against the real stitches): the
    /// tables are installed directly, so the assertion is on the *pairing* —
    /// `sprite_rect`'s two arms tagging their rects — with no dependency on a
    /// GPU or a jar. It is deliberately a **paired** test: `Sheet == 1` alone
    /// is satisfied by a constant, and `Block == 0` alone by a zeroed field.
    #[test]
    fn an_instances_atlas_selector_distinguishes_a_sheet_sprite_from_a_block_sprite() {
        let rect = [0.0f32, 0.0, 0.0625, 0.0625];
        let mut p = Particles::new(None);
        p.state_uv = Arc::new(vec![None, Some(rect)]);
        p.sheet_uv = Arc::new(HashMap::from([((Sheet::Flame, 0u16), rect)]));

        // Terrain debris — the block-model atlas.
        p.destroy_block([0, 64, 0], 1, [1.0; 3]);
        let terrain = p.extract(&Camera::default(), 0.0, &|_, _, _| {
            Some(lodestone_particle::FULL_BRIGHT)
        });
        assert!(terrain.drawn > 0, "the burst must resolve");
        assert_eq!(
            terrain.sheet_drawn, 0,
            "a block-state sprite must never be tagged as a sheet sprite, or terrain \
             debris would sample the particle stitch"
        );
        assert!(
            p.instances
                .iter()
                .all(|i| i.atlas == SpriteAtlas::Block as u32),
            "every terrain instance must select the block atlas"
        );

        // The same rect, from the sheet — the selector, not the rect, is what
        // tells the shader which texture the numbers belong to.
        p.engine.clear();
        emit::flame(p.engine_mut(), 0.5, 65.0, 0.5, 0.0, 0.05, 0.0);
        let sheet = p.extract(&Camera::default(), 0.0, &|_, _, _| {
            Some(lodestone_particle::FULL_BRIGHT)
        });
        assert!(sheet.drawn > 0, "flame must resolve");
        assert_eq!(
            sheet.sheet_drawn, sheet.drawn,
            "every flame instance must select the particle sheet"
        );
        assert!(
            p.instances
                .iter()
                .all(|i| i.atlas == SpriteAtlas::Sheet as u32),
            "a sheet sprite tagged as a block sprite is issue #45 exactly: the UVs \
             resolve and address the wrong image"
        );
    }

    /// Negative control: an unrecognised particle type must not spawn
    /// anything (dropped, not guessed at), so a caller can tell "no emitter
    /// wired" apart from "wired but unresolved".
    #[test]
    fn spawn_particles_for_an_unknown_type_spawns_nothing() {
        let mut p = resolvable();
        p.spawn_particles(
            "totally_not_a_real_particle",
            [0.0, 64.0, 0.0],
            [0.0; 3],
            0.0,
            5,
            ParticleOptions::None,
        );
        assert!(
            p.engine.particles().is_empty(),
            "an unmapped kind must spawn nothing rather than guess at a sheet"
        );
    }

    /// Vanilla's `count == 0` special case: exactly **one** particle, at the
    /// *exact* position (no positional jitter), whose velocity is
    /// `max_speed * offset` per axis — confirmed against
    /// `ClientPacketListener.handleParticleEvent` in the 26.2 client sources.
    /// This is the case an implementation that reads "count" as "how many to
    /// spawn" gets silently wrong by spawning zero.
    #[test]
    fn count_zero_spawns_one_particle_at_the_exact_position_with_offset_as_velocity() {
        let mut p = resolvable();
        p.spawn_particles(
            "flame",
            [1.0, 64.0, -2.0],
            [0.25, 0.5, -0.25],
            4.0,
            0,
            ParticleOptions::None,
        );
        let particles = p.engine.particles();
        assert_eq!(particles.len(), 1, "count == 0 means exactly one particle");
        let particle = &particles[0];
        // `FlameParticle`'s constructor adds its own small (< 0.05 per axis)
        // positional jitter on top of whatever position it is constructed
        // at, so "no positional jitter" is checked as "close to `pos`", not
        // bit-exact — the property under test is that `spawn_particles`
        // itself never applies the `gaussian() * offset` jitter the `count >
        // 0` branch uses, not that nothing downstream ever perturbs it.
        assert!(
            (particle.x - 1.0).abs() < 0.1
                && (particle.y - 64.0).abs() < 0.1
                && (particle.z - -2.0).abs() < 0.1,
            "count == 0 must not apply spawn_particles's own positional jitter, got ({}, {}, {})",
            particle.x,
            particle.y,
            particle.z
        );
        // `FlameParticle`'s constructor almost entirely discards the seeded
        // scatter component (a `* 0.01` damp) and replaces it with the
        // requested velocity, so the resulting `xd`/`yd`/`zd` should track
        // `max_speed * offset` closely rather than the request being ignored.
        assert!(
            (particle.xd - 4.0 * 0.25).abs() < 0.05,
            "xd {} should track max_speed * offset.x = 1.0",
            particle.xd
        );
        assert!(
            (particle.zd - 4.0 * -0.25).abs() < 0.05,
            "zd {} should track max_speed * offset.z = -1.0",
            particle.zd
        );
    }

    /// A `count > 0` burst must scatter around `pos`, not stack every
    /// particle on top of it — the observable difference between "offset
    /// consumed as jitter" and "offset ignored".
    #[test]
    fn count_greater_than_zero_scatters_positions_around_pos() {
        let mut p = resolvable();
        p.spawn_particles(
            "flame",
            [0.0, 64.0, 0.0],
            [1.0, 1.0, 1.0],
            0.0,
            64,
            ParticleOptions::None,
        );
        let particles = p.engine.particles();
        assert_eq!(particles.len(), 64);
        let distinct_x = particles
            .iter()
            .map(|particle| particle.x.to_bits())
            .collect::<std::collections::HashSet<_>>()
            .len();
        assert!(
            distinct_x > 1,
            "a count > 0 burst with a nonzero offset must scatter positions, not clone one point"
        );
    }

    /// No models loaded (the offline demo world) must report unresolved rather
    /// than pretending the frame was empty — a silently-zero particle count is
    /// indistinguishable from "the emitter never fired", which is exactly the
    /// confusion this counter exists to prevent.
    #[test]
    fn terrain_particles_without_models_are_counted_unresolved() {
        let mut p = Particles::new(None);
        p.destroy_block([0, 64, 0], 1, [1.0, 1.0, 1.0]);
        assert!(
            p.engine.particles().len() >= 64,
            "a full cube throws 4^3 fragments; got {}",
            p.engine.particles().len()
        );

        let camera = Camera::default();
        let frame = p.extract(&camera, 0.0, &|_, _, _| Some(lodestone_particle::FULL_BRIGHT));
        assert_eq!(frame.drawn, 0, "no atlas, so nothing can be drawn");
        assert_eq!(
            frame.unresolved, frame.alive,
            "every live particle must be accounted for as unresolved, not dropped"
        );
    }

    /// With a sprite table present the same burst resolves and produces
    /// instances whose UVs land inside the declared sprite rect. This is the
    /// positive control for the test above: without it, an `extract` that
    /// resolved *nothing at all* would still satisfy the unresolved assertion.
    #[test]
    fn resolved_terrain_particles_produce_instances_inside_the_sprite_rect() {
        let rect = [0.25f32, 0.5, 0.3125, 0.5625];
        let mut p = Particles::new(None);
        p.state_uv = Arc::new(vec![None, Some(rect)]);
        p.destroy_block([0, 64, 0], 1, [1.0, 1.0, 1.0]);

        let camera = Camera::default();
        let frame = p.extract(&camera, 0.0, &|_, _, _| Some(lodestone_particle::FULL_BRIGHT));
        assert_eq!(frame.unresolved, 0);
        assert_eq!(frame.drawn, frame.alive);
        assert!(frame.drawn >= 64);

        for inst in &p.instances {
            for (i, uv) in inst.uv.iter().enumerate() {
                let (lo, hi) = if i % 2 == 0 {
                    (rect[0], rect[2])
                } else {
                    (rect[1], rect[3])
                };
                assert!(
                    *uv >= lo - 1e-5 && *uv <= hi + 1e-5,
                    "UV {uv} escaped the sprite rect {lo}..{hi} — a terrain fragment \
                     would sample a neighbouring block's texture"
                );
            }
            assert!(inst.centre_size[3] > 0.0, "a zero-size quad draws nothing");
        }
    }

    /// The light term must match the model shader's, which is now vanilla's own
    /// `lightmap.fsh` curve rather than the retired `0.2 + 0.8 * max(sky,
    /// block)` ramp. A particle lit differently from the block it came from
    /// reads as a rendering bug in the terrain, not in the particle.
    ///
    /// Every expectation below is written out from `level / (4 - 3 * level)` and
    /// `notGamma` at vanilla's default gamma of 0.5 — **not** read back from
    /// `lodestone_render::light`, which is the code under test here. The retired
    /// ramp's value is computed alongside each one, because the two curves agree
    /// at both endpoints and a full-bright-only assertion passes on either.
    #[test]
    fn light_term_matches_the_terrain_shader() {
        let rect = [0.0f32, 0.0, 1.0, 1.0];
        let mut p = Particles::new(None);
        p.state_uv = Arc::new(vec![None, Some(rect)]);
        p.destroy_block([0, 64, 0], 1, [1.0, 1.0, 1.0]);

        // The shade now travels in its own instance lane instead of being folded
        // into `colour`, so these read it directly rather than dividing out the
        // 0.6 `TerrainParticle` scales the block colour by in its constructor.
        // Full bright must be exactly 1.0 — `apply_brightness_option` is the
        // identity at 1.0, which is what keeps every full-bright path in the tree
        // byte-identical.
        let shade_of = |p: &Particles| p.instances[0].roll_light[1];
        let frame = p.extract(&Camera::default(), 0.0, &|_, _, _| {
            Some(lodestone_particle::FULL_BRIGHT)
        });
        assert!(frame.drawn > 0);
        let base = shade_of(&p);
        assert!(
            (base - 1.0).abs() < 1e-6,
            "a full-bright particle must shade at exactly 1.0, got {base}"
        );
        // The tint itself must survive un-multiplied now, since the shader is
        // what applies the shade: a build that still folded light into `colour`
        // would leave 0.6 here under full bright and something smaller under any
        // other light.
        assert!(
            (p.instances[0].colour[0] - 0.6).abs() < 1e-5,
            "instance colour {} is not `TerrainParticle`'s bare 0.6 tint — the light \
             term is being premultiplied into it again, which is the gamma-space bug",
            p.instances[0].colour[0]
        );

        // Block light 0, sky light 0. `get_brightness(0)` is 0, but vanilla seeds
        // the accumulator with `AmbientColor` — `0x0A0A0A` in the overworld, per
        // `DimensionTypes.java` — so an unlit particle is not black either: it
        // reads 0.0935 once `notGamma` is mixed in. The retired ramp floored it at
        // 0.2, which is still the floor that fix named; the correct replacement
        // is a *smaller* floor, not none.
        //
        // This is also why the value below is worth asserting at all. Against a
        // pure-black expectation, an unlit shade is 0.000 under any build that
        // darkens — including one that draws nothing at all — so the assertion
        // would have been vacuous in the sense CLAUDE.md calls the *world*
        // species. A non-zero floor makes it discriminating again.
        let ambient = 10.0_f32 / 255.0;
        let floor = ambient + ((1.0 - (1.0 - ambient).powi(4)) - ambient) * 0.5;
        assert!((floor - 0.093_545).abs() < 1e-5, "hypothesis drifted: {floor}");
        let _ = p.extract(&Camera::default(), 0.0, &|_, _, _| Some(0));
        let dark = shade_of(&p);
        assert!(
            (dark - floor).abs() < 1e-5,
            "unlit particle shade {dark} must be vanilla's ambient floor {floor} — not pure \
             black, and not the retired ramp's 0.2"
        );

        // The interior of the curve, which is where the hypotheses differ most.
        // Block light 8: `get_brightness(8/15) = 0.2222`, plus ambient `10/255`,
        // and mixing `notGamma` in at 0.5 gives 0.4819. Dropping ambient gives
        // 0.4281 and the retired ramp 0.6267.
        let level: f32 = 8.0 / 15.0;
        let curved = level / (4.0 - 3.0 * level);
        let mix = |c: f32| c + ((1.0 - (1.0 - c).powi(4)) - c) * 0.5;
        let vanilla = mix(curved + ambient);
        let ambient_free = mix(curved);
        let retired_ramp = 0.2 + 0.8 * level;
        assert!((vanilla - 0.481_948).abs() < 1e-5, "hypothesis drifted: {vanilla}");
        assert!(
            (ambient_free - 0.428_136).abs() < 1e-5,
            "hypothesis drifted: {ambient_free}"
        );
        assert!((retired_ramp - 0.626_667).abs() < 1e-5, "hypothesis drifted: {retired_ramp}");
        let _ = p.extract(&Camera::default(), 0.0, &|_, _, _| Some(8 << 4));
        let mid = shade_of(&p);
        assert!(
            (mid - vanilla).abs() < 1e-5,
            "block light 8 must shade at vanilla's {vanilla} — not the retired ramp's \
             {retired_ramp} and not the ambient-free {ambient_free}; got {mid}"
        );

        // Sky-only and block-only must agree: the shader takes the max, so a
        // particle in full skylight is as bright as one beside a torch.
        let _ = p.extract(&Camera::default(), 0.0, &|_, _, _| Some(15 << 20));
        let sky_only = shade_of(&p);
        assert!(
            (sky_only - base).abs() < 1e-5,
            "sky-lit particle {sky_only} != block-lit {base}"
        );
    }

    /// A state's own particle tint must reach the emitted fragments, and an
    /// untinted state must be left alone.
    ///
    /// This is the hermetic half of `tests/break_particle_tint.rs` (which judges
    /// the same thing against the real vanilla atlas): here the table is
    /// installed directly, so the assertion is on the *wiring* —
    /// `state_tint_of`'s multiply reaching `TerrainParticle`'s colour — with no
    /// dependency on which blocks vanilla happens to tint.
    #[test]
    fn a_states_particle_tint_reaches_the_emitted_fragments() {
        let rect = [0.0f32, 0.0, 1.0, 1.0];
        let mut p = Particles::new(None);
        p.state_uv = Arc::new(vec![None, Some(rect), Some(rect)]);
        // State 1 untinted, state 2 tinted green.
        let green = [0.5f32, 0.75, 0.25];
        p.state_tint = Arc::new(vec![[1.0; 3], [1.0; 3], green]);
        assert_eq!(p.tinted_state_count(), 1, "one of the three states is tinted");

        let colour_of = |p: &mut Particles, state: u32| -> [f32; 4] {
            p.engine.clear();
            p.destroy_block([0, 64, 0], state, [1.0; 3]);
            let frame = p.extract(&Camera::default(), 0.0, &|_, _, _| {
                Some(lodestone_particle::FULL_BRIGHT)
            });
            assert!(frame.drawn > 0, "state {state} drew nothing");
            p.instances[0].colour
        };

        // The control: an untinted state must be pure grey, so the tinted case
        // below cannot be satisfied by something that colours every particle.
        let plain = colour_of(&mut p, 1);
        assert!(
            (plain[0] - plain[1]).abs() < 1e-6 && (plain[1] - plain[2]).abs() < 1e-6,
            "an untinted state must stay grey, got {plain:?}"
        );
        assert!(plain[0] > 0.0, "a black particle makes the ratio meaningless");

        let tinted = colour_of(&mut p, 2);
        for c in 0..3 {
            assert!(
                (tinted[c] / plain[0] - green[c]).abs() < 1e-5,
                "channel {c}: tinted {} / untinted {} = {}, expected the state tint {}",
                tinted[c],
                plain[0],
                tinted[c] / plain[0],
                green[c]
            );
        }

        // A caller-supplied tint composes with the state's, rather than being
        // ignored or replacing it.
        p.engine.clear();
        p.destroy_block([0, 64, 0], 2, [0.5, 0.5, 0.5]);
        let _ = p.extract(&Camera::default(), 0.0, &|_, _, _| {
            Some(lodestone_particle::FULL_BRIGHT)
        });
        let composed = p.instances[0].colour;
        assert!(
            (composed[1] / plain[0] - green[1] * 0.5).abs() < 1e-5,
            "caller tint must multiply the state tint, got {}",
            composed[1] / plain[0]
        );
    }

    /// A state id past the end of the tint table must not panic — it is the same
    /// id that is about to be counted into [`ParticleFrame::unresolved`], and one
    /// report of a bad id is enough.
    #[test]
    fn an_out_of_range_state_id_falls_back_to_the_callers_tint() {
        let mut p = Particles::new(None);
        p.state_tint = Arc::new(vec![[1.0; 3]]);
        p.destroy_block([0, 64, 0], 9_999, [1.0; 3]);
        let frame = p.extract(&Camera::default(), 0.0, &|_, _, _| {
            Some(lodestone_particle::FULL_BRIGHT)
        });
        assert!(frame.alive > 0, "the burst must still be emitted");
        assert_eq!(
            frame.unresolved, frame.alive,
            "an unknown state resolves to no sprite and must be reported, not drawn"
        );
    }

    /// Ticking must retire particles, or a single break leaks 64 quads for the
    /// rest of the session.
    #[test]
    fn particles_expire() {
        struct Air;
        impl CollisionView for Air {
            fn collision_boxes(
                &self,
                _x: i32,
                _y: i32,
                _z: i32,
                _out: &mut Vec<lodestone_physics::Aabb>,
            ) {
            }
        }

        let mut p = Particles::new(None);
        p.destroy_block([0, 64, 0], 1, [1.0, 1.0, 1.0]);
        let start = p.engine.particles().len();
        assert!(start >= 64);
        for _ in 0..200 {
            p.tick(&Air);
        }
        assert_eq!(
            p.engine.particles().len(),
            0,
            "every fragment's lifetime is well under 200 ticks"
        );
    }

    /// Sheet-sourced particles (smoke, flame, crits, splashes, …) have no
    /// resolution table by default — the same "counted, not dropped"
    /// discipline as the terrain case above, but for `SpriteSource::Sheet`.
    /// This is the negative control for
    /// [`sheet_particle_resolves_with_an_atlas`] below: it proves the gap is
    /// actually observed firing, not merely assumed.
    #[test]
    fn sheet_particle_without_atlas_is_counted_unresolved() {
        let mut p = Particles::new(None);
        emit::flame(p.engine_mut(), 0.5, 65.0, 0.5, 0.0, 0.05, 0.0);
        assert!(!p.engine.particles().is_empty(), "flame must emit a particle");

        let frame = p.extract(&Camera::default(), 0.0, &|_, _, _| {
            Some(lodestone_particle::FULL_BRIGHT)
        });
        eprintln!(
            "flame, no atlas: alive={} drawn={} unresolved={}",
            frame.alive, frame.drawn, frame.unresolved
        );
        assert_eq!(
            frame.drawn, 0,
            "no particle atlas attached, so nothing can draw"
        );
        assert_eq!(
            frame.unresolved, frame.alive,
            "every live sheet-sourced particle must be counted unresolved, not dropped"
        );
        assert!(frame.unresolved > 0, "the negative control must actually fire");
    }

    /// With a `(Sheet, frame)` table present the same emission resolves.
    /// Mirrors [`resolved_terrain_particles_produce_instances_inside_the_sprite_rect`]:
    /// the table is populated directly (bypassing `ParticleAtlas`/jar I/O,
    /// which [`sheet_particle_resolves_against_the_real_particle_atlas`]
    /// below covers) so this stays a fast, hermetic gate on the resolution
    /// *mechanism* — `sprite_rect`'s `Sheet` arm and the `unresolved` count —
    /// rather than on atlas stitching.
    #[test]
    fn sheet_particle_resolves_with_an_atlas() {
        let rect = [0.5f32, 0.0, 0.5625, 0.0625];
        let mut p = Particles::new(None);
        p.sheet_uv = Arc::new(HashMap::from([((Sheet::Flame, 0u16), rect)]));
        emit::flame(p.engine_mut(), 0.5, 65.0, 0.5, 0.0, 0.05, 0.0);
        let alive = p.engine.particles().len();
        assert!(alive > 0, "flame must emit a particle");

        let frame = p.extract(&Camera::default(), 0.0, &|_, _, _| {
            Some(lodestone_particle::FULL_BRIGHT)
        });
        eprintln!(
            "flame, with atlas: alive={} drawn={} unresolved={}",
            frame.alive, frame.drawn, frame.unresolved
        );
        assert_eq!(
            frame.unresolved, 0,
            "flame's (Sheet, frame) is in the table, so nothing should be unresolved"
        );
        assert_eq!(frame.drawn, alive);

        for inst in &p.instances {
            for (i, uv) in inst.uv.iter().enumerate() {
                let (lo, hi) = if i % 2 == 0 {
                    (rect[0], rect[2])
                } else {
                    (rect[1], rect[3])
                };
                assert!(
                    *uv >= lo - 1e-5 && *uv <= hi + 1e-5,
                    "UV {uv} escaped the sprite rect {lo}..{hi} — a flame particle \
                     would sample a neighbouring sheet frame"
                );
            }
        }
    }

    /// End-to-end against the real vanilla particle atlas: builds
    /// [`ParticleAtlas`] from the same jar `resources::vanilla_manager` opens
    /// for the other GPU/jar gates, attaches it via
    /// [`Particles::with_particle_atlas`], and checks that real flame, smoke
    /// and crit emissions resolve. A synthetic fixture (as in the test above)
    /// cannot catch a wrong sprite-naming convention — e.g. forgetting the
    /// `particle/` directory segment `Sheet::texture_name` bakes in — because
    /// it never exercises the real jar's actual paths; this test does.
    #[test]
    #[ignore = "requires a fetched vanilla client.jar (see crate::resources::vanilla_manager)"]
    fn sheet_particle_resolves_against_the_real_particle_atlas() {
        let manager = crate::resources::vanilla_manager()
            .expect("no vanilla client.jar under .cache/mc/<version>/; fetch it first");
        let (atlas, report) = ParticleAtlas::build_reported(&manager)
            .expect("build particle atlas from the real jar");
        eprintln!(
            "particle atlas: definitions={} sprites={} atlas={}x{}",
            report.definitions,
            report.sprites,
            atlas.atlas().width,
            atlas.atlas().height
        );
        assert!(report.missing_textures.is_empty(), "{:?}", report.missing_textures);

        let mut p = Particles::new(None).with_particle_atlas(Some(&atlas));
        emit::flame(p.engine_mut(), 0.5, 65.0, 0.5, 0.0, 0.05, 0.0);
        emit::smoke(p.engine_mut(), 0.5, 65.0, 0.5, 0.0, 0.0, 0.0, 1.0);
        emit::crit(p.engine_mut(), 0.5, 65.0, 0.5, 0.0, 0.0, 0.0);
        // The particle batch (several closed fixes, plus the sweep-attack particle split
        // out of an earlier one): every one of these names a *new* `Sheet` variant, so
        // this is the only test in the tree that proves the `stem()` chosen
        // for each (`sweep`, `spell`, `angry`, `glint`) actually matches a
        // real file under `textures/particle/` in the jar, rather than a
        // plausible-looking guess. `Sheet::Note`/`Heart`/`Glitter` already
        // existed but had no emitter ever exercising them either.
        emit::sweep_attack(p.engine_mut(), 0.5, 65.0, 0.5, 0.0);
        emit::note(p.engine_mut(), 0.5, 65.0, 0.5, 0.5);
        emit::heart(p.engine_mut(), 0.5, 65.0, 0.5);
        emit::angry_villager(p.engine_mut(), 0.5, 65.0, 0.5);
        emit::happy_villager(p.engine_mut(), 0.5, 65.0, 0.5, 0.0, 0.0, 0.0);
        emit::witch(p.engine_mut(), 0.5, 65.0, 0.5, 0.0, 0.0, 0.0);
        emit::totem_of_undying(p.engine_mut(), 0.5, 65.0, 0.5, 0.0, 0.2, 0.0);
        // `explosion`: `Sheet::Explosion` is `explosion_0`
        // through `explosion_15` — the one sheet in this whole list with a
        // 16-frame stem rather than the usual 8, so this is also the proof
        // that `frame_count()`'s per-frame `explosion_N` naming resolves
        // every one of those sixteen files, not just frame 0.
        emit::huge_explosion(p.engine_mut(), 0.5, 65.0, 0.5, 0.0);
        // The ambient/environmental batch. Ten more `Sheet`
        // variants, and this is the only place that proves each names real files:
        // `soul` is 11 frames, `enchant` is alphabetic (`sga_a`…`sga_z`) rather
        // than numbered at all, `big_smoke` is 12, `sonic_boom` is 16, and the
        // drip phases are three separate single-frame sheets. A hermetic fixture
        // cannot see any of that.
        emit::soul(p.engine_mut(), 0.5, 65.0, 0.5, 0.0, 0.05, 0.0);
        emit::soul_fire_flame(p.engine_mut(), 0.5, 65.0, 0.5, 0.0, 0.05, 0.0);
        emit::portal(p.engine_mut(), 0.5, 65.0, 0.5, 0.25, 0.0, 0.25);
        emit::campfire_smoke(p.engine_mut(), 0.5, 65.0, 0.5, 0.0, 0.07, 0.0, false);
        emit::end_rod(p.engine_mut(), 0.5, 65.0, 0.5, 0.0, 0.0, 0.0);
        emit::spark(p.engine_mut(), 0.5, 65.0, 0.5, 0.0, 0.0, 0.0);
        for sheet in [Sheet::SculkCharge, Sheet::Gust, Sheet::SonicBoom, Sheet::Enchant] {
            emit::animated_ambient(p.engine_mut(), 0.5, 65.0, 0.5, 0.0, 0.0, 0.0, sheet, 1.0, 15);
        }
        for sheet in [Sheet::DripHang, Sheet::DripFall, Sheet::DripLand] {
            emit::drip(p.engine_mut(), 0.5, 65.0, 0.5, sheet, [1.0, 1.0, 1.0], 0.0);
        }
        let alive = p.engine.particles().len();
        assert!(alive >= 24, "every emitter must have added a particle, got {alive}");

        let frame = p.extract(&Camera::default(), 0.0, &|_, _, _| {
            Some(lodestone_particle::FULL_BRIGHT)
        });
        eprintln!(
            "particle batch resolution: alive={} drawn={} unresolved={}",
            frame.alive, frame.drawn, frame.unresolved
        );
        assert_eq!(
            frame.unresolved, 0,
            "every emitted sheet must name a real vanilla texture and resolve \
             against the stitched atlas"
        );
        assert_eq!(frame.drawn, frame.alive);
    }
}

/// How many random block positions the ambient emitter samples per tick, and how
/// far from the eye it looks.
///
/// Vanilla's `ClientLevel.animateTick` draws **667** positions in a ±16 box twice
/// per tick, i.e. ~1.9% of the 33³ volume. `128` in a ±8 box is ~2.6% of 17³ —
/// the same *density* at a fraction of the cost, which is the number that decides
/// how often a given torch flickers. Dropping the density is what makes torches
/// look dead rather than making the scan cheap.
const AMBIENT_SAMPLES: usize = 128;
/// Half-extent of the ambient scan box, in blocks. Bounded by
/// `LiveCollision`'s own 3×3-column snapshot, which is ±16 at best and clips
/// asymmetrically depending on where in its chunk the player stands.
const AMBIENT_RANGE: i32 = 8;

impl Particles {
    /// Emit this tick's **client-predicted** ambient particles — vanilla's
    /// `Block.animateTick`, which is not on the wire at all.
    ///
    /// # Why this cannot be a server-event consumer
    ///
    /// A torch's flame, a nether portal's shimmer and an end rod's sparkle are
    /// spawned by `ClientLevel.animateTick` walking random nearby positions and
    /// calling each block's own `animateTick`. **No packet carries them**, so a
    /// client that only consumed `LEVEL_PARTICLES` would show a torch-lit room
    /// with no flames however complete its dispatch table was. That is the shape
    /// of the gap this closes, and it is why several of the types below *also*
    /// have a `spawn_one` arm: the same type can arrive both ways.
    ///
    /// # The probe, and why it is a closure
    ///
    /// `probe` answers "what block state is at this position" and is injected
    /// rather than taken as a world reference, exactly as `ShellAmbience::tick`
    /// injects its light probe: the two callers hold *different* view types (a
    /// live 3×3 column snapshot, or the offline demo world) and neither is
    /// nameable here. A probe returning `0` (air) for an unloaded position is
    /// correct — nothing should be emitted there.
    ///
    /// Sampling is uniform over a box around `eye`, so the cost is
    /// [`AMBIENT_SAMPLES`] probes per tick regardless of how much is nearby.
    pub fn ambient_tick(&mut self, eye: [f64; 3], probe: &mut impl FnMut([i32; 3]) -> u32) {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "block coordinates; the eye is always within i32 range"
        )]
        let centre = [
            eye[0].floor() as i32,
            eye[1].floor() as i32,
            eye[2].floor() as i32,
        ];
        for _ in 0..AMBIENT_SAMPLES {
            let span = AMBIENT_RANGE * 2 + 1;
            let rng = self.engine.rng();
            let offset = [
                rng.next_int_bound(span) - AMBIENT_RANGE,
                rng.next_int_bound(span) - AMBIENT_RANGE,
                rng.next_int_bound(span) - AMBIENT_RANGE,
            ];
            let block = [
                centre[0] + offset[0],
                centre[1] + offset[1],
                centre[2] + offset[2],
            ];
            let state = probe(block);
            if state == 0 {
                continue;
            }
            self.animate_block(block, state);
        }
    }

    /// One block's `animateTick`, for the handful of blocks a survival player
    /// actually notices. Silent for everything else.
    fn animate_block(&mut self, block: [i32; 3], state: u32) {
        let Some(name) = lodestone_data::block_states::block_name(state) else {
            return;
        };
        let props = lodestone_data::block_states::properties(state).unwrap_or(&[]);
        let prop = |key: &str| props.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
        let [bx, by, bz] = [
            f64::from(block[0]),
            f64::from(block[1]),
            f64::from(block[2]),
        ];
        match name.strip_prefix("minecraft:").unwrap_or(name) {
            // `TorchBlock.animateTick`: one flame and one smoke at the flame's
            // own position, which for a wall torch is offset *away* from the wall
            // it hangs on. Using the block centre for both puts the flame inside
            // the wall.
            "torch" | "soul_torch" | "wall_torch" | "soul_wall_torch" => {
                let (dx, dz, dy) = match prop("facing") {
                    Some("north") => (0.0, 0.27, 0.22),
                    Some("south") => (0.0, -0.27, 0.22),
                    Some("west") => (0.27, 0.0, 0.22),
                    Some("east") => (-0.27, 0.0, 0.22),
                    // A standing torch: centred, flame at the tip.
                    _ => (0.0, 0.0, 0.0),
                };
                let (x, y, z) = (bx + 0.5 + dx, by + 0.7 + dy, bz + 0.5 + dz);
                emit::smoke(&mut self.engine, x, y, z, 0.0, 0.0, 0.0, 1.0);
                if name.contains("soul") {
                    emit::soul_fire_flame(&mut self.engine, x, y, z, 0.0, 0.0, 0.0);
                } else {
                    emit::flame(&mut self.engine, x, y, z, 0.0, 0.0, 0.0);
                }
            }
            // `NetherPortalBlock.animateTick`: four motes per tick at random
            // points inside the block, drifting on a signed offset — which for
            // `PortalParticle` is the *amplitude* it converges from, not a speed.
            "nether_portal" | "end_gateway" => {
                for _ in 0..4 {
                    let rng = self.engine.rng();
                    let (rx, ry, rz) = (
                        f64::from(rng.next_float()),
                        f64::from(rng.next_float()),
                        f64::from(rng.next_float()),
                    );
                    let sign = |r: &mut lodestone_particle::rng::JavaRandom| {
                        if r.next_bool() { 1.0 } else { -1.0 }
                    };
                    let rng = self.engine.rng();
                    let (sx, sz) = (sign(rng), sign(rng));
                    emit::portal(
                        &mut self.engine,
                        bx + rx,
                        by + ry,
                        bz + rz,
                        sx * 0.25,
                        (ry - 0.5) * 0.25,
                        sz * 0.25,
                    );
                }
            }
            // `EndRodBlock.animateTick`: one sparkle just off the rod's tip,
            // along whatever axis it points.
            "end_rod" => {
                let (dx, dy, dz) = match prop("facing") {
                    Some("up") => (0.0, 0.4, 0.0),
                    Some("down") => (0.0, -0.4, 0.0),
                    Some("north") => (0.0, 0.0, -0.4),
                    Some("south") => (0.0, 0.0, 0.4),
                    Some("west") => (-0.4, 0.0, 0.0),
                    _ => (0.4, 0.0, 0.0),
                };
                emit::end_rod(
                    &mut self.engine,
                    bx + 0.5 + dx,
                    by + 0.5 + dy,
                    bz + 0.5 + dz,
                    0.0,
                    0.0,
                    0.0,
                );
            }
            // `CampfireBlock.animateTick`, gated on `lit` — an unlit campfire
            // must be silent, and the property is the only thing distinguishing
            // the two states.
            "campfire" | "soul_campfire" => {
                if prop("lit") != Some("true") {
                    return;
                }
                let signal = prop("signal_fire") == Some("true");
                let rng = self.engine.rng();
                let (rx, rz) = (
                    f64::from(rng.next_float()) * 0.2 - 0.1,
                    f64::from(rng.next_float()) * 0.2 - 0.1,
                );
                emit::campfire_smoke(
                    &mut self.engine,
                    bx + 0.5 + rx,
                    by + 1.0,
                    bz + 0.5 + rz,
                    0.0,
                    0.07,
                    0.0,
                    signal,
                );
            }
            _ => {}
        }
    }
}
