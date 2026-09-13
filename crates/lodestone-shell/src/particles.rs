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
//! `docs/particles.md`.

use std::collections::HashMap;
use std::sync::Arc;

use lodestone_assets::{ParticleAtlas, ResourceLocation};
use lodestone_data::block_states::StateId;
use lodestone_data::item::Item;
use lodestone_model::event::{BlockStateRef, ParticleOptions};
use lodestone_particle::{
    DripKind, DripPhase, Layer, ParticleEngine, ParticleQuad, Sheet, SpriteSource, emit,
};
use lodestone_physics::{CollisionView, Vec3d};
use lodestone_render::{BlockModels, Camera};

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
    /// debris — it renders it near-**white**. See `docs/particles.md`.
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

#[path = "particles/events.rs"]
mod events;
#[path = "particles/lifecycle.rs"]
mod lifecycle;
#[path = "particles/render.rs"]
mod render;

pub use render::ParticleRenderer;
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
