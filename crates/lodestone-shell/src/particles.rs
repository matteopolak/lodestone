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
//! into UVs needs the baked model set — vanilla's own baked-model particle-icon accessor,
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
use lodestone_data::item::Item;
use lodestone_model::event::{BlockStateRef, ParticleOptions};
use lodestone_particle::{
    DripKind, DripPhase, Layer, ParticleEngine, ParticleQuad, Sheet, SpriteSource, emit,
};
use lodestone_physics::{CollisionView, Vec3d};
use lodestone_render::{BlockModels, Camera};
use wgpu::util::DeviceExt;

/// The untinted particle colour.
///
/// Two unrelated uses, and only one of them is a real vanilla value.
/// `infested`, `raid_omen` and `trial_omen` are registered against
/// vanilla's own spell-particle provider, which takes a bare `SimpleParticleType` and never
/// calls `setColor` at all — white *is* their colour, and their sprites carry
/// the tint. `effect`, `entity_effect` and `instant_effect` reach it only on
/// the fallback arms, where a connection's protocol family gave this client no
/// `ParticleOptions` payload to read a tint out of; those arms log before
/// drawing, because an untinted potion mote looks like a working particle.
const WHITE: [f32; 3] = [1.0, 1.0, 1.0];

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
    /// Live particles belonging specifically to the cosy/signal campfire plume.
    ///
    /// This is an anti-vacuity diagnostic for screenshot and live gates: a
    /// generic `alive > 0` can be satisfied by an unrelated ambient block or a
    /// server packet while the campfire block-entity lifecycle is still dead.
    pub campfire_smoke_alive: usize,
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
    /// This exists because vanilla's own terrain-particle type does not multiply its
    /// `0.6` grey by white — it multiplies by the block's own tint source, resolved
    /// through the same per-block-state colour lookup its foliage/water tinting uses. The
    /// blocks that have such a source are exactly the ones whose sprites are
    /// **greyscale in the atlas** (`grass`, `fern`, the leaves, `sugar_cane`,
    /// `redstone_dust_*`), so dropping the tint does not merely desaturate their
    /// debris — it renders it near-**white**. See `docs/break-particles.md`.
    state_tint: Arc<Vec<[f32; 3]>>,
    /// Per-built-in-item atlas UV rect. `SpriteSource::Item` keeps the typed
    /// identity until [`Self::sprite_rect`] lowers it for this indexed lookup.
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

/// Lowers a source-tagged state only where the built-in particle tables need a
/// generated-state index. A protocol-local value can overlap this build's
/// census, but its numeric range is not permission to render it as 26.2.
fn built_in_state_for_particles(
    state: BlockStateRef,
    effect: &str,
) -> Option<lodestone_data::block_states::StateId> {
    let BlockStateRef::Canonical(raw) = state else {
        tracing::debug!(
            target: "particles",
            raw = state.raw(),
            "protocol-local or custom block state for {effect}; not rendered by the built-in resolver"
        );
        return None;
    };
    let Some(state) = lodestone_data::block_states::StateId::new(raw) else {
        tracing::debug!(
            target: "particles",
            raw,
            "out-of-census canonical block state for {effect}; dropped"
        );
        return None;
    };
    Some(state)
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

    /// Emit vanilla's block-destruction burst — vanilla's own client-level add-destroy-block-effect.
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
    pub fn destroy_block(&mut self, block: [i32; 3], state: BlockStateRef, tint: [f32; 3]) {
        let Some(state) = built_in_state_for_particles(state, "destroy debris") else {
            return;
        };
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
    /// face — vanilla's own client-level add-breaking-block-effect.
    ///
    /// `tint` is an extra multiplier on top of the state's own particle tint,
    /// exactly as in [`destroy_block`](Self::destroy_block): the two emitters
    /// both construct vanilla's own terrain particle, so they must tint identically or a
    /// block's mining flecks and its final burst come out different colours.
    pub fn breaking_block(
        &mut self,
        block: [i32; 3],
        state: BlockStateRef,
        tint: [f32; 3],
        face: emit::Face,
    ) {
        let Some(state) = built_in_state_for_particles(state, "mining debris") else {
            return;
        };
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

    /// `extra` multiplied by `state`'s own particle tint — the same
    /// tint-source-multiply step of
    /// vanilla's own terrain-particle constructor.
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
    /// `StateId` has already made the state-census range invariant true at the
    /// ingress. A partial tint table is still legitimate (the demo palette is
    /// empty), so its miss leaves the caller's multiplier alone.
    fn state_tint_of(
        &self,
        state: lodestone_data::block_states::StateId,
        extra: [f32; 3],
    ) -> [f32; 3] {
        let Some(t) = self.state_tint.get(state.raw() as usize) else {
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

    /// Vanilla's own client-side particle-event handling — the general
    /// `LEVEL_PARTICLES` packet path, as opposed to the `LevelEvent` 2001
    /// shortcut [`Self::destroy_block`] covers. Spawns `count` particles of
    /// `kind` (the particle type's namespace-stripped path, e.g. `"flame"`)
    /// at `pos`.
    ///
    /// # `count == 0` is not "spawn nothing"
    ///
    /// Confirmed against the 26.2 client sources, vanilla's own
    /// particle-event packet handler:
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
    /// vanilla's own add-particle per-type dispatch, narrowed to the sheet
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
            // Vanilla's own crit-particle family. All three share one constructor and
            // differ only in sheet plus a provider-level tweak; see
            // `emit::crit_particle`'s callers. `enchanted_hit` is the one this
            // client's own sprite table already knew about and nothing emitted
            // — `Sheet::EnchantedHit` has been stitched into the particle atlas
            // and unreachable since the sheet enum was written.
            "enchanted_hit" => emit::enchanted_hit(&mut self.engine, x, y, z, xa, ya, za),
            "damage_indicator" => emit::damage_indicator(&mut self.engine, x, y, z, xa, ya, za),
            // Vanilla's own spell-particle family, over the four sheets vanilla's own
            // `particles/*.json` assign it. `witch` (below) is the fifth
            // member; it draws its tint from the RNG rather than from a
            // provider constant, so it keeps its own emitter.
            //
            // `effect`/`instant_effect` carry vanilla's own spell-particle option (an RGB
            // word plus a velocity multiplier) and `entity_effect` a
            // `ColorParticleOption` (an ARGB word). Those payloads are the
            // *whole* of a potion particle's colour — the class has no palette
            // of its own — so a missing one draws a white mote, which looks
            // like a working particle and is why this went unnoticed. `v770`
            // decodes all three; the legacy families do not carry the payload
            // in this shape at all (1.12's `WORLD_PARTICLES` puts a mob-spell
            // tint in the offset words instead), so the fallback arms below
            // keep drawing white and **say so** rather than dropping the
            // particle, which would be a visible regression on those servers.
            "effect" => match options {
                ParticleOptions::Spell { color, power } => {
                    emit::spell_instant(
                        &mut self.engine,
                        x,
                        y,
                        z,
                        xa,
                        ya,
                        za,
                        Sheet::Effect,
                        color,
                        power,
                    );
                }
                _ => {
                    tracing::debug!(
                        target: "particles",
                        "effect particle with no spell-particle-option payload; \
                         drawing an untinted white mote"
                    );
                    emit::spell(&mut self.engine, x, y, z, xa, ya, za, Sheet::Effect, WHITE);
                }
            },
            "instant_effect" => match options {
                ParticleOptions::Spell { color, power } => {
                    emit::spell_instant(
                        &mut self.engine,
                        x,
                        y,
                        z,
                        xa,
                        ya,
                        za,
                        Sheet::Spell,
                        color,
                        power,
                    );
                }
                _ => {
                    tracing::debug!(
                        target: "particles",
                        "instant_effect particle with no spell-particle-option payload; \
                         drawing an untinted white mote"
                    );
                    emit::spell(&mut self.engine, x, y, z, xa, ya, za, Sheet::Spell, WHITE);
                }
            },
            "entity_effect" => match options {
                ParticleOptions::Color { color } => {
                    emit::spell_mob_effect(
                        &mut self.engine,
                        x,
                        y,
                        z,
                        xa,
                        ya,
                        za,
                        Sheet::Effect,
                        color,
                    );
                }
                _ => {
                    tracing::debug!(
                        target: "particles",
                        "entity_effect particle with no ColorParticleOption payload; \
                         drawing an untinted white mote"
                    );
                    emit::spell(&mut self.engine, x, y, z, xa, ya, za, Sheet::Effect, WHITE);
                }
            },
            "infested" => {
                emit::spell(&mut self.engine, x, y, z, xa, ya, za, Sheet::Infested, WHITE);
            }
            "raid_omen" => {
                emit::spell(&mut self.engine, x, y, z, xa, ya, za, Sheet::RaidOmen, WHITE);
            }
            "trial_omen" => {
                emit::spell(&mut self.engine, x, y, z, xa, ya, za, Sheet::TrialOmen, WHITE);
            }
            // Vanilla's own fly-towards-position particle's two argument-identical providers.
            // The wire's three velocity words are an **offset** for these, not a
            // velocity — see `emit::fly_towards_position`.
            "enchant" => {
                emit::fly_towards_position(&mut self.engine, x, y, z, xa, ya, za, Sheet::Enchant);
            }
            "nautilus" => {
                emit::fly_towards_position(&mut self.engine, x, y, z, xa, ya, za, Sheet::Nautilus);
            }
            "note" => emit::note(&mut self.engine, x, y, z, xa),
            "heart" => emit::heart(&mut self.engine, x, y, z),
            "angry_villager" => emit::angry_villager(&mut self.engine, x, y, z),
            "happy_villager" => emit::happy_villager(&mut self.engine, x, y, z, xa, ya, za),
            "witch" => emit::witch(&mut self.engine, x, y, z, xa, ya, za),
            "totem_of_undying" => emit::totem_of_undying(&mut self.engine, x, y, z, xa, ya, za),
            // `minecraft:explosion_emitter`/`minecraft:explosion`.
            // Correction the doc for these two carried until this pass: they
            // are **not** blocked on the shared `ParticleOptions` decoder
            // (`docs/particle-catalogue.md`'s "explosion_emitter"/"explosion"
            // section) — both are argument-less particle types with no encoded
            // fields, and
            // `decode_explode` already recognises their registry ids. What
            // was missing was exactly this arm plus the `Sheet`/`Behaviour`
            // pair in `lodestone_particle`, not a decoder.
            //
            // `explosion_emitter` (the seed vanilla's own explosion packet
            // actually names) ignores every
            // positional argument here: vanilla's own explosion-seed particle
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
            // because vanilla spawns them from its own per-block animate-tick rather than
            // over the network; a type can legitimately have both.
            "soul" => emit::soul(&mut self.engine, x, y, z, xa, ya, za),
            "soul_fire_flame" => emit::soul_fire_flame(&mut self.engine, x, y, z, xa, ya, za),
            // `reverse_portal` shares vanilla's own portal-particle implementation and differs only in the
            // sign the *caller* gives the offset, which the wire already carries.
            "portal" | "reverse_portal" => emit::portal(&mut self.engine, x, y, z, xa, ya, za),
            "campfire_cosy_smoke" => {
                emit::campfire_smoke(&mut self.engine, x, y, z, xa, ya, za, false);
            }
            "campfire_signal_smoke" => {
                emit::campfire_smoke(&mut self.engine, x, y, z, xa, ya, za, true);
            }
            "end_rod" => emit::end_rod(&mut self.engine, x, y, z, xa, ya, za),
            // Vanilla's own glow-particle family, all five over `particle/glow`. These
            // two shared one approximation of its own firework spark particle
            // until this pass — a plausible-looking spark with the wrong
            // friction, the wrong lifetime, no tint and collision left on, and
            // `glow`'s own provider (a glow squid's two-population shimmer)
            // collapsed into `electric_spark`'s.
            "electric_spark" => emit::electric_spark(&mut self.engine, x, y, z, xa, ya, za),
            "glow" => emit::glow_squid(&mut self.engine, x, y, z, xa, ya, za),
            "scrape" => emit::scrape(&mut self.engine, x, y, z, xa, ya, za),
            "wax_on" => emit::wax_on(&mut self.engine, x, y, z, xa, ya, za),
            "wax_off" => emit::wax_off(&mut self.engine, x, y, z, xa, ya, za),
            // Vanilla's own flame-particle provider's other two registry types, and
            // its own small-flame provider. Each names its own sheet; the shared
            // provider decides nothing.
            "copper_fire_flame" => {
                emit::copper_fire_flame(&mut self.engine, x, y, z, xa, ya, za);
            }
            "small_flame" => emit::small_flame(&mut self.engine, x, y, z, xa, ya, za),
            "sculk_soul" => emit::sculk_soul(&mut self.engine, x, y, z, xa, ya, za),
            "sculk_charge_pop" => emit::sculk_charge_pop(&mut self.engine, x, y, z, xa, ya, za),
            // Vanilla's own player-cloud-particle's two providers. An area-effect cloud's
            // puff and a panda's sneeze.
            "cloud" => emit::cloud(&mut self.engine, x, y, z, xa, ya, za),
            "sneeze" => emit::sneeze(&mut self.engine, x, y, z, xa, ya, za),
            // Vanilla's own lava-particle reads none of the three velocity words: its
            // constructor damps them to 0.8 and then overwrites `yd` outright,
            // so every pop launches upward whatever the packet said. It is also
            // the only particle here that spawns a *different* type as it
            // lives — see `Behaviour::Lava`'s trailing-smoke roll.
            "lava" => emit::lava(&mut self.engine, x, y, z),
            "squid_ink" => emit::squid_ink(&mut self.engine, x, y, z, xa, ya, za),
            "glow_squid_ink" => emit::glow_squid_ink(&mut self.engine, x, y, z, xa, ya, za),
            // Vanilla's own firework spark particle via its own spark provider -- the plain
            // wire particle a `LEVEL_PARTICLES` packet can name directly, not the
            // rocket-explosion burst its own starter/no-render particle spawns
            // client-side (never sent over the wire at all). See
            // `docs/particle-catalogue.md`'s "Correction" entry for why this was
            // never blocked on the `ParticleOptions` decoder the way it first
            // looked: vanilla's own firework particle type is a `SimpleParticleType`.
            "firework" => emit::firework(&mut self.engine, x, y, z, xa, ya, za),
            // Vanilla's own dragon-breath particle — a dragon's breath attack and, far more
            // commonly, every lingering potion cloud. Its `PowerParticleOption`
            // is a velocity multiplier and nothing else; the purple is drawn
            // per particle inside the emitter, so a missing payload costs
            // motion rather than colour and the fallback is power 1.0
            // (`PowerParticleOption`'s own data-codec default).
            "dragon_breath" => match options {
                ParticleOptions::Power { power } => {
                    emit::dragon_breath(&mut self.engine, x, y, z, xa, ya, za, power);
                }
                _ => {
                    tracing::debug!(
                        target: "particles",
                        "dragon_breath particle with no PowerParticleOption payload; \
                         drawing at unit power"
                    );
                    emit::dragon_breath(&mut self.engine, x, y, z, xa, ya, za, 1.0);
                }
            },
            // `SculkChargeParticle` has its own emitter rather than sharing
            // `animated_ambient` with the three below: its roll is a wire
            // field, its lifetime a per-particle draw, and its provider
            // installs the packet's velocity words verbatim.
            "sculk_charge" => match options {
                ParticleOptions::SculkCharge { roll } => {
                    emit::sculk_charge(&mut self.engine, x, y, z, xa, ya, za, roll);
                }
                _ => {
                    tracing::debug!(
                        target: "particles",
                        "sculk_charge particle with no SculkChargeParticleOptions payload; \
                         drawing at zero roll"
                    );
                    emit::sculk_charge(&mut self.engine, x, y, z, xa, ya, za, 0.0);
                }
            },
            // Sheet, scale and lifetime are what separate these three; the tick
            // shape is identical. Lifetimes are each class's own constructor.
            "gust" => {
                emit::animated_ambient(&mut self.engine, x, y, z, 0.0, 0.0, 0.0, Sheet::Gust, 3.0, 12)
            }
            // `Sheet::SmallGust`, not `Sheet::Gust`: vanilla's own small-gust
            // provider
            // shares the class but `small_gust.json` names `small_gust_0`…`_6`,
            // seven frames of its own. Pointed at `Gust` this sampled the wrong
            // texture and indexed a twelve-frame sequence it does not have.
            "small_gust" => emit::animated_ambient(
                &mut self.engine, x, y, z, 0.0, 0.0, 0.0, Sheet::SmallGust, 1.0, 12,
            ),
            "sonic_boom" => emit::animated_ambient(
                &mut self.engine, x, y, z, 0.0, 0.0, 0.0, Sheet::SonicBoom, 3.0, 16,
            ),
            // The drip family: seventeen registry types over one class, one
            // `(kind, phase)` table in `emit::drip`, and a chain that continues
            // **inside** the particle's own tick — a hanging drip spawns the
            // falling one when it lets go, and that spawns the splash or the
            // landing phase where it hits.
            //
            // Only the five below existed before, all as unchained one-shots
            // with a hardcoded 64-tick lifetime, so a cave ceiling grew drips
            // that hung for the wrong length of time and then blinked out
            // without ever falling.
            //
            // These take no velocity from the packet by design: vanilla's
            // providers all use vanilla's own drip-particle's zero-velocity constructor, and
            // the only velocity a drip ever has is the one its hanging phase
            // hands to its falling phase.
            "dripping_water" => self.drip(DripKind::Water, DripPhase::Hang, pos),
            "falling_water" => self.drip(DripKind::Water, DripPhase::Fall, pos),
            "dripping_lava" => self.drip(DripKind::Lava, DripPhase::Hang, pos),
            "falling_lava" => self.drip(DripKind::Lava, DripPhase::Fall, pos),
            "landing_lava" => self.drip(DripKind::Lava, DripPhase::Land, pos),
            "dripping_honey" => self.drip(DripKind::Honey, DripPhase::Hang, pos),
            "falling_honey" => self.drip(DripKind::Honey, DripPhase::Fall, pos),
            "landing_honey" => self.drip(DripKind::Honey, DripPhase::Land, pos),
            "falling_nectar" => self.drip(DripKind::Nectar, DripPhase::Fall, pos),
            "dripping_obsidian_tear" => self.drip(DripKind::ObsidianTear, DripPhase::Hang, pos),
            "falling_obsidian_tear" => self.drip(DripKind::ObsidianTear, DripPhase::Fall, pos),
            "landing_obsidian_tear" => self.drip(DripKind::ObsidianTear, DripPhase::Land, pos),
            "dripping_dripstone_water" => {
                self.drip(DripKind::DripstoneWater, DripPhase::Hang, pos);
            }
            "falling_dripstone_water" => {
                self.drip(DripKind::DripstoneWater, DripPhase::Fall, pos);
            }
            "dripping_dripstone_lava" => self.drip(DripKind::DripstoneLava, DripPhase::Hang, pos),
            "falling_dripstone_lava" => self.drip(DripKind::DripstoneLava, DripPhase::Fall, pos),
            "falling_spore_blossom" => self.drip(DripKind::SporeBlossom, DripPhase::Fall, pos),
            // `spore_blossom_air` used to sit in this drip block, and it is
            // not a drip particle at all — vanilla's own suspended-particle.
            // SporeBlossomAirProvider`, which shares `drip_fall`'s *texture*
            // with `falling_spore_blossom` and nothing else. It hangs rather
            // than falling, and its lifetime is a flat 500..=1000 ticks against
            // the drip's own draw, so as a drip it vanished far too fast.
            "spore_blossom_air" => emit::spore_blossom_air(&mut self.engine, x, y, z),

            // -- Vanilla's own suspended-particle biome drift ----------------
            //
            // Four types over one class. Each supplies its own velocity inside
            // the emitter (vanilla's providers draw it, rather than taking it
            // from the packet), so the wire's three velocity words are
            // deliberately unused here — that is the class's shape, not a
            // dropped field.
            "underwater" => emit::underwater(&mut self.engine, x, y, z),
            "crimson_spore" => emit::crimson_spore(&mut self.engine, x, y, z),
            "warped_spore" => emit::warped_spore(&mut self.engine, x, y, z),

            // -- The `SuspendedTownParticle` ambient specks ----------------
            "mycelium" => emit::mycelium(&mut self.engine, x, y, z, xa, ya, za),
            "composter" => emit::composter(&mut self.engine, x, y, z, xa, ya, za),
            "egg_crack" => emit::egg_crack(&mut self.engine, x, y, z, xa, ya, za),
            "dolphin" => emit::dolphin(&mut self.engine, x, y, z, xa, ya, za),

            // -- `BaseAshSmokeParticle`'s other three subclasses ----------------
            //
            // `ash` and `white_ash` take no velocity from the packet either;
            // `white_ash` draws its own and `ash` has none.
            "ash" => emit::ash(&mut self.engine, x, y, z),
            "white_ash" => emit::white_ash(&mut self.engine, x, y, z),
            "white_smoke" => emit::white_smoke(&mut self.engine, x, y, z, xa, ya, za),

            // -- `ExplodeParticle` ----------------
            //
            // `poof` is the mob-death, breeding and spawner puff — among the
            // most frequently spawned particles in the game, and until this arm
            // existed every one of them hit the catch-all below.
            "poof" => emit::poof(&mut self.engine, x, y, z, xa, ya, za),
            "spit" => emit::spit(&mut self.engine, x, y, z, xa, ya, za),
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

            // -- The `BlockParticleOption` family ------------------
            //
            // One wire payload, five providers. The payload is shared and the
            // *behaviour* is not: three build vanilla's own terrain particle (differing in
            // speed and lifetime), one a physics-free marker quad, one a
            // sheet-textured mote wearing the block's colour rather than its
            // texture. Every arm goes through `block_state_payload`, which is
            // where the `isAir`/`moving_piston` refusal lives — vanilla's
            // vanilla's own create-terrain-particle returns `null` for those and a fragment of
            // air is a fragment of nothing.
            "block" => {
                if let Some(state) = self.block_state_payload(kind, options) {
                    let tint = self.state_tint_of(state, [1.0; 3]);
                    emit::block_fragment(&mut self.engine, pos, vel, state, tint);
                }
            }
            "block_crumble" => {
                if let Some(state) = self.block_state_payload(kind, options) {
                    let tint = self.state_tint_of(state, [1.0; 3]);
                    emit::block_crumble(&mut self.engine, pos, vel, state, tint);
                }
            }
            "dust_pillar" => {
                if let Some(state) = self.block_state_payload(kind, options) {
                    let tint = self.state_tint_of(state, [1.0; 3]);
                    emit::dust_pillar(&mut self.engine, pos, vel, state, tint);
                }
            }
            // No tint: vanilla's own block-marker constructor never touches `rCol`, so a
            // marker over grass is the grass sprite at full brightness, not the
            // `0.6`-grey-times-biome-tint a fragment of it would be.
            "block_marker" => {
                if let Some(state) = self.block_state_payload(kind, options) {
                    emit::block_marker(&mut self.engine, pos, state);
                }
            }
            // The tint is the *whole* identity here — the sprite is a generic
            // grey mote — so this one reads `state_tint_of` for a purpose the
            // other four only decorate with.
            "falling_dust" => {
                if let Some(state) = self.block_state_payload(kind, options) {
                    let tint = self.state_tint_of(state, [1.0; 3]);
                    emit::falling_dust(&mut self.engine, pos, tint);
                }
            }

            // -- The water column and weather family ---------------
            //
            // `rain` is the splash a raindrop makes where it *lands*, not the
            // falling streaks: those are `lodestone_render::weather`'s textured
            // columns, which never become particles at all. Wiring `rain` here
            // does not duplicate them.
            "rain" => emit::rain(&mut self.engine, x, y, z),
            "snowflake" => emit::snowflake(&mut self.engine, x, y, z, xa, ya, za),
            "bubble_column_up" => {
                emit::bubble_column_up(&mut self.engine, x, y, z, xa, ya, za);
            }
            // `WaterCurrentDownParticle`'s provider ignores the packet's
            // velocity entirely — the sink speed is a constant and the drift is
            // the spiral. Passing the wire's words would give every magma
            // column an initial kick vanilla does not have.
            "current_down" => emit::current_down(&mut self.engine, x, y, z),
            "bubble_pop" => emit::bubble_pop(&mut self.engine, x, y, z, xa, ya, za),
            // The bobber's ring. Its producer is the fishing bobber entity,
            // which already draws; this is the water it disturbs.
            "fishing" => emit::fishing(&mut self.engine, x, y, z, xa, ya, za),
            "dust_plume" => emit::dust_plume(&mut self.engine, x, y, z, xa, ya, za),

            // -- `FallingLeavesParticle` ---------------------------
            //
            // One class, three registry types, and the providers differ in five
            // constants at once — see `emit::LeafParams`, which carries them as
            // a set so a transposed pair cannot hide. The two untinted variants
            // take no colour; `tinted_leaves` carries a `ColorParticleOption`.
            "cherry_leaves" => {
                emit::falling_leaves(&mut self.engine, x, y, z, emit::LeafParams::cherry(), None);
            }
            "pale_oak_leaves" => {
                emit::falling_leaves(&mut self.engine, x, y, z, emit::LeafParams::pale_oak(), None);
            }
            "tinted_leaves" => match options {
                ParticleOptions::Color { color } => {
                    emit::falling_leaves(
                        &mut self.engine,
                        x,
                        y,
                        z,
                        emit::LeafParams::tinted(),
                        Some([color[0], color[1], color[2]]),
                    );
                }
                _ => tracing::debug!(
                    target: "particles",
                    "tinted_leaves particle with no ColorParticleOption payload; dropped"
                ),
            },

            "firefly" => emit::firefly(&mut self.engine, x, y, z, ya),
            // Vanilla's own firework flash provider reads all four ARGB components:
            // the alpha byte is a real field here, not padding, and dropping it
            // makes every firework flash fully opaque.
            "flash" => match options {
                ParticleOptions::Color { color } => emit::flash(&mut self.engine, x, y, z, color),
                _ => tracing::debug!(
                    target: "particles",
                    "flash particle with no ColorParticleOption payload; dropped"
                ),
            },

            // -- `BreakingItemParticle`'s hardcoded-item providers --
            //
            // Three `SimpleParticleType`s with **no wire payload**: each
            // provider names its own item and calls the four-argument
            // constructor. The item ids are resolved here rather than baked
            // into `lodestone-particle`, which knows nothing about the item
            // registry, and a missing id drops the particle rather than
            // drawing a wrong sprite.
            "item_slime" => self.item_burst(pos, "minecraft:slime_ball"),
            "item_cobweb" => self.item_burst(pos, "minecraft:cobweb"),
            "item_snowball" => self.item_burst(pos, "minecraft:snowball"),

            // The geyser eruption seed. Its wire payload's own water-blocks
            // field this client's own decoder does not yet surface (there is
            // no `ParticleOptions` variant for it), so every geyser draws at
            // the minimum valid payload value (`1`) rather than dropping the
            // particle — the same "log and use a harmless default" shape the
            // `dragon_breath`/`sculk_charge` arms above use for their own
            // missing payloads. `geyser_base`/`geyser_poof`/`geyser_plume`
            // are vanilla's own eruption particle's own three children, drawn
            // through the same emitters here so a direct `/particle` of one
            // of those three (never how vanilla itself spawns them) still
            // draws something.
            "geyser" => {
                tracing::debug!(
                    target: "particles",
                    "geyser particle with no decoded water-blocks payload; drawing at \
                     water_blocks = 1"
                );
                emit::geyser(&mut self.engine, x, y, z, xa, ya, za, 1);
            }
            "geyser_base" => {
                emit::geyser_base_or_poof(&mut self.engine, x, y, z, xa, ya, za, 1, 1.5, Sheet::GeyserBase);
            }
            "geyser_poof" => {
                emit::geyser_base_or_poof(&mut self.engine, x, y, z, xa, ya, za, 1, 2.0, Sheet::GeyserPoof);
            }
            "geyser_plume" => emit::geyser_plume(&mut self.engine, x, y, z, xa, ya, za, 1),

            // The potent-sulfur block's noxious-gas family: the puff itself,
            // fixed scale 3.0, and the non-rendering seed that throws puffs
            // around itself every two ticks for its own 20-tick life.
            "noxious_gas" => emit::noxious_gas(&mut self.engine, x, y, z, xa, ya, za),
            "noxious_gas_cloud" => emit::noxious_gas_cloud(&mut self.engine, x, y, z),
            // The potent-sulfur spring's rising bubble and the debris a
            // broken sulfur cube throws. The bubble reads no `ya`: vanilla's
            // own provider drops it too.
            "sulfur_bubbles" => emit::sulfur_bubbles(&mut self.engine, x, y, z, xa, za),
            "sulfur_cube_goo" => emit::sulfur_cube_goo(&mut self.engine, x, y, z),
            // The trial spawner's and the vault's own detection runes — one
            // class, two sheets, no wire payload.
            "trial_spawner_detection" => emit::trial_spawner_detection(
                &mut self.engine, x, y, z, xa, ya, za, Sheet::TrialSpawnerDetection,
            ),
            "trial_spawner_detection_ominous" => emit::trial_spawner_detection(
                &mut self.engine, x, y, z, xa, ya, za, Sheet::TrialSpawnerDetectionOminous,
            ),
            "vault_connection" => {
                emit::vault_connection(&mut self.engine, x, y, z, xa, ya, za);
            }
            "ominous_spawning" => {
                emit::ominous_spawning(&mut self.engine, x, y, z, xa, ya, za);
            }
            // The wind-charge/gust-emitter seeds — vanilla's own two provider
            // registrations' own constants, never wire-driven.
            "gust_emitter_large" => emit::gust_emitter(&mut self.engine, x, y, z, 3.0, 7, 0),
            "gust_emitter_small" => emit::gust_emitter(&mut self.engine, x, y, z, 1.0, 3, 2),
            "pause_mob_growth" => {
                emit::simple_vertical(&mut self.engine, x, y, z, xa, ya, za, false);
            }
            "reset_mob_growth" => {
                emit::simple_vertical(&mut self.engine, x, y, z, xa, ya, za, true);
            }
            // The sculk shrieker's shockwave. Its wire payload carries a
            // delay this client's own decoder does not yet surface (there is
            // no `ParticleOptions` variant for it), so every shriek draws
            // with delay `0` (immediate) rather than dropping the particle.
            "shriek" => emit::shriek(&mut self.engine, x, y, z, 0),

            other => tracing::debug!(
                target: "particles",
                "no emitter wired for particle type {other:?}; dropped"
            ),
        }
    }

    /// The block state a `BlockParticleOption` particle should wear, or `None`
    /// if this one must not spawn at all.
    ///
    /// Two refusals, and they are different in kind. A missing payload is a
    /// *caller* fault — production cannot produce one, since the adapter decodes
    /// the state alongside the type and hands both through together, so this can
    /// only be reached from a test or a future non-network producer, and it is
    /// logged rather than asserted for the same reason every other payload arm
    /// here logs.
    ///
    /// The second is vanilla's own: its own create-terrain-particle returns `null` for
    /// air and for `moving_piston`, and vanilla's own falling-dust-particle
    /// provider refuses
    /// an invisible-render-shape state. Air is the one that matters — a
    /// `LevelEvent`-adjacent producer that reads a block *after* it has been
    /// removed sends the air state, and without this test the client spends a
    /// full burst of particles on a sprite that resolves to nothing and lands in
    /// [`ParticleFrame::unresolved`], where it reads as a broken atlas rather
    /// than as a refusal vanilla also makes.
    ///
    /// `moving_piston` is included because it is the second half of the same
    /// vanilla condition and costs one string compare; the invisible-render-shape
    /// clause is **not** ported, because this client has no per-state render-shape
    /// table and the states it would catch (barriers, structure voids, light
    /// blocks) have no particle sprite either, so they are already refused one
    /// layer down.
    fn block_state_payload(
        &self,
        kind: &str,
        options: ParticleOptions,
    ) -> Option<lodestone_data::block_states::StateId> {
        let ParticleOptions::BlockState { state } = options else {
            tracing::debug!(
                target: "particles",
                "{kind} particle with no BlockParticleOption payload; dropped"
            );
            return None;
        };
        let BlockStateRef::Canonical(raw) = state else {
            tracing::debug!(
                target: "particles",
                raw = state.raw(),
                "{kind} particle with a protocol-local or custom \
                 BlockParticleOption state; not rendered by the built-in resolver"
            );
            return None;
        };
        let Some(state) = lodestone_data::block_states::StateId::new(raw) else {
            tracing::debug!(
                target: "particles",
                "{kind} particle with an out-of-census canonical BlockParticleOption state; dropped"
            );
            return None;
        };
        if matches!(
            state.block(),
            lodestone_data::block::Block::Air
                | lodestone_data::block::Block::CaveAir
                | lodestone_data::block::Block::VoidAir
                | lodestone_data::block::Block::MovingPiston
        ) {
            return None;
        }
        Some(state)
    }

    /// One `BreakingItemParticle` from the four-argument constructor, for the
    /// three registry types whose provider hardcodes an item.
    ///
    /// A named helper for the same reason [`Self::drip`] is one: the whole of
    /// what it adds is the registry lookup and the zero velocity, and spelling
    /// those three times is three chances to reach for [`emit::item_particle`]
    /// — the *seven*-argument sibling, which damps the jitter to a tenth and
    /// would leave these crumbs motionless.
    fn item_burst(&mut self, pos: [f64; 3], item: &str) {
        let Some(item) = Item::from_name(item) else {
            tracing::debug!(
                target: "particles",
                "no registry id for {item:?}; item particle dropped"
            );
            return;
        };
        let particle = emit::item_burst_particle(pos[0], pos[1], pos[2], item, self.engine.rng());
        self.engine.add(particle);
    }

    /// One drip of the hang → fall → land chain, at the packet's position.
    ///
    /// A named helper rather than seventeen `emit::drip(&mut self.engine, kind,
    /// phase, pos, [0.0; 3])` calls: the zero velocity is the *whole* of what
    /// this adds, and spelling it seventeen times is seventeen chances to pass
    /// the packet's velocity words instead. Vanilla's providers all use
    /// its own drip-particle's zero-velocity constructor; the only velocity a drip ever
    /// carries is the one its hanging phase hands on when it lets go.
    fn drip(&mut self, kind: DripKind, phase: DripPhase, pos: [f64; 3]) {
        emit::drip(&mut self.engine, kind, phase, pos, [0.0; 3]);
    }

    /// Vanilla's own client-level particle-level calculation folded together with
    /// its own add-particle "particle level not minimal" test — `true` to spawn.
    ///
    /// Transcribed rather than approximated, because the two halves are not
    /// separable: the fold is what makes `DECREASED` a *probability* rather
    /// than a second fixed budget.
    ///
    /// The level starts at the option's own setting; if always-show is set and
    /// the level is minimal, a one-in-ten roll promotes it to decreased; then,
    /// independently, a one-in-three roll on a decreased level demotes it back
    /// to minimal. The particle spawns whenever the resulting level is not
    /// minimal.
    ///
    /// So `All` always spawns, `Decreased` spawns two times in three, and
    /// `Minimal` spawns only via the always-show reprieve — `(1/10) x (2/3)`,
    /// i.e. one time in fifteen.
    ///
    /// `always_show` reaches here from the wire: `LevelParticles::always_show`
    /// on a 26.2 connection, threaded through `ClientEvent::Particles` and
    /// `NetUpdate::Particles` to `net_apply.rs`'s arm. It is `false` on every
    /// legacy family because the field does not exist on their particle
    /// packets, which is the same value vanilla's own particle-spawn overload
    /// passes, not an unported one.
    ///
    /// Note the reprieve is a *probability*, not an exemption: an always-show
    /// particle on `Minimal` still fails fourteen times in fifteen. A gate on
    /// this therefore has to count over many draws or pin the RNG; a single
    /// call proves nothing either way.
    ///
    /// Draws from the particle engine's own `JavaRandom`, which is
    /// `java.util.Random`-compatible, so `next_i32_bound` is vanilla's
    /// `nextInt` exactly. Not the same *stream* as vanilla's own per-level
    /// random source,
    /// which does not matter: nothing observes particle randomness across the
    /// wire.
    pub fn particle_level_permits(
        &mut self,
        level: crate::config::ParticleLevel,
        always_show: bool,
    ) -> bool {
        use crate::config::ParticleLevel;
        let mut level = level;
        if always_show && level == ParticleLevel::Minimal && self.engine.rng().next_i32_bound(10) == 0
        {
            level = ParticleLevel::Decreased;
        }
        if level == ParticleLevel::Decreased && self.engine.rng().next_i32_bound(3) == 0 {
            level = ParticleLevel::Minimal;
        }
        level != ParticleLevel::Minimal
    }

    /// One standard-normal draw (Box-Muller), for the positional/velocity
    /// jitter [`Self::spawn_particles`] needs. See that method's docs for why
    /// this does not need to match `java.util.Random.nextGaussian()`
    /// bit-for-bit.
    fn gaussian(&mut self) -> f64 {
        let rng = self.engine.rng();
        let u1 = rng.next_f64().max(1e-12);
        let u2 = rng.next_f64();
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
        // `TRANSLUCENT_PARTICLE`, both built from
        // the same `PARTICLE_SNIPPET`.
        //
        // One deliberate deviation: vanilla's opaque pipeline has no blending at
        // all, and this one keeps `ALPHA_BLENDING`. `Behaviour::layer()` assigns
        // every `Terrain` particle to `Layer::Opaque` unconditionally, where
        // vanilla's own by-sprite layer selection consults the sprite's own transparency and
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
                    // Strictly nearer wins. Note this is *not* vanilla's
                    // `GREATER_THAN_OR_EQUAL`, which admits an exact tie; the
                    // difference is inert for a billboard, which is never
                    // coplanar with the surface behind it, and the divergence
                    // predates the reversed-Z conversion rather than being
                    // introduced by it.
                    depth_compare: Some(lodestone_render::DEPTH_COMPARE_NEARER),
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
    /// This is vanilla's own opaque-layer submission of the particle group: block-break
    /// debris, crits, flame, bubbles and the rest of [`Layer::Opaque`] go in
    /// here, with depth write on, so the water surface blends over the ones
    /// beneath it and depth-rejects over the ones in front of it. See
    /// [`ParticleRenderer::new`] on the pipelines and `gpu/frame.rs`'s module
    /// doc on the ordering rule.
    ///
    /// Returns the number of instances it actually submitted, which the caller
    /// is expected to total into `RenderStats::particles_drawn` rather than
    /// reading [`count`](Self::count). That is not bookkeeping pedantry: this
    /// pass shipped for two weeks with the opaque half sitting inside a
    /// `if let Some(model)` gate it did not need, so on the packed (demo) path
    /// nothing was ever submitted while the counter — sourced from `count` —
    /// reported the full 64. A submitted-instance total makes a dropped draw
    /// visible in the debug overlay instead of indistinguishable from a healthy
    /// frame.
    pub fn draw_opaque(&self, pass: &mut wgpu::RenderPass<'_>, atlas: &wgpu::BindGroup) -> usize {
        self.draw_range(pass, atlas, &self.opaque_pipeline, 0, self.opaque_count)
    }

    /// Record the **translucent-layer** draw, which runs after translucent
    /// water as vanilla's own after-terrain draw phase does. No-op when the last
    /// [`prepare`](Self::prepare) produced no translucent instances.
    /// Returns the number of instances submitted, for the reason
    /// [`draw_opaque`](Self::draw_opaque) documents.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, atlas: &wgpu::BindGroup) -> usize {
        self.draw_range(pass, atlas, &self.pipeline, self.opaque_count, self.count)
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
    ) -> usize {
        if end <= first {
            return 0;
        }
        let stride = std::mem::size_of::<ParticleInstance>() as u64;
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &self.cam_bind_group, &[]);
        pass.set_bind_group(1, atlas, &[]);
        pass.set_vertex_buffer(0, self.instances.slice(u64::from(first) * stride..));
        pass.draw(0..4, 0..(end - first));
        (end - first) as usize
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

    /// Installs a `(Sheet, frame) -> UV` table covering **every frame of every
    /// sheet**, mirroring `sheet_particle_resolves_with_an_atlas`'s
    /// single-sheet fixture but wide enough to resolve any type
    /// `spawn_particles` dispatches.
    ///
    /// Built from `Sheet::all()` rather than listed by hand. The hand-written
    /// version this replaces named thirteen `(sheet, frame)` pairs, so a new
    /// dispatch arm over a sheet nobody remembered to add read as an
    /// *unresolved* particle — a fixture gap presenting as a renderer bug, in a
    /// test whose subject is the dispatch and not the atlas. What the real
    /// atlas contains is `sheet_uv_table`'s business, and
    /// `every_sheet_frame_stitches_into_the_particle_atlas` in
    /// `lodestone-particle` is what asserts a sheet's frames exist at all.
    fn resolvable() -> Particles {
        let mut p = Particles::new(None);
        let rect = [0.0f32, 0.0, 0.0625, 0.0625];
        let mut table = HashMap::new();
        for &sheet in Sheet::all() {
            for frame in 0..sheet.frame_count() {
                table.insert((sheet, frame), rect);
            }
        }
        p.sheet_uv = Arc::new(table);
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
            // `firework` (vanilla's own firework spark particle/provider):
            // the dispatch arm this module was missing while `emit::firework`
            // itself already existed -- see `docs/particle-catalogue.md`'s
            // "Correction" entry. `count == 1` here (like every other case in
            // this loop) draws position jitter from `gaussian() * offset` and
            // velocity from `gaussian() * max_speed` with `max_speed == 0.0`,
            // so this proves reachability, not a specific spark velocity.
            ("firework", [0.3, 0.1, -0.2]),
            // Vanilla's own crit-particle and spell-particle families, and the two
            // fly-towards-position particle types.
            ("enchanted_hit", [0.0, 0.0, 0.0]),
            ("damage_indicator", [0.0, 0.0, 0.0]),
            ("effect", [0.0, 0.0, 0.0]),
            ("entity_effect", [0.0, 0.0, 0.0]),
            ("instant_effect", [0.0, 0.0, 0.0]),
            ("infested", [0.0, 0.0, 0.0]),
            ("raid_omen", [0.0, 0.0, 0.0]),
            ("trial_omen", [0.0, 0.0, 0.0]),
            ("enchant", [0.4, 0.7, -0.3]),
            ("nautilus", [0.4, 0.7, -0.3]),
            // The ambient/biome family: vanilla's own suspended particle,
            // `SuspendedTownParticle`, the rest of `BaseAshSmokeParticle`, and
            // `ExplodeParticle`.
            ("spore_blossom_air", [0.0, 0.0, 0.0]),
            ("underwater", [0.0, 0.0, 0.0]),
            ("crimson_spore", [0.0, 0.0, 0.0]),
            ("warped_spore", [0.0, 0.0, 0.0]),
            ("mycelium", [0.0, 0.0, 0.0]),
            ("composter", [0.0, 0.0, 0.0]),
            ("egg_crack", [0.0, 0.0, 0.0]),
            ("dolphin", [0.0, 0.0, 0.0]),
            ("ash", [0.0, 0.0, 0.0]),
            ("white_ash", [0.0, 0.0, 0.0]),
            ("white_smoke", [0.0, 0.0, 0.0]),
            ("poof", [0.0, 0.0, 0.0]),
            ("spit", [0.0, 0.0, 0.0]),
            // Vanilla's own glow-particle family and the flame/soul siblings.
            ("electric_spark", [0.0, 0.0, 0.0]),
            ("glow", [0.0, 0.0, 0.0]),
            ("scrape", [0.0, 0.0, 0.0]),
            ("wax_on", [0.0, 0.0, 0.0]),
            ("wax_off", [0.0, 0.0, 0.0]),
            ("copper_fire_flame", [0.0, 0.0, 0.0]),
            ("small_flame", [0.0, 0.0, 0.0]),
            ("sculk_soul", [0.0, 0.0, 0.0]),
            // The drip family, all seventeen phases.
            ("dripping_water", [0.0, 0.0, 0.0]),
            ("falling_water", [0.0, 0.0, 0.0]),
            ("dripping_lava", [0.0, 0.0, 0.0]),
            ("falling_lava", [0.0, 0.0, 0.0]),
            ("landing_lava", [0.0, 0.0, 0.0]),
            ("dripping_honey", [0.0, 0.0, 0.0]),
            ("falling_honey", [0.0, 0.0, 0.0]),
            ("landing_honey", [0.0, 0.0, 0.0]),
            ("falling_nectar", [0.0, 0.0, 0.0]),
            ("dripping_obsidian_tear", [0.0, 0.0, 0.0]),
            ("falling_obsidian_tear", [0.0, 0.0, 0.0]),
            ("landing_obsidian_tear", [0.0, 0.0, 0.0]),
            ("dripping_dripstone_water", [0.0, 0.0, 0.0]),
            ("falling_dripstone_water", [0.0, 0.0, 0.0]),
            ("dripping_dripstone_lava", [0.0, 0.0, 0.0]),
            ("falling_dripstone_lava", [0.0, 0.0, 0.0]),
            ("falling_spore_blossom", [0.0, 0.0, 0.0]),
            // Vanilla's own player-cloud, lava, squid-ink particles and the
            // sculk-charge burst.
            ("cloud", [0.0, 0.0, 0.0]),
            ("sneeze", [0.0, 0.0, 0.0]),
            ("lava", [0.0, 0.0, 0.0]),
            ("squid_ink", [0.0, 0.0, 0.0]),
            ("glow_squid_ink", [0.0, 0.0, 0.0]),
            ("sculk_charge_pop", [0.0, 0.0, 0.0]),
            // The water-column and weather family. `rain` is the impact
            // splash, not the falling streaks — those are the weather
            // renderer's textured columns and never become particles.
            ("rain", [0.0, 0.0, 0.0]),
            ("snowflake", [0.0, 0.0, 0.0]),
            ("bubble_column_up", [0.0, 0.0, 0.0]),
            ("current_down", [0.0, 0.0, 0.0]),
            ("bubble_pop", [0.0, 0.0, 0.0]),
            ("fishing", [0.0, 0.0, 0.0]),
            ("dust_plume", [0.0, 0.0, 0.0]),
            // `FallingLeavesParticle`'s two payload-free variants. The tinted
            // third needs a `ColorParticleOption` and so cannot ride this
            // loop's blanket `ParticleOptions::None`; it is covered by
            // `no_sheet_is_atlas_resident_and_unreachable_from_the_dispatch`,
            // which supplies payloads, and by
            // `the_leaf_variants_differ_in_every_constant_that_separates_them`.
            ("cherry_leaves", [0.0, 0.0, 0.0]),
            ("pale_oak_leaves", [0.0, 0.0, 0.0]),
            ("firefly", [0.0, 0.0, 0.0]),
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

    /// Every sheet in `Sheet::all()` must be reachable by spawning some real
    /// registry particle type through the production dispatch.
    ///
    /// `Sheet::all()` is what `sheet_uv_table` walks to build the atlas UV
    /// table, so a sheet listed there is **stitched into the particle atlas
    /// whether or not anything can ever emit it**. Three were exactly that —
    /// `Effect`, `Enchant` and `EnchantedHit` were declared, listed, stitched,
    /// and constructed by nothing outside a test — and no existing gate could
    /// see it: the census that found the `enchanted_hit` *type* reads
    /// `spawn_one`'s arms, which is the subject side, and says nothing about a
    /// renderer no subject routes to.
    ///
    /// This is the reverse query. It drives the **whole particle registry**
    /// through the same `spawn_particles` entry point the network path uses and
    /// collects the sheets that come out, so the expectation is not a
    /// transcribed list that can drift. Adding a `Sheet` variant without an
    /// emitter fails here by name.
    #[test]
    fn no_sheet_is_atlas_resident_and_unreachable_from_the_dispatch() {
        let mut reached: std::collections::HashSet<Sheet> = std::collections::HashSet::new();
        for id in 0..lodestone_data::particle_types::PARTICLE_TYPE_COUNT {
            #[expect(
                clippy::cast_possible_wrap,
                reason = "the registry count is far below i32::MAX"
            )]
            let Some(id) = lodestone_data::particle_types::ParticleTypeId::new(id as i32) else {
                continue;
            };
            let name = lodestone_data::particle_types::particle_type_name(id);
            let kind = name.split_once(':').map_or(name, |(_, path)| path);
            let mut p = Particles::new(None);
            // Some types drop outright without their payload, so a bare
            // `None` here would report their sheets as orphans. This table is
            // **hand-maintained and it goes stale silently in one direction
            // only**: a new payload-carrying type left out of it fails this
            // gate by name (which is how `TintedLeaves` and `Flash` were
            // caught the day they landed), never the reverse. Types whose
            // `spawn_one` arm has a payload-less fallback — `effect`,
            // `instant_effect`, `entity_effect` — deliberately stay out of it,
            // since exercising the fallback is the more useful arm.
            let options = match kind {
                "dust" => ParticleOptions::Dust { color: [1.0, 0.0, 0.0], scale: 1.0 },
                "dust_color_transition" => ParticleOptions::DustColorTransition {
                    from_color: [1.0, 0.0, 0.0],
                    to_color: [0.0, 0.0, 1.0],
                    scale: 1.0,
                },
                // `ColorParticleOption`, decoded ARGB. Deliberately not grey
                // and not fully opaque: an arm that transposed a colour
                // component or dropped the alpha would still pass against
                // `[1.0; 4]`.
                "tinted_leaves" | "flash" => {
                    ParticleOptions::Color { color: [0.25, 0.5, 0.75, 0.6] }
                }
                // The `BlockParticleOption` family. `falling_dust` is the only
                // one of the five that reaches a `Sheet` at all, so it is the
                // only one this gate's orphan set can see — but all five are
                // listed, because the table's job is "give every payload-
                // carrying type a payload" and singling one out would leave
                // the next reader to work out why the other four are absent.
                // The state must be a real, non-air block: `block_state_payload`
                // refuses air exactly as vanilla's provider does, so a `0` here
                // would silently drop all five.
                "block" | "block_marker" | "block_crumble" | "dust_pillar"
                | "falling_dust" => ParticleOptions::BlockState {
                    state: BlockStateRef::canonical(
                        lodestone_data::block_states::state_id("minecraft:stone")
                            .expect("stone is in the block-state registry"),
                    ),
                },
                _ => ParticleOptions::None,
            };
            p.spawn_particles(kind, [0.5, 65.0, 0.5], [0.2, 0.3, 0.4], 0.0, 1, options);
            for particle in p.engine.particles() {
                if let SpriteSource::Sheet { sheet, .. } = particle.sprite {
                    reached.insert(sheet);
                }
            }
        }
        let orphans: Vec<Sheet> = Sheet::all()
            .iter()
            .copied()
            .filter(|s| !reached.contains(s))
            .collect();
        assert!(
            orphans.is_empty(),
            "these sheets are stitched into the particle atlas and no registry type \
             reaches them through the dispatch: {orphans:?}"
        );
    }

    /// `item_slime`, `item_cobweb` and `item_snowball` must each carry **their
    /// own** item's registry id.
    ///
    /// These three are the sharpest transposition risk in the dispatch: three
    /// adjacent arms differing in one string each, all reaching one helper, and
    /// a swap is invisible everywhere downstream — the particle count, the
    /// layer, the behaviour and the quad size are byte-identical whichever item
    /// the arm names, and the only visible difference is the texture. So this
    /// asserts the id against a registry lookup made *here*, and additionally
    /// that the three are pairwise distinct, which is what a copy-pasted arm
    /// fails.
    ///
    /// It covers the producer half only: that the right id reaches the
    /// particle. Whether the *shell* can resolve that id to a sprite is
    /// `item_uv_table`'s business and is exercised by the crumb gates in
    /// `consume`.
    #[test]
    fn the_three_item_burst_types_carry_their_own_registry_item() {
        let cases: &[(&str, &str)] = &[
            ("item_slime", "minecraft:slime_ball"),
            ("item_cobweb", "minecraft:cobweb"),
            ("item_snowball", "minecraft:snowball"),
        ];
        let mut seen: Vec<Item> = Vec::new();
        for &(kind, item) in cases {
            let expected = Item::from_name(item).expect("a built-in item is in the registry");
            let mut p = resolvable();
            p.spawn_particles(
                kind,
                [0.5, 65.0, 0.5],
                [0.0, 0.0, 0.0],
                0.0,
                1,
                ParticleOptions::None,
            );
            let particles = p.engine.particles();
            assert_eq!(
                particles.len(),
                1,
                "{kind:?} must spawn exactly one particle via the generic dispatch"
            );
            assert_eq!(
                particles[0].sprite,
                SpriteSource::Item(expected),
                "{kind:?} must carry {item:?}'s validated item identity"
            );
            // The four-argument `BreakingItemParticle` constructor, not the
            // seven-argument one: `gravity = 1.0` and the quad size halved,
            // with the jitter left undamped.
            assert!(
                (particles[0].gravity - 1.0).abs() < f32::EPSILON,
                "{kind:?} must use `BreakingItemParticle`'s gravity"
            );
            seen.push(expected);
        }
        let mut distinct = seen.clone();
        distinct.sort_unstable_by_key(|item| item.registry_id());
        distinct.dedup();
        assert_eq!(
            distinct.len(),
            seen.len(),
            "the three item bursts must name three different items; got {seen:?}"
        );
    }

    /// The three `FallingLeavesParticle` variants must land on their **own**
    /// provider constants, not on a sibling's.
    ///
    /// One class, three registry types, five constants apart — and the wrong
    /// set still produces exactly one drifting leaf on the right sheet, so
    /// every reachability assertion in this module passes either way. This gate
    /// predicts the value: each arm computes the correct *and* the
    /// suspected-wrong hypothesis from the provider constants and requires the
    /// measurement to land on one.
    ///
    /// `gravity` is the discriminator because it is the one quantity that is a
    /// pure function of the provider's `fallAcceleration` with no RNG in it:
    /// `fallAcceleration * 1.2 * ACCELERATION_SCALE`. `quadSize` and the flow
    /// scales all consume random draws and so cannot be predicted exactly here.
    #[test]
    fn the_leaf_variants_differ_in_every_constant_that_separates_them() {
        /// `FallingLeavesParticle.ACCELERATION_SCALE`.
        const ACCELERATION_SCALE: f32 = 0.0025;
        /// The `1.2F` the constructor multiplies `fallAcceleration` by.
        const GRAVITY_FACTOR: f32 = 1.2;

        let cherry_gravity = 0.25 * GRAVITY_FACTOR * ACCELERATION_SCALE;
        let pale_oak_gravity = 0.07 * GRAVITY_FACTOR * ACCELERATION_SCALE;
        assert!(
            (cherry_gravity - pale_oak_gravity).abs() > f32::EPSILON,
            "the two hypotheses must differ, or this gate measures that the code runs"
        );

        let cases: &[(&str, f32, f32, ParticleOptions)] = &[
            ("cherry_leaves", cherry_gravity, pale_oak_gravity, ParticleOptions::None),
            ("pale_oak_leaves", pale_oak_gravity, cherry_gravity, ParticleOptions::None),
            // `TintedLeavesProvider` takes the **pale oak** constants exactly,
            // and differs from it only in sheet and colour. Asserting it
            // against cherry's is the mistake this arm exists to catch.
            (
                "tinted_leaves",
                pale_oak_gravity,
                cherry_gravity,
                ParticleOptions::Color { color: [0.25, 0.5, 0.75, 0.6] },
            ),
        ];
        let mut mismatches: Vec<String> = Vec::new();
        for &(kind, correct, wrong, options) in cases {
            let mut p = resolvable();
            p.spawn_particles(kind, [0.5, 65.0, 0.5], [0.0, 0.0, 0.0], 0.0, 1, options);
            let Some(particle) = p.engine.particles().first() else {
                mismatches.push(format!("{kind}: spawned nothing"));
                continue;
            };
            let got = particle.gravity;
            if (got - correct).abs() >= f32::EPSILON {
                mismatches.push(format!(
                    "{kind}: gravity {got} is neither its own {correct} nor \
                     (for diagnosis) the sibling's {wrong}"
                ));
            }
        }
        assert!(
            mismatches.is_empty(),
            "leaf provider constants are crossed: {mismatches:?}"
        );

        // The tinted variant is the only one that takes a colour off the wire,
        // and the three components are deliberately pairwise distinct so a
        // transposed channel cannot survive.
        let mut p = resolvable();
        p.spawn_particles(
            "tinted_leaves",
            [0.5, 65.0, 0.5],
            [0.0, 0.0, 0.0],
            0.0,
            1,
            ParticleOptions::Color { color: [0.25, 0.5, 0.75, 0.6] },
        );
        assert_eq!(
            p.engine.particles()[0].colour,
            [0.25, 0.5, 0.75],
            "tinted_leaves must take its RGB from the wire, in order"
        );
        // The untinted siblings must be left at the default white — a leaf that
        // picked up a colour would mean the tint leaked across arms.
        let mut q = resolvable();
        q.spawn_particles(
            "cherry_leaves",
            [0.5, 65.0, 0.5],
            [0.0, 0.0, 0.0],
            0.0,
            1,
            ParticleOptions::None,
        );
        assert_eq!(
            q.engine.particles()[0].colour,
            [1.0, 1.0, 1.0],
            "cherry_leaves carries no colour payload and must stay untinted"
        );
    }

    /// `rain` must be a `WaterDropParticle` at **its own** gravity, and the two
    /// water-column types must pull in opposite directions.
    ///
    /// Three magnitude claims that a reachability gate cannot make, each
    /// against a specific wrong hypothesis a plausible implementation lands on:
    ///
    /// | type | correct | the wrong one |
    /// |---|---|---|
    /// | `rain` | `0.06` | `0.04`, `splash`'s — the class it is the base of |
    /// | `bubble_column_up` | `-0.125` | any positive value, i.e. a sinking bubble |
    /// | `current_down` | `0.002` | `-0.125`, its sibling's |
    ///
    /// The `rain`/`splash` pair is the sharp one: `SplashParticle extends
    /// WaterDropParticle` and overrides exactly this field, so the natural way
    /// to write `rain` is to copy `splash` — which silently keeps `0.04` and
    /// leaves raindrops hanging in the air.
    #[test]
    fn the_water_types_carry_their_own_gravity_and_not_a_sibling_s() {
        fn gravity_of(kind: &str) -> f32 {
            let mut p = resolvable();
            p.spawn_particles(
                kind,
                [0.5, 65.0, 0.5],
                [0.0, 0.0, 0.0],
                0.0,
                1,
                ParticleOptions::None,
            );
            p.engine
                .particles()
                .first()
                .unwrap_or_else(|| panic!("{kind:?} must spawn a particle"))
                .gravity
        }

        let mut mismatches: Vec<String> = Vec::new();
        for &(kind, correct, wrong) in &[
            ("rain", 0.06_f32, 0.04_f32),
            ("splash", 0.04, 0.06),
            ("bubble_column_up", -0.125, 0.002),
            ("current_down", 0.002, -0.125),
        ] {
            let got = gravity_of(kind);
            if (got - correct).abs() >= f32::EPSILON {
                mismatches.push(format!(
                    "{kind}: gravity {got}, wanted {correct} (the wrong hypothesis is {wrong})"
                ));
            }
        }
        assert!(
            mismatches.is_empty(),
            "water-family gravities are crossed: {mismatches:?}"
        );

        // A rising bubble and a sinking one are the whole point of the pair, so
        // assert the *sign* separately from the magnitudes above: a future
        // edit that made both `0.002` would satisfy neither claim above only
        // by accident.
        assert!(
            gravity_of("bubble_column_up") < 0.0 && gravity_of("current_down") > 0.0,
            "a soul-sand column's bubbles must rise and a magma column's must sink"
        );
    }

    /// Each type in vanilla's own crit-particle/spell-particle/fly-towards-position
    /// families must sample the sheet **its own `particles/<name>.json` names**,
    /// not the one its Java class's better-known sibling uses.
    ///
    /// Vanilla assigns sheets per registry type, never per class: six types
    /// share vanilla's own spell-particle across four different sheets, and the damage
    /// indicator is vanilla's own crit particle that does *not* share the crit sprite.
    /// Deriving the sheet from the class is the mistake that would put
    /// `spell_N` texels on a potion mote and leave `Sheet::Effect` dead a
    /// second time — and it is invisible at the draw site, since every wrong
    /// answer here still resolves to a real sprite.
    ///
    /// Expectations read out of `.cache/mc/26.2/client-src/assets/minecraft/
    /// particles/*.json` — the pack's own texture lists, not our `Sheet` enum.
    /// Mismatches are collected rather than asserted inside the loop, so a
    /// failing run reports every wrong arm instead of only the first.
    #[test]
    fn each_spell_and_crit_type_samples_the_sheet_its_own_definition_names() {
        // (kind, the sheet whose `frames()` equals that type's `textures` list)
        let cases: &[(&str, Sheet)] = &[
            // `crit.json` -> ["critical_hit"], `enchanted_hit.json` ->
            // ["enchanted_hit"], `damage_indicator.json` -> ["damage"].
            ("crit", Sheet::CriticalHit),
            ("enchanted_hit", Sheet::EnchantedHit),
            ("damage_indicator", Sheet::Damage),
            // `effect.json` and `entity_effect.json` -> effect_7..effect_0;
            // `instant_effect.json` and `witch.json` -> spell_7..spell_0.
            ("effect", Sheet::Effect),
            ("entity_effect", Sheet::Effect),
            ("instant_effect", Sheet::Spell),
            ("witch", Sheet::Spell),
            // Single-texture sheets of their own, despite sharing
            // vanilla's own spell-particle with the four above.
            ("infested", Sheet::Infested),
            ("raid_omen", Sheet::RaidOmen),
            ("trial_omen", Sheet::TrialOmen),
            // `enchant.json` -> sga_a..sga_z; `nautilus.json` -> ["nautilus"].
            ("enchant", Sheet::Enchant),
            ("nautilus", Sheet::Nautilus),
        ];
        let mut wrong: Vec<String> = Vec::new();
        for &(kind, want) in cases {
            let mut p = resolvable();
            p.spawn_particles(kind, [0.5, 65.0, 0.5], [0.0; 3], 0.0, 1, ParticleOptions::None);
            match p.engine.particles().first().map(|q| q.sprite) {
                Some(SpriteSource::Sheet { sheet, .. }) if sheet == want => {}
                other => wrong.push(format!("{kind}: wanted {want:?}, got {other:?}")),
            }
        }
        assert!(wrong.is_empty(), "wrong sheet for {} type(s): {wrong:#?}", wrong.len());
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


    /// The potion-effect colour, driven from **real wire bytes** rather than a
    /// hand-built `ParticleOptions`.
    ///
    /// The defect this closes was entirely at the decoder: `emit::spell`
    /// already took a colour, so any gate that handed it one would have passed
    /// throughout — proving the emitter and nothing about the producer. So the
    /// input here is a `LEVEL_PARTICLES` payload transcribed from the packet's
    /// own wire layout, run through the same registry-resolved adapter `net.rs`
    /// obtains from `lodestone_registry::adapter_for_protocol(776)`, and only
    /// the namespace strip in between is done by hand — that hop has its own
    /// gate in `net.rs` (`forward_translates_particles_with_stripped_namespace`).
    ///
    /// The colour bytes are pairwise distinct (`0x11`/`0x22`/`0x33`, plus
    /// `0x44` as `entity_effect`'s alpha) so neither a channel transposition
    /// nor an ARGB/RGB24 confusion survives, and none of them is `0xFF`, so a
    /// regression back to the white default is a visible mismatch on all three
    /// channels rather than on one.
    #[cfg(feature = "live")]
    #[test]
    fn a_real_potion_effect_packet_tints_the_particle_it_spawns() {
        use lodestone_client::{ClientEvent, ConnectionState, Directive};

        /// `LEVEL_PARTICLES`'s wire layout: bool override-limiter, bool
        /// always-show, 3×f64 position, 3×f32 spread, f32 max speed, i32
        /// count, VarInt particle-type registry id, then the type's own
        /// option bytes to end of packet.
        fn payload(particle_id: u8, options: &[u8]) -> Vec<u8> {
            let mut bytes = Vec::new();
            bytes.push(0x00); // override limiter
            bytes.push(0x00); // always show
            bytes.extend_from_slice(&0.5f64.to_be_bytes());
            bytes.extend_from_slice(&65.0f64.to_be_bytes());
            bytes.extend_from_slice(&0.5f64.to_be_bytes());
            bytes.extend_from_slice(&0.0f32.to_be_bytes());
            bytes.extend_from_slice(&0.0f32.to_be_bytes());
            bytes.extend_from_slice(&0.0f32.to_be_bytes());
            bytes.extend_from_slice(&0.0f32.to_be_bytes());
            bytes.extend_from_slice(&1i32.to_be_bytes()); // count
            bytes.push(particle_id);
            bytes.extend_from_slice(options);
            bytes
        }

        /// Feeds one payload through the real adapter and hands the decoded
        /// event to the real dispatch, returning the spawned particle.
        fn spawn_from_wire(particle_id: u8, options: &[u8]) -> lodestone_particle::Particle {
            let adapter = lodestone_registry::adapter_for_protocol(776)
                .expect("the `live` feature compiles a family in for protocol 776");
            let mut world = lodestone_world::World::new();
            let directives = adapter
                .handle_packet(
                    &mut world,
                    ConnectionState::Play,
                    47, // play::clientbound::LEVEL_PARTICLES
                    &payload(particle_id, options),
                )
                .expect("a byte-accurate level_particles payload must decode");
            let [Directive::Emit(ClientEvent::Particles {
                particle,
                pos,
                offset,
                max_speed,
                count,
                options,
                ..
            })] = directives.as_slice()
            else {
                panic!("expected exactly one Particles directive, got {directives:?}");
            };
            let kind = particle.path().to_owned();
            let mut p = resolvable();
            p.spawn_particles(
                &kind,
                [pos.x, pos.y, pos.z],
                [offset.x, offset.y, offset.z],
                *max_speed,
                *count,
                *options,
            );
            assert_eq!(
                p.engine.particles().len(),
                1,
                "{kind} must dispatch to an emitter"
            );
            p.engine.particles()[0].clone()
        }

        let want_rgb = [
            0x11 as f32 / 255.0,
            0x22 as f32 / 255.0,
            0x33 as f32 / 255.0,
        ];
        // Vanilla's own spell-particle option: RGB24 then an f32 power. Power 1.0 here so
        // this gate measures only the tint; `spell_instant`'s velocity
        // multiplier has its own gate below.
        let mut spell_options = Vec::new();
        spell_options.extend_from_slice(&0x0011_2233i32.to_be_bytes());
        spell_options.extend_from_slice(&1.0f32.to_be_bytes());

        for (id, name) in [(23u8, "effect"), (53, "instant_effect")] {
            let particle = spawn_from_wire(id, &spell_options);
            assert_eq!(
                particle.colour, want_rgb,
                "{name}'s spell-particle-option colour must reach the particle, \
                 not the white default"
            );
        }

        // `ColorParticleOption`: one ARGB word, alpha in the top byte.
        let particle = spawn_from_wire(28, &0x4411_2233u32.to_be_bytes());
        assert_eq!(
            particle.colour, want_rgb,
            "entity_effect's ColorParticleOption colour must reach the particle"
        );
        assert!(
            (particle.alpha - 0x44 as f32 / 255.0).abs() < 1e-6,
            "entity_effect's alpha byte is a real field (MobEffectProvider calls \
             setAlpha with it); got {}",
            particle.alpha
        );

        // `SculkChargeParticleOptions`: one f32 roll. Deliberately not a round
        // multiple of anything, so it cannot coincide with the zero default.
        let particle = spawn_from_wire(45, &1.234_5f32.to_be_bytes());
        assert_eq!(particle.roll, 1.234_5, "sculk_charge's roll must reach the particle");
        assert_eq!(
            particle.o_roll, 1.234_5,
            "and its previous-tick roll too, or the first drawn frame interpolates \
             from zero"
        );
    }

    /// Vanilla's own spell-particle option's second field, on the one input where the right
    /// formula and the plausible wrong one give different answers regardless of
    /// what the RNG drew.
    ///
    /// Vanilla's own particle set-power is `xd *= p; yd = (yd - 0.1) * p + 0.1; zd *= p` --
    /// it rescales the vertical component **about** the `0.1` upward bias the
    /// base constructor added, rather than multiplying it. At `power = 0.0`
    /// the correct formula lands on exactly `(0, 0.1, 0)` while a naive
    /// `yd *= p` lands on exactly `(0, 0, 0)`, so the two hypotheses are
    /// separated by a deterministic value and the entropy-seeded engine cannot
    /// blur them. Any power in between would need the seed pinned to say
    /// anything at all.
    ///
    /// A zero power is also a legal wire value, not a contrivance: the field is
    /// unconditional on the wire and its data-codec default is `1.0`, so
    /// nothing stops a datapack sending one.
    #[test]
    fn a_spell_particles_power_rescales_velocity_about_the_upward_bias() {
        let mut p = resolvable();
        p.spawn_particles(
            "effect",
            [0.5, 65.0, 0.5],
            [0.0; 3],
            0.0,
            1,
            ParticleOptions::Spell { color: [0.5, 0.25, 0.75], power: 0.0 },
        );
        assert_eq!(p.engine.particles().len(), 1);
        let particle = &p.engine.particles()[0];
        assert_eq!(
            [particle.xd, particle.yd, particle.zd],
            [0.0, 0.1, 0.0],
            "setPower(0) must leave the 0.1 upward bias standing (a naive `yd *= power` \
             gives 0.0 here, and an unapplied power leaves the constructor's jitter)"
        );
    }

    /// `dragon_breath`, which had no dispatch arm at all: a dragon's breath
    /// attack and — far more often — every lingering potion cloud fell into the
    /// catch-all and drew nothing.
    ///
    /// Three things at once, because they are three failure modes of the same
    /// port. **The sheet** is `dragon_breath.json`'s own three-frame ascending
    /// `generic_5..generic_7`, not `Sheet::Generic`'s eight descending; a
    /// particle pointed at `Generic` still resolves to a real sprite, so
    /// nothing would be red. **The tint** is drawn per particle out of two
    /// narrow bands whose ranges do not overlap, so asserting each channel
    /// against its own band catches a transposition — reading blue's band into
    /// green makes green non-zero, which is the assertion that fires.
    ///
    /// What this deliberately does **not** claim to catch is green's draw being
    /// replaced by a bare `0.0`: `Mth.nextFloat` draws even when both bounds
    /// are equal, so omitting it shifts every later number in the RNG stream —
    /// but every shifted number is still a uniform float mapped into the same
    /// band, so no assertion here can see it. The transcription is right and
    /// this gate is not the thing that proves it; only the source is. **The
    /// power** is the `PowerParticleOption` payload.
    #[test]
    fn dragon_breath_draws_its_own_sheet_and_its_own_purple_band() {
        let mut p = resolvable();
        p.spawn_particles(
            "dragon_breath",
            [0.5, 65.0, 0.5],
            [0.0; 3],
            0.0,
            1,
            ParticleOptions::Power { power: 1.0 },
        );
        assert_eq!(p.engine.particles().len(), 1, "dragon_breath must dispatch");
        let particle = &p.engine.particles()[0];
        assert!(
            matches!(
                particle.sprite,
                SpriteSource::Sheet { sheet: Sheet::DragonBreath, .. }
            ),
            "dragon_breath.json names generic_5..7 ascending, which is Sheet::DragonBreath \
             and not Sheet::Generic; got {:?}",
            particle.sprite
        );
        let [r, g, b] = particle.colour;
        assert!(
            (0.717_647_1..=0.874_509_8).contains(&r),
            "red must come from Mth.nextFloat(random, 0.7176471, 0.8745098), got {r}"
        );
        assert_eq!(
            g, 0.0,
            "green's two bounds are both 0.0 -- but it is still a real draw, and a \
             non-zero value here means the blue band was read into it"
        );
        assert!(
            (0.823_529_4..=0.976_470_6).contains(&b),
            "blue must come from Mth.nextFloat(random, 0.8235294, 0.9764706), got {b}"
        );
    }

    /// `PowerParticleOption` reaching vanilla's own particle set-power, on the same
    /// deterministic input the `effect` gate uses and for the same reason: at
    /// `power = 0.0` the correct formula lands on exactly `(0, 0.1, 0)` and the
    /// plausible wrong one (`yd *= power`) on `(0, 0, 0)`.
    ///
    /// `dragon_breath` is the sharper of the two subjects, because unlike
    /// vanilla's own spell particle its constructor assigns the packet's velocity words
    /// **directly** — no jitter, no `0.1` bias — so the `+ 0.1` that survives
    /// here can only have come from `setPower` itself.
    #[test]
    fn dragon_breaths_power_reaches_set_power() {
        let mut p = resolvable();
        p.spawn_particles(
            "dragon_breath",
            [0.5, 65.0, 0.5],
            // `count == 0` takes the branch that uses `offset * max_speed` as a
            // raw velocity, so these three words are the particle's own
            // velocity rather than a scatter bound -- the only way to hand this
            // constructor a known `yd`.
            [1.0, 1.0, 1.0],
            0.5,
            0,
            ParticleOptions::Power { power: 0.0 },
        );
        assert_eq!(p.engine.particles().len(), 1);
        let particle = &p.engine.particles()[0];
        assert_eq!(
            [particle.xd, particle.yd, particle.zd],
            [0.0, 0.1, 0.0],
            "setPower(0) rescales yd about the 0.1 bias rather than multiplying it"
        );
    }

    /// [`Behaviour::DragonBreath`]'s tick is a **full override**, and the tell
    /// is horizontal: `if (y == yo) { xd *= 1.1; zd *= 1.1; }` fires on every
    /// tick a `hasPhysics = false` cloud with no vertical velocity takes, so
    /// its horizontal speed *grows* by `1.1 * friction` — `1.1 * 0.96 = 1.056`
    /// per tick — where `tick_base` would only damp it by `0.96`. The two
    /// hypotheses therefore move the number in opposite directions, which is
    /// what makes a single tick enough.
    ///
    /// That creep is the whole visual: it is what makes a lingering potion
    /// cloud spread across a floor instead of hanging where it landed.
    #[test]
    fn a_dragon_breath_cloud_accelerates_outward_rather_than_being_damped() {
        struct NoCollision;
        impl CollisionView for NoCollision {
            fn collision_boxes(&self, _x: i32, _y: i32, _z: i32, _out: &mut Vec<lodestone_physics::Aabb>) {}
        }

        let mut p = resolvable();
        p.spawn_particles(
            "dragon_breath",
            [0.5, 65.0, 0.5],
            // `count == 0` again: a purely horizontal velocity, so `y == yo`
            // holds and the creep branch is the one under test.
            [1.0, 0.0, 0.0],
            0.25,
            0,
            ParticleOptions::Power { power: 1.0 },
        );
        let before = p.engine.particles()[0].xd;
        p.tick(&NoCollision);
        let after = p.engine.particles()[0].xd;
        assert!(
            after > before,
            "a dragon_breath cloud must accelerate horizontally ({before} -> {after}); \
             tick_base's plain friction would have damped it instead"
        );
        assert!(
            (after / before - 1.056).abs() < 1e-6,
            "the growth must be exactly 1.1 * friction (0.96), got {}",
            after / before
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
        p.destroy_block([0, 64, 0], BlockStateRef::canonical(1), [1.0; 3]);
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
    /// vanilla's own client-side particle-event handling in the 26.2 client sources.
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
        // vanilla's own flame-particle's constructor adds its own small (< 0.05 per axis)
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
        // vanilla's own flame-particle's constructor almost entirely discards the seeded
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
        p.destroy_block(
            [0, 64, 0],
            BlockStateRef::canonical(1),
            [1.0, 1.0, 1.0],
        );
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
        p.destroy_block(
            [0, 64, 0],
            BlockStateRef::canonical(1),
            [1.0, 1.0, 1.0],
        );

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

    // -- The `BlockParticleOption` family ----------------------------------
    //
    // Five registry types, one wire payload, five different providers. Every
    // gate below exists because the shared payload is the *only* thing they
    // share: a copy of one arm into another compiles, spawns a particle, draws,
    // and is wrong in exactly one constant.

    /// A non-air block state to hand the family, resolved from the registry
    /// here rather than written as a literal — `block_state_payload` refuses
    /// air, so a hardcoded `0` would make every gate below pass vacuously by
    /// spawning nothing.
    use lodestone_particle::{Behaviour, Particle};

    fn stone_state() -> u32 {
        lodestone_data::block_states::state_id("minecraft:stone")
            .expect("stone is in the block-state registry")
    }

    /// Spawns one particle of `kind` with a stone `BlockParticleOption` payload
    /// and returns it. `vel` is delivered exactly, via the `count == 0` branch.
    fn spawn_block_particle(p: &mut Particles, kind: &str, vel: [f32; 3]) -> Particle {
        let before = p.engine.particles().len();
        p.spawn_particles(
            kind,
            [0.5, 65.0, 0.5],
            vel,
            1.0,
            0,
            ParticleOptions::BlockState {
                state: BlockStateRef::canonical(stone_state()),
            },
        );
        assert_eq!(
            p.engine.particles().len(),
            before + 1,
            "{kind:?} must spawn exactly one particle from a BlockState payload"
        );
        p.engine.particles()[before].clone()
    }

    /// Each of the five must wear the sprite its own provider gives it.
    ///
    /// Four take the block's atlas sprite and one — `falling_dust` — takes a
    /// generic sheet mote and carries the block's identity in its *colour*
    /// instead. That inversion is the single most copy-pasteable mistake in the
    /// family: a `falling_dust` arm written from its neighbour would render the
    /// block's own texture, which looks plausible in a screenshot and is not
    /// what a falling sand column sheds.
    #[test]
    fn the_block_particle_family_splits_four_atlas_sprites_from_one_sheet_mote() {
        let state = lodestone_data::block_states::StateId::new(stone_state())
            .expect("stone is in the generated block-state census");
        for kind in ["block", "block_crumble", "dust_pillar", "block_marker"] {
            let mut p = resolvable();
            let particle = spawn_block_particle(&mut p, kind, [0.0, 0.0, 0.0]);
            assert_eq!(
                particle.sprite,
                SpriteSource::BlockState(state),
                "{kind:?} must wear the block's own particle sprite"
            );
        }

        let mut p = resolvable();
        let particle = spawn_block_particle(&mut p, "falling_dust", [0.0, 0.0, 0.0]);
        assert!(
            matches!(particle.sprite, SpriteSource::Sheet { sheet: Sheet::Generic, .. }),
            "falling_dust must wear a generic sheet mote, not the block's sprite; got {:?}",
            particle.sprite
        );
    }

    /// The constants that separate the five, other than lifetime.
    ///
    /// Every expectation is transcribed from the provider it belongs to, and
    /// each one is the constant that a copy of the *neighbouring* arm would get
    /// wrong: `block_marker` alone has no gravity, no physics and a flat quad
    /// size; `block_crumble` alone discards the packet's velocity outright;
    /// `block`/`block_crumble`/`dust_pillar` alone carry vanilla's own terrain particle's
    /// `0.6` grey.
    #[test]
    fn the_block_particle_family_differs_in_every_constant_that_separates_them() {
        let mut p = resolvable();
        let block = spawn_block_particle(&mut p, "block", [0.0, 0.0, 0.0]);
        assert_eq!(block.gravity, 1.0, "vanilla's own terrain particle sets gravity = 1.0F");
        assert_eq!(
            block.colour,
            [0.6, 0.6, 0.6],
            "vanilla's own terrain particle starts at a 0.6 grey; an untinted block keeps it exactly"
        );
        assert!(block.has_physics, "a block fragment collides");
        assert!(
            matches!(block.behaviour, Behaviour::Terrain { .. }),
            "a block fragment takes a random quarter of the sprite"
        );

        // `setParticleSpeed(0.0, 0.0, 0.0)` — exactly zero, on all three axes.
        // The construction still runs through `Particle(level, x, y, z, xa, ya,
        // za)`, which can never produce a zero `yd` (it adds a flat `+ 0.1`), so
        // a crumble arm that forgot the override would fail this on `yd` alone.
        let mut p = resolvable();
        let crumble = spawn_block_particle(&mut p, "block_crumble", [1.0, 1.0, 1.0]);
        assert_eq!(
            [crumble.xd, crumble.yd, crumble.zd],
            [0.0, 0.0, 0.0],
            "vanilla's own crumbling provider discards the constructed velocity entirely"
        );

        // Vanilla's own block-marker particle: gravity 0, no physics, no tint, and `getQuadSize`
        // returning a flat `0.5F`. The quad size is asserted at age 0, where the
        // `* 32` fade-in every other member of this family's neighbours use
        // would read 0.0 — the one sample that separates a constant from a ramp.
        let mut p = resolvable();
        let marker = spawn_block_particle(&mut p, "block_marker", [1.0, 1.0, 1.0]);
        assert_eq!(marker.gravity, 0.0, "vanilla's own block marker sets gravity = 0.0F");
        assert!(!marker.has_physics, "vanilla's own block marker sets hasPhysics = false");
        assert_eq!(
            marker.colour,
            [1.0, 1.0, 1.0],
            "vanilla's own block marker never touches rCol — it is not a terrain particle"
        );
        assert_eq!(
            marker.quad_size(0.0),
            0.5,
            "vanilla's own block-marker quad-size accessor returns a flat 0.5F; a `* 32` fade-in ramp \
             would read 0.0 at age 0"
        );
        assert_eq!(
            marker.quad_size(0.0),
            marker.quad_size(1.0),
            "a flat size does not move with the partial tick"
        );

        // Vanilla's own falling-dust particle carries the block's colour and nothing of its
        // texture, and its quad size is the `* 32` ramp — the exact opposite of
        // the marker above, which is why the two are asserted against each other.
        let mut p = resolvable();
        p.state_tint = Arc::new(vec![[1.0; 3]; stone_state() as usize + 1]);
        let dust = spawn_block_particle(&mut p, "falling_dust", [0.0, 0.0, 0.0]);
        assert!(
            matches!(dust.behaviour, Behaviour::FallingDust { .. }),
            "falling_dust needs its own tick — the raw 0.003 fall and the -0.14 clamp"
        );
        assert_eq!(
            dust.quad_size(0.0),
            0.0,
            "vanilla's own falling-dust quad-size accessor is the `* 32` fade-in, which is 0 at age 0"
        );
        assert!(
            dust.quad_size(0.0) < dust.quad_size(1.0),
            "the ramp must actually rise between two partial ticks"
        );
    }

    /// `falling_dust` must carry the block's tint into its **colour**, since
    /// that is where all of the block's identity lives for this one type.
    ///
    /// The tint is deliberately neither grey nor white: an arm that transposed
    /// two channels, or that dropped the tint for vanilla's own terrain particle's `0.6`
    /// grey, passes against `[1.0; 3]` and fails here.
    #[test]
    fn falling_dust_wears_the_block_tint_as_its_colour() {
        let tint = [0.25f32, 0.5, 0.75];
        let state = stone_state();
        let mut p = resolvable();
        let mut table = vec![[1.0f32; 3]; state as usize + 1];
        table[state as usize] = tint;
        p.state_tint = Arc::new(table);
        let dust = spawn_block_particle(&mut p, "falling_dust", [0.0, 0.0, 0.0]);
        assert_eq!(
            dust.colour, tint,
            "the block tint is falling_dust's whole identity — the sprite is a grey mote"
        );

        // The control: the same tint must *not* reach a terrain-family member
        // undivided, because those multiply it into vanilla's own terrain particle's 0.6.
        let mut p = resolvable();
        let mut table = vec![[1.0f32; 3]; state as usize + 1];
        table[state as usize] = tint;
        p.state_tint = Arc::new(table);
        let block = spawn_block_particle(&mut p, "block", [0.0, 0.0, 0.0]);
        assert_eq!(
            block.colour,
            [0.6 * tint[0], 0.6 * tint[1], 0.6 * tint[2]],
            "a block fragment is the tint times vanilla's own terrain particle's 0.6 grey"
        );
    }

    /// Each provider's lifetime roll, as a closed interval derived from its own
    /// expression rather than from the code under test.
    ///
    /// The four intervals are the sharpest discriminator this family has, and
    /// three of them are *reachable in full* — `nextInt(10) + 1` and
    /// `nextInt(20) + 20` hit both ends within a few hundred draws, so the
    /// observed extremes are asserted exactly and an off-by-one in either the
    /// bound or the offset fails by name. The base constructor's own
    /// `(int)(4.0F / (nextFloat() * 0.9F + 0.1F))` reaches `40` only at
    /// `nextFloat() == 0.0` (one draw in 2²⁴), so its upper end is asserted as
    /// containment plus a floor that the *wrong* hypothesis — a copy of
    /// `block_crumble`'s roll, capped at 10 — cannot reach.
    #[test]
    fn each_block_particle_provider_rolls_its_own_lifetime_interval() {
        /// Enough draws that a 1-in-20 outcome is certain and a 1-in-2²⁴ one is
        /// still out of reach, which is what makes the assertions below split
        /// into "exact extremes" and "containment plus a floor".
        const DRAWS: usize = 2_000;

        struct Case {
            kind: &'static str,
            /// `[min, max]`, from the provider's own expression.
            interval: [i32; 2],
            /// The lowest value whose observation rules out the neighbouring
            /// provider's roll, or `None` when both extremes are reachable and
            /// asserted exactly instead.
            floor: Option<i32>,
        }
        let cases = [
            // Vanilla's own base particle constructor's own `(int)(4.0F / (nextFloat() * 0.9F +
            // 0.1F))`, untouched by its own terrain-particle provider. A copy of
            // `block_crumble`'s `nextInt(10) + 1` would top out at 10.
            Case { kind: "block", interval: [4, 40], floor: Some(11) },
            // Vanilla's own crumbling provider: `setLifetime(random.nextInt(10) + 1)`.
            // Dropping the `+ 1` gives [0, 9]; both ends are asserted.
            Case { kind: "block_crumble", interval: [1, 10], floor: None },
            // Vanilla's own dust-pillar provider: `setLifetime(random.nextInt(20) + 20)`.
            Case { kind: "dust_pillar", interval: [20, 39], floor: None },
            // Vanilla's own block-marker particle: a flat `this.lifetime = 80`.
            Case { kind: "block_marker", interval: [80, 80], floor: None },
            // Vanilla's own falling-dust particle: base `(int)(32.0 / (nextFloat() * 0.8 +
            // 0.2))` in [32, 160], then `(int) max(base * 0.9F, 1.0F)` in
            // [28, 144]. Dropping the `* 0.9` leaves the base interval, whose
            // upper half this containment check excludes; 144 itself needs
            // `nextFloat() == 0.0` and so is not asserted as an extreme.
            Case { kind: "falling_dust", interval: [28, 144], floor: Some(130) },
        ];

        for case in &cases {
            let mut p = resolvable();
            let mut lo = i32::MAX;
            let mut hi = i32::MIN;
            for _ in 0..DRAWS {
                let particle = spawn_block_particle(&mut p, case.kind, [0.0, 0.0, 0.0]);
                lo = lo.min(particle.lifetime);
                hi = hi.max(particle.lifetime);
                p.engine.clear();
            }
            let [want_lo, want_hi] = case.interval;
            assert!(
                lo >= want_lo && hi <= want_hi,
                "{}: lifetimes {lo}..={hi} escaped the provider's own interval \
                 {want_lo}..={want_hi}",
                case.kind
            );
            assert_eq!(
                lo, want_lo,
                "{}: the lowest lifetime in {DRAWS} draws must be the interval's own \
                 minimum, not one above or below it",
                case.kind
            );
            match case.floor {
                None => assert_eq!(
                    hi, want_hi,
                    "{}: both ends of this roll are reachable, so the highest lifetime \
                     in {DRAWS} draws must be the interval's own maximum",
                    case.kind
                ),
                Some(floor) => assert!(
                    hi >= floor,
                    "{}: the highest lifetime in {DRAWS} draws was {hi}, below the {floor} \
                     that separates this roll from the neighbouring provider's",
                    case.kind
                ),
            }
        }
    }

    /// vanilla's own dust-pillar provider's vertical velocity is the packet's own `ya`
    /// **plus** a gaussian, not a gaussian alone.
    ///
    /// That additive base is the whole reason a mace smash throws a column
    /// upward rather than a puff sideways, and it is invisible in a single
    /// sample because the gaussian swamps it. Averaging pins it: with `ya = 7`
    /// the mean must sit on 7 to within a few hundredths, where the
    /// gaussian-alone hypothesis sits on 0. Both hypotheses are computed, and
    /// the tolerance is derived from the roll's own standard error
    /// (`0.5 / sqrt(n)`, so `~0.035` at n = 200) rather than picked.
    #[test]
    fn dust_pillar_adds_the_packets_own_vertical_velocity_to_its_gaussian() {
        const DRAWS: usize = 200;
        const YA: f64 = 7.0;

        let mut p = resolvable();
        let mut sum = 0.0f64;
        let mut horizontal = 0.0f64;
        for _ in 0..DRAWS {
            #[expect(clippy::cast_possible_truncation, reason = "7.0 is exact in f32")]
            let particle = spawn_block_particle(&mut p, "dust_pillar", [0.0, YA as f32, 0.0]);
            sum += particle.yd;
            horizontal += particle.xd.abs() + particle.zd.abs();
            p.engine.clear();
        }
        #[expect(clippy::cast_precision_loss, reason = "200 is exact in f64")]
        let mean = sum / DRAWS as f64;
        assert!(
            (mean - YA).abs() < 0.25,
            "mean yd was {mean}, not the packet's own {YA} — the gaussian-alone \
             hypothesis puts this on 0.0"
        );

        // And the horizontal axes must *not* pick the same base up: they are
        // `gaussian / 30` with no `xa`/`za` term at all, so a mean absolute
        // value well under one is the shape, and copying the vertical line
        // across would put them on 7 as well.
        #[expect(clippy::cast_precision_loss, reason = "200 is exact in f64")]
        let mean_horizontal = horizontal / (2.0 * DRAWS as f64);
        assert!(
            mean_horizontal < 0.1,
            "mean |xd|/|zd| was {mean_horizontal}; the horizontal terms are \
             `gaussian / 30` and carry no velocity from the packet"
        );
    }

    /// Vanilla's own falling-dust particle tick falls by a **raw** `0.003` per tick and
    /// clamps at `-0.14`, and neither number goes through `gravity`.
    ///
    /// Both hypotheses are computed from outside constants. The correct one
    /// gives `yd == -0.003 * n` for the first few ticks; reading `0.003` as a
    /// `gravity` multiplier instead would put it through the base tick's
    /// `yd -= 0.04 * gravity` and give `-0.00012 * n`, a thirteenth of the
    /// speed — and would lose the clamp entirely, so a long run separates them
    /// a second way.
    #[test]
    fn falling_dust_falls_at_a_raw_rate_and_clamps_at_terminal_velocity() {
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

        // A mote with no initial velocity, so `yd` is purely the fall term.
        let mut p = resolvable();
        p.spawn_particles(
            "falling_dust",
            [0.5, 65.0, 0.5],
            [0.0, 0.0, 0.0],
            0.0,
            0,
            ParticleOptions::BlockState {
                state: BlockStateRef::canonical(stone_state()),
            },
        );
        assert_eq!(p.engine.particles().len(), 1);

        const TICKS: i32 = 5;
        for _ in 0..TICKS {
            p.tick(&Air);
        }
        let yd = p.engine.particles()[0].yd;
        let correct = -f64::from(0.003_f32) * f64::from(TICKS);
        let as_gravity = -0.04 * f64::from(0.003_f32) * f64::from(TICKS);
        assert!(
            (yd - correct).abs() < 1e-9,
            "yd after {TICKS} ticks was {yd}; the raw-0.003 fall predicts {correct} and \
             the `gravity = 0.003` reading predicts {as_gravity}"
        );

        // Long enough to pass the clamp several times over: 0.14 / 0.003 is
        // ~47 ticks, and the mote's lifetime is at least 28, so this is run on
        // a fresh one whose lifetime is long enough to observe it.
        let mut clamped = None;
        for _ in 0..400 {
            let mut p = resolvable();
            p.spawn_particles(
                "falling_dust",
                [0.5, 65.0, 0.5],
                [0.0, 0.0, 0.0],
                0.0,
                0,
                ParticleOptions::BlockState {
                    state: BlockStateRef::canonical(stone_state()),
                },
            );
            if p.engine.particles()[0].lifetime < 60 {
                continue;
            }
            for _ in 0..60 {
                p.tick(&Air);
            }
            if let Some(particle) = p.engine.particles().first() {
                clamped = Some(particle.yd);
            }
            break;
        }
        let yd = clamped.expect("a mote with a 60-tick lifetime within 400 rolls");
        assert_eq!(
            yd,
            f64::from(-0.14_f32),
            "60 ticks of a raw 0.003 fall is -0.18 unclamped; vanilla's own falling-dust particle \
             holds it at -0.14, and the `gravity` reading would be at -0.0072"
        );
    }

    /// The three refusals, each with a control proving the detector fires.
    ///
    /// Air is vanilla's own (its own create-terrain-particle returns `null` for it), a missing
    /// payload is ours, and an out-of-census canonical payload is the registry
    /// boundary. A protocol-local value is a separate refusal: it can overlap a
    /// generated raw number, but this renderer has no matching version or
    /// dynamic-registry model resolver.
    /// All three are silent drops, so without the control arm below an emitter
    /// that spawned *nothing at all* would satisfy them.
    #[test]
    fn the_block_particle_family_refuses_air_and_a_missing_payload() {
        let air = lodestone_data::block_states::air_state_id();
        for kind in ["block", "block_crumble", "dust_pillar", "block_marker", "falling_dust"] {
            let mut p = resolvable();
            p.spawn_particles(
                kind,
                [0.5, 65.0, 0.5],
                [0.0, 0.0, 0.0],
                0.0,
                1,
                ParticleOptions::BlockState {
                    state: BlockStateRef::canonical(air),
                },
            );
            assert_eq!(
                p.engine.particles().len(),
                0,
                "{kind:?} must refuse the air state, as vanilla's own create-terrain-particle does"
            );

            let mut p = resolvable();
            p.spawn_particles(kind, [0.5, 65.0, 0.5], [0.0, 0.0, 0.0], 0.0, 1, ParticleOptions::None);
            assert_eq!(
                p.engine.particles().len(),
                0,
                "{kind:?} must drop rather than guess when the payload is absent"
            );

            let mut p = resolvable();
            p.spawn_particles(
                kind,
                [0.5, 65.0, 0.5],
                [0.0, 0.0, 0.0],
                0.0,
                1,
                ParticleOptions::BlockState {
                    state: BlockStateRef::canonical(lodestone_data::block_states::STATE_COUNT),
                },
            );
            assert_eq!(
                p.engine.particles().len(),
                0,
                "{kind:?} must reject an out-of-census state before emitter lookup"
            );

            // This raw number is intentionally a valid built-in stone state.
            // The source tag, not the number's range, decides whether this
            // 26.2 model table may consume it.
            let mut p = resolvable();
            p.spawn_particles(
                kind,
                [0.5, 65.0, 0.5],
                [0.0, 0.0, 0.0],
                0.0,
                1,
                ParticleOptions::BlockState {
                    state: BlockStateRef::protocol_local(stone_state()),
                },
            );
            assert_eq!(
                p.engine.particles().len(),
                0,
                "{kind:?} must not treat a protocol-local value as a generated 26.2 state"
            );

            // The control: the same call with a real state must spawn.
            let mut p = resolvable();
            p.spawn_particles(
                kind,
                [0.5, 65.0, 0.5],
                [0.0, 0.0, 0.0],
                0.0,
                1,
                ParticleOptions::BlockState {
                    state: BlockStateRef::canonical(stone_state()),
                },
            );
            assert_eq!(
                p.engine.particles().len(),
                1,
                "{kind:?} must spawn for a real block state, or the two refusals above \
                 prove nothing"
            );
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
        p.destroy_block(
            [0, 64, 0],
            BlockStateRef::canonical(1),
            [1.0, 1.0, 1.0],
        );

        // The shade now travels in its own instance lane instead of being folded
        // into `colour`, so these read it directly rather than dividing out the
        // 0.6 vanilla's own terrain particle scales the block colour by in its constructor.
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
            "instance colour {} is not vanilla's own terrain particle's bare 0.6 tint — the light \
             term is being premultiplied into it again, which is the gamma-space bug",
            p.instances[0].colour[0]
        );

        // Block light 0, sky light 0. `get_brightness(0)` is 0, but vanilla seeds
        // the accumulator with its own per-dimension ambient color — `0x0A0A0A`
        // in the overworld — so an unlit particle is not black either: it
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
    /// `state_tint_of`'s multiply reaching vanilla's own terrain particle's colour — with no
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
            p.destroy_block([0, 64, 0], BlockStateRef::canonical(state), [1.0; 3]);
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
        p.destroy_block([0, 64, 0], BlockStateRef::canonical(2), [0.5, 0.5, 0.5]);
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

    /// Direct local destroy effects lower only canonical values at the same
    /// generated-state boundary as the packet family. A protocol-local value
    /// may overlap the census but must still be dropped; the canonical control
    /// proves the detector is not a no-op.
    #[test]
    fn direct_destroy_effects_respect_source_and_census() {
        let mut p = Particles::new(None);
        let stone = lodestone_data::block::Block::Stone.default_state().raw();
        p.destroy_block(
            [0, 64, 0],
            BlockStateRef::protocol_local(stone),
            [1.0; 3],
        );
        assert!(
            p.engine.is_empty(),
            "a protocol-local value must not select a built-in sprite merely because its raw number fits"
        );
        p.destroy_block([0, 64, 0], BlockStateRef::canonical(stone), [1.0; 3]);
        assert_eq!(
            p.engine.particles().len(),
            64,
            "a valid built-in state must still produce the full-cube burst"
        );
        p.engine.clear();
        p.breaking_block(
            [0, 64, 0],
            BlockStateRef::protocol_local(stone),
            [1.0; 3],
            emit::Face::Up,
        );
        assert!(
            p.engine.is_empty(),
            "the mining-hit emitter must reject the same protocol-local state"
        );
        p.breaking_block(
            [0, 64, 0],
            BlockStateRef::canonical(stone),
            [1.0; 3],
            emit::Face::Up,
        );
        assert_eq!(
            p.engine.particles().len(),
            1,
            "the canonical mining-hit control must emit exactly one fragment"
        );
        p.engine.clear();
        p.destroy_block(
            [0, 64, 0],
            BlockStateRef::canonical(lodestone_data::block_states::STATE_COUNT),
            [1.0; 3],
        );
        assert!(
            p.engine.is_empty(),
            "an out-of-census state must be dropped before sprite resolution"
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
        p.destroy_block(
            [0, 64, 0],
            BlockStateRef::canonical(1),
            [1.0, 1.0, 1.0],
        );
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
        // Every wired registry type, driven through the same `spawn_particles`
        // entry the network path uses — not a hand-listed set of `emit::` calls.
        //
        // The list this replaces named twenty-odd emitters and had to be
        // extended by hand for each new sheet, which makes it exactly the
        // fixture corpus that certifies "the sheets I remembered" rather than
        // "the sheets that exist". Driving the registry means a new dispatch arm
        // over a sheet whose frame names are wrong fails *here*, against the
        // real jar, which is the only place a naming-convention mistake shows:
        // a hermetic `(Sheet, frame) -> UV` fixture resolves any name at all.
        let mut wired = 0usize;
        for id in 0..lodestone_data::particle_types::PARTICLE_TYPE_COUNT {
            #[expect(
                clippy::cast_possible_wrap,
                reason = "the registry count is far below i32::MAX"
            )]
            let Some(id) = lodestone_data::particle_types::ParticleTypeId::new(id as i32) else {
                continue;
            };
            let name = lodestone_data::particle_types::particle_type_name(id);
            let kind = name.split_once(':').map_or(name, |(_, path)| path);
            let options = match kind {
                "dust" => ParticleOptions::Dust { color: [1.0, 0.0, 0.0], scale: 1.0 },
                "dust_color_transition" => ParticleOptions::DustColorTransition {
                    from_color: [1.0, 0.0, 0.0],
                    to_color: [0.0, 0.0, 1.0],
                    scale: 1.0,
                },
                // The `BlockParticleOption` family is deliberately left
                // payload-free here, unlike in
                // `no_sheet_is_atlas_resident_and_unreachable_from_the_dispatch`:
                // four of the five wear a `SpriteSource::BlockState`, and this
                // `Particles` was built with no `BlockModels`, so giving them a
                // payload would spawn particles that resolve against nothing and
                // fail this gate's `unresolved == 0` for a reason that has
                // nothing to do with the particle *sheet* atlas it exists to
                // check. Their sprite resolution is `resolved_terrain_particles_
                // produce_instances_inside_the_sprite_rect`'s subject.
                _ => ParticleOptions::None,
            };
            let before = p.engine.particles().len();
            p.spawn_particles(kind, [0.5, 65.0, 0.5], [0.2, 0.3, 0.4], 0.05, 1, options);
            if p.engine.particles().len() > before {
                wired += 1;
            }
        }
        let alive = p.engine.particles().len();
        eprintln!("wired particle types: {wired}/{}", lodestone_data::particle_types::PARTICLE_TYPE_COUNT);
        assert!(
            wired >= 60,
            "far fewer types produced a particle than expected ({wired}); either the \
             dispatch regressed or the registry moved"
        );
        assert!(alive >= wired, "every wired type must have added a particle, got {alive}");

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
        // Exactly one live particle legitimately draws nothing:
        // `explosion_emitter` is a `NoRenderParticle` (`Behaviour::
        // HugeExplosionSeed`), excluded from `extract` by construction. Naming
        // it rather than relaxing the equality keeps a *second* undrawn type
        // from hiding behind a `>=`.
        let seeds = p
            .engine
            .particles()
            .iter()
            .filter(|q| q.behaviour == lodestone_particle::Behaviour::HugeExplosionSeed)
            .count();
        assert_eq!(seeds, 1, "only `explosion_emitter` may be a no-render seed");
        assert_eq!(frame.drawn, frame.alive - seeds);
    }

    fn campfire_state(lit: bool) -> lodestone_data::block_states::StateId {
        (0..lodestone_data::block_states::STATE_COUNT)
            .map(|raw| {
                lodestone_data::block_states::StateId::new(raw)
                    .expect("the generated state census range is valid")
            })
            .find(|state| {
                state.block() == lodestone_data::block::Block::Campfire
                    && state
                    .properties()
                    .iter()
                    .any(|(name, value)| {
                        *name == "lit" && *value == if lit { "true" } else { "false" }
                    })
            })
            .expect("the 26.2 state table must contain the requested campfire state")
    }

    #[test]
    fn campfire_block_entity_tick_emits_the_vanilla_plume_shape_and_lifetimes() {
        for (signal, lifetime) in [(false, 80..130), (true, 280..330)] {
            let mut particles = Particles::new(None);
            particles.engine = ParticleEngine::seeded(4096);
            particles.campfire_block_entity_tick(&[([2, 64, 18], signal)]);

            let plume = particles.engine.particles();
            let plume_len = plume.len();
            assert!(
                (2..=3).contains(&plume_len),
                "a successful 26.2 smoke roll emits two or three particles"
            );
            for particle in plume {
                assert_eq!(
                    particle.behaviour,
                    lodestone_particle::Behaviour::CampfireSmoke
                );
                assert!(lifetime.contains(&particle.lifetime));
                assert!((2.0 + 1.0 / 6.0..=2.0 + 5.0 / 6.0).contains(&particle.x));
                assert!((18.0 + 1.0 / 6.0..=18.0 + 5.0 / 6.0).contains(&particle.z));
                assert!((64.0..66.0).contains(&particle.y));
                assert!(particle.yd >= 0.07 && particle.yd < 0.072);
            }
            let frame = particles.extract(&Camera::default(), 0.0, &|_, _, _| {
                Some(lodestone_particle::FULL_BRIGHT)
            });
            assert_eq!(frame.campfire_smoke_alive, plume_len);
        }
    }

    #[test]
    fn block_animate_tick_does_not_duplicate_the_block_entity_smoke_plume() {
        let mut particles = Particles::new(None);
        particles.engine = ParticleEngine::seeded(4096);
        particles.animate_block([2, 64, 18], campfire_state(true));
        assert!(
            particles.engine.particles().is_empty(),
            "26.2 owns the main plume in its own campfire-block-entity particle tick"
        );
    }

    /// The ambient probe crosses the raw-state boundary once. Its valid control
    /// proves this test is not satisfied by an ambient loop that never emits.
    #[test]
    fn ambient_probe_drops_an_out_of_census_state_before_typed_block_dispatch() {
        let mut particles = Particles::new(None);
        particles.engine = ParticleEngine::seeded(4096);
        particles.ambient_tick([0.0, 64.0, 0.0], &mut |_| {
            lodestone_data::block_states::STATE_COUNT
        });
        assert!(
            particles.engine.particles().is_empty(),
            "an out-of-census probe result must not reach a block-specific emitter"
        );

        particles.ambient_tick([0.0, 64.0, 0.0], &mut |_| {
            lodestone_data::block::Block::EndRod.default_state().raw()
        });
        assert_eq!(
            particles.engine.particles().len(),
            AMBIENT_SAMPLES,
            "a valid typed-block control must reach the end-rod emitter once per probe"
        );
    }

    /// The world-coverage closure: every one of the twenty particle types this
    /// pass added a wire-dispatch arm for actually reaches live, resolved
    /// geometry — not just a `match` arm that logs and drops.
    ///
    /// `spawn_one`, not `spawn_particles`: unlike the count-loop wrapper,
    /// `spawn_one` is exactly the site `world-coverage`'s "wire dispatch"
    /// detector reads its arm literals from, so a type present here is
    /// present in the same sense the coverage census counts. `count` stays
    /// out of it entirely.
    ///
    /// `vibration` and `trail` are deliberately absent from this table: both
    /// need a decoded target position this client's `ParticleOptions` does
    /// not carry yet (an adapter-side change, out of this pass's scope), and
    /// `elder_guardian` draws a full animated entity mesh rather than a
    /// billboard, which this particle engine has no facility for at all. All
    /// three are the tracked remainder in `docs/particle-catalogue.md`.
    #[test]
    fn every_family_this_pass_added_resolves_to_live_drawn_geometry() {
        struct NoCollision;
        impl CollisionView for NoCollision {
            fn collision_boxes(&self, _x: i32, _y: i32, _z: i32, _out: &mut Vec<lodestone_physics::Aabb>) {}
        }

        // Non-rendering spawners draw nothing themselves — see
        // `Behaviour::HugeExplosionSeed`'s doc and the two new ones this pass
        // added — so this table ticks each one until a child exists before
        // asking `extract` for a quad count. Every other type here is a plain
        // billboard and draws on its very first, untouched frame.
        const SPAWNERS: &[&str] =
            &["geyser", "noxious_gas_cloud", "gust_emitter_large", "gust_emitter_small"];

        for kind in [
            "geyser",
            "geyser_base",
            "geyser_poof",
            "geyser_plume",
            "noxious_gas",
            "noxious_gas_cloud",
            "sulfur_bubbles",
            "sulfur_cube_goo",
            "trial_spawner_detection",
            "trial_spawner_detection_ominous",
            "vault_connection",
            "ominous_spawning",
            "gust_emitter_large",
            "gust_emitter_small",
            "pause_mob_growth",
            "reset_mob_growth",
            "shriek",
        ] {
            let mut p = resolvable();
            p.spawn_one(
                kind,
                [0.5, 65.0, 0.5],
                [0.02, 0.02, 0.02],
                ParticleOptions::None,
            );
            assert!(
                !p.engine.particles().is_empty(),
                "{kind}: spawn_one produced no live particle at all"
            );
            if SPAWNERS.contains(&kind) {
                for _ in 0..4 {
                    if p.engine.particles().len() > 1 {
                        break;
                    }
                    p.engine.tick(&NoCollision);
                }
                assert!(
                    p.engine.particles().len() > 1,
                    "{kind}: still only the seed after several ticks — its own schedule \
                     spawned no children"
                );
            }
            let frame = p.extract(&Camera::default(), 0.0, &|_, _, _| {
                Some(lodestone_particle::FULL_BRIGHT)
            });
            assert_eq!(
                frame.unresolved, 0,
                "{kind}: at least one instance did not resolve against the sheet fixture"
            );
            assert!(
                frame.drawn > 0,
                "{kind}: {} live particle(s) but zero drawn quads",
                p.engine.particles().len()
            );
        }
    }

    /// The three non-rendering spawners specifically: each must itself draw
    /// nothing (`Behaviour::*Seed` is excluded from `extract` by design, the
    /// same way `Behaviour::HugeExplosionSeed` already was), and each must
    /// still produce its own follow-up geometry once ticked — the two
    /// symmetric ways a spawner can look "done" while doing nothing: drawing
    /// itself (wrong; it would double the population once children are
    /// added), or never spawning a child at all (the actual absence a
    /// `no-op tick` would produce).
    #[test]
    fn the_three_new_spawners_draw_nothing_themselves_but_seed_real_children() {
        struct NoCollision;
        impl CollisionView for NoCollision {
            fn collision_boxes(&self, _x: i32, _y: i32, _z: i32, _out: &mut Vec<lodestone_physics::Aabb>) {}
        }

        for kind in ["noxious_gas_cloud", "gust_emitter_large", "geyser"] {
            let mut p = resolvable();
            p.spawn_one(kind, [0.5, 65.0, 0.5], [0.0, 0.0, 0.0], ParticleOptions::None);
            assert_eq!(
                p.engine.particles().len(),
                1,
                "{kind}: the seed itself must be the only live particle before its first tick"
            );
            let frame = p.extract(&Camera::default(), 0.0, &|_, _, _| {
                Some(lodestone_particle::FULL_BRIGHT)
            });
            assert_eq!(
                frame.drawn, 0,
                "{kind}: the seed drew a quad — it must be excluded from extraction like \
                 every other non-rendering particle"
            );

            // Tick until a child appears (each schedule fires on its own
            // cadence — `noxious_gas_cloud`/`geyser` every two ticks,
            // `gust_emitter_large` every tick — so a handful of ticks covers
            // all three without depending on the exact schedule).
            let mut spawned_a_child = false;
            for _ in 0..4 {
                p.engine.tick(&NoCollision);
                if p.engine.particles().len() > 1 {
                    spawned_a_child = true;
                    break;
                }
            }
            assert!(
                spawned_a_child,
                "{kind}: still only the seed after several ticks — its own schedule spawned \
                 no children"
            );
        }
    }
}

/// How many random block positions the ambient emitter samples per tick, and how
/// far from the eye it looks.
///
/// Vanilla's own client-level animate-tick draws **667** positions in a ±16 box twice
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
    /// Tick the main plume for every loaded lit campfire block entity.
    ///
    /// Minecraft 26.2 runs this from its own campfire-block-entity particle
    /// tick, once
    /// per client simulation tick. Each source has an independent 11% chance
    /// to emit a burst of two or three particles. Its own campfire-block
    /// animate-tick
    /// owns only the occasional lava fleck and crackle sound, so putting this
    /// in [`Self::ambient_tick`] both misses campfires outside that random scan
    /// and gives sampled ones the wrong probability.
    pub fn campfire_block_entity_tick(&mut self, sources: &[([i32; 3], bool)]) {
        for &(block, signal) in sources {
            if self.engine.rng().next_f32() >= 0.11 {
                continue;
            }
            let count = self.engine.rng().next_i32_bound(2) + 2;
            for _ in 0..count {
                let (x, y, z) = {
                    let rng = self.engine.rng();
                    let x_offset = rng.next_f64() / 3.0;
                    let x_sign = if rng.next_bool() { 1.0 } else { -1.0 };
                    let y_offset = rng.next_f64() + rng.next_f64();
                    let z_offset = rng.next_f64() / 3.0;
                    let z_sign = if rng.next_bool() { 1.0 } else { -1.0 };
                    (
                        f64::from(block[0]) + 0.5 + x_offset * x_sign,
                        f64::from(block[1]) + y_offset,
                        f64::from(block[2]) + 0.5 + z_offset * z_sign,
                    )
                };
                emit::campfire_smoke(
                    &mut self.engine,
                    x,
                    y,
                    z,
                    0.0,
                    0.07,
                    0.0,
                    signal,
                );
            }
        }
    }

    /// Emit this tick's **client-predicted** ambient particles — vanilla's own
    /// per-block animate-tick, which is not on the wire at all.
    ///
    /// # Why this cannot be a server-event consumer
    ///
    /// A torch's flame, a nether portal's shimmer and an end rod's sparkle are
    /// spawned by vanilla's own client-level animate-tick walking random nearby positions and
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
                rng.next_i32_bound(span) - AMBIENT_RANGE,
                rng.next_i32_bound(span) - AMBIENT_RANGE,
                rng.next_i32_bound(span) - AMBIENT_RANGE,
            ];
            let block = [
                centre[0] + offset[0],
                centre[1] + offset[1],
                centre[2] + offset[2],
            ];
            let Some(state) = lodestone_data::block_states::StateId::new(probe(block)) else {
                continue;
            };
            if state.block() == lodestone_data::block::Block::Air {
                continue;
            }
            self.animate_block(block, state);
        }
    }

    /// One block's `animateTick`, for the handful of blocks a survival player
    /// actually notices. Silent for everything else.
    fn animate_block(
        &mut self,
        block: [i32; 3],
        state: lodestone_data::block_states::StateId,
    ) {
        let block_kind = state.block();
        let props = state.properties();
        let prop = |key: &str| props.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
        let [bx, by, bz] = [
            f64::from(block[0]),
            f64::from(block[1]),
            f64::from(block[2]),
        ];
        match block_kind {
            // Vanilla's own torch-block animate-tick: one flame and one smoke at the flame's
            // own position, which for a wall torch is offset *away* from the wall
            // it hangs on. Using the block centre for both puts the flame inside
            // the wall.
            lodestone_data::block::Block::Torch
            | lodestone_data::block::Block::SoulTorch
            | lodestone_data::block::Block::WallTorch
            | lodestone_data::block::Block::SoulWallTorch => {
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
                if matches!(
                    block_kind,
                    lodestone_data::block::Block::SoulTorch
                        | lodestone_data::block::Block::SoulWallTorch
                ) {
                    emit::soul_fire_flame(&mut self.engine, x, y, z, 0.0, 0.0, 0.0);
                } else {
                    emit::flame(&mut self.engine, x, y, z, 0.0, 0.0, 0.0);
                }
            }
            // Vanilla's own nether-portal-block animate-tick: four motes per tick at random
            // points inside the block, drifting on a signed offset — which for
            // vanilla's own portal particle is the *amplitude* it converges from, not a speed.
            lodestone_data::block::Block::NetherPortal
            | lodestone_data::block::Block::EndGateway => {
                for _ in 0..4 {
                    let rng = self.engine.rng();
                    let (rx, ry, rz) = (
                        f64::from(rng.next_f32()),
                        f64::from(rng.next_f32()),
                        f64::from(rng.next_f32()),
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
            // Vanilla's own end-rod-block animate-tick: one sparkle just off the rod's tip,
            // along whatever axis it points.
            lodestone_data::block::Block::EndRod => {
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
            _ => {}
        }
    }
}
