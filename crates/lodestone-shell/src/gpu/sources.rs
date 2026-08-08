//! Polled per-frame render sources: the crate-external wires this render
//! module cannot lay itself (entity light, sky darkening, the world clock,
//! the local player's third-person body, outline shapes, hand-swing progress
//! and the main-hand item). See each type's doc for why the wire lives here
//! rather than being threaded through [`RenderState::render`]'s signature.
use glam::Vec3;

use lodestone_assets::ResourceLocation;
use lodestone_render::{AnimInput, ENTITY_FULLBRIGHT, entity::player_model_name};
use lodestone_model::event::EquipmentSlot;

use crate::entities::EntityDraw;

/// Samples the world's packed sky/block light (`sky << 4 | block`) at an
/// entity's feet, so a mob is lit by the block it stands in exactly as vanilla
/// lights it (`LivingEntityRenderer` → `Level::getLightColor`, one sample per
/// entity).
///
/// Only the shell's `Sim` owns a world to sample, and `RenderState` is handed
/// pre-interpolated `EntityDraw`s with no light on them, so this is the seam
/// between the two. Unset — the offline demo, a headless test — every mob is
/// [`ENTITY_FULLBRIGHT`], which is the behaviour before entity lighting existed.
///
/// The `Fn` is boxed rather than a `fn` pointer because a real sampler has to
/// capture the client handle; the manual [`Debug`] keeps `RenderState`'s derive
/// working.
#[derive(Default)]
pub struct EntityLightSource(pub(super) Option<Box<dyn Fn(Vec3) -> Option<u8> + Send + Sync>>);

impl EntityLightSource {
    /// Packed light at `feet`, or [`ENTITY_FULLBRIGHT`] when there is no sampler
    /// or the position is outside loaded chunks. A `None` here is deliberately
    /// **not** darkness: an unloaded neighbour should not black out a mob, the
    /// same call the particle path makes (`Sim::extract_particles`).
    #[must_use]
    pub(super) fn sample(&self, feet: Vec3) -> u8 {
        self.0
            .as_ref()
            .and_then(|f| f(feet))
            .unwrap_or(ENTITY_FULLBRIGHT)
    }
}

impl std::fmt::Debug for EntityLightSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("EntityLightSource")
            .field(&if self.0.is_some() {
                "set"
            } else {
                "full-bright"
            })
            .finish()
    }
}

/// Where this frame's **sky darkening** comes from: the factor the sky half of
/// the lightmap is scaled by, `1.0` at noon down to `0.24` at midnight.
///
/// Separate from [`EntityLightSource`] because it is a property of the *world
/// clock*, not of a position — one value per frame, not one per mob — and
/// because the server never sends it. A server's sky-light array is
/// time-invariant, so without this term a sky-lit mob is full-bright all night
/// no matter how correctly its light byte was sampled. Measured live: packed
/// `0xF0` and `light_term` `1.000` at both noon and midnight.
///
/// Unset — the offline demo, a headless test — is `1.0`, i.e. permanent noon and
/// exactly the behaviour before this existed.
#[derive(Default)]
pub struct SkyDarkenSource(pub(super) Option<Box<dyn Fn() -> Option<f32> + Send + Sync>>);

impl SkyDarkenSource {
    /// This frame's factor, or `1.0` when there is no source or the world clock
    /// is not known yet (pre-login). Clamped into vanilla's `[0.24, 1.0]`: a
    /// source that hands back garbage should look like a wrong time of day, not
    /// like a black or blown-out frame.
    #[must_use]
    pub(super) fn value(&self) -> f32 {
        self.0
            .as_ref()
            .and_then(|f| f())
            .map_or(1.0, |v| v.clamp(0.24, 1.0))
    }
}

impl std::fmt::Debug for SkyDarkenSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("SkyDarkenSource")
            .field(&if self.0.is_some() { "set" } else { "noon" })
            .finish()
    }
}

/// Where the sky pass's **world clock** comes from: the raw `time_of_day`
/// tick [`lodestone_render::SkyRenderer::render`] places the sun/moon from and
/// phases the star/cloud animation with.
///
/// Separate from [`SkyDarkenSource`] even though both are driven by the same
/// server clock: that source hands back the already-*derived* darken factor
/// (`sky_darken_for_time_of_day`) the entity/model passes fold into their
/// lightmap lane, while the sky pass needs the raw tick itself — placing the
/// sun at a fixed factor of 1.0 would freeze it at noon's position forever.
///
/// Unset — no sky installed, a headless test — is noon (`6000`), matching
/// every other per-frame source in this file's "unset means noon" convention.
#[derive(Default)]
pub(super) struct TimeOfDaySource(pub(super) Option<Box<dyn Fn() -> Option<i64> + Send + Sync>>);

impl TimeOfDaySource {
    /// This frame's `time_of_day`, or noon (`6000`) when there is no source or
    /// the world clock is not known yet (pre-login).
    #[must_use]
    pub(super) fn value(&self) -> i64 {
        self.0.as_ref().and_then(|f| f()).unwrap_or(6000)
    }
}

impl std::fmt::Debug for TimeOfDaySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("TimeOfDaySource")
            .field(&if self.0.is_some() { "set" } else { "noon" })
            .finish()
    }
}

/// The local player's own third-person body for one frame: everything
/// [`EntityInstance::new`](lodestone_render::EntityInstance::new) (via
/// [`RenderState::prepare_entities`]) needs to pose it, plus which skin rig to
/// draw it with.
///
/// This is deliberately *not* an [`EntityDraw`] itself, even though
/// [`Self::into_draw`] immediately turns it into one. The local player is not
/// a tracked network entity — `EntityInterpolator` in `entities.rs` never
/// sees it, on purpose, the same fact [`RenderState::prepare_first_person_arm`]
/// already documents ("`render` receives only `&[EntityDraw]`, and the local
/// player is not in it"). A caller building one of these supplies raw local
/// state (feet, body yaw, an [`AnimInput`]) rather than pretending to be a
/// server snapshot.
///
/// # Why this reuses `EntityDraw` at all, rather than a parallel draw path
///
/// The first-person arm needed a genuinely separate pose function
/// ([`first_person_arm_pose`]) because vanilla draws the arm *part* from its
/// rest pose with one hand-picked rotation, and puts the swing in the
/// camera-space chain the rested arm hangs off — never in the animated
/// `setupAnim` result a third-person view needs. The body has no
/// such divergence: vanilla's own third-person player renderer is just
/// `PlayerModel`/`HumanoidModel.setupAnim`, exactly what
/// [`lodestone_render::entity_anim::Skeleton::pose`] already computes for
/// every other humanoid mob.
///
/// The two therefore share the swing *scalar* ([`AnimInput::attack_anim`], which
/// is the same `Sim::hand_swing_progress` the arm pass polls) and no code at all.
/// Feeding either pose function the other's chain produces a plausible-looking
/// wrong arm, which is why they stay apart. Reusing [`EntityDraw`] means the local player's
/// body goes through the *exact* resolve → cull → pose → upload path
/// ([`RenderState::prepare_entities`]) and the *exact* held-item path
/// ([`RenderState::merge_held_items`]) every zombie and every remote player
/// already does, instead of a second copy of either.
#[derive(Debug, Clone, PartialEq)]
pub struct ThirdPersonBodyState {
    /// Feet position in world space — the same quantity [`EntityDraw::feet`]
    /// carries for a network entity.
    pub feet: Vec3,
    /// Body yaw in degrees (Minecraft convention: `0` faces `+Z`).
    pub body_yaw_deg: f32,
    /// Per-part animation drive: head look, walk cycle, idle age. Build this
    /// the way `entities.rs`'s `Track::render_anim` builds it for a network
    /// entity — `head_yaw_deg`/`head_pitch_deg` **relative to the body**
    /// (matching [`AnimInput::head_yaw_deg`]'s contract), `limb_swing`/
    /// `limb_swing_amount` from the local player's own per-tick travel
    /// distance through the same `WalkAnimation` shape entities.rs already
    /// has, so a walking self-avatar animates identically to a walking
    /// remote one.
    pub anim: AnimInput,
    /// Uniform render scale. `1.0` for a normal adult.
    pub scale: f32,
    /// Which rig to draw: `true` for `player_slim` ("Alex" arms), `false` for
    /// `player_wide` ("Steve" arms) — see [`player_model_name`]. Nothing in
    /// this codebase decodes real skin-model data yet (the same gap
    /// [`RenderState::prepare_first_person_arm`] already notes for the arm),
    /// so every caller has to pick a value today; `false` reproduces the
    /// arm's existing default.
    pub slim: bool,
    /// What the local player is holding/wearing, in the shape
    /// [`EntityDraw::equipment`] carries: main hand, off hand, and all four
    /// armour slots (head/chest/legs/feet), the same as any other entity's
    /// `EntityDraw::equipment`.
    pub equipment: Vec<(EquipmentSlot, ResourceLocation)>,
}

/// A reserved id for [`ThirdPersonBodyState::into_draw`]'s synthetic
/// [`EntityDraw`]. Real entity ids are server-assigned and never negative
/// (`v770`'s entity id is a non-negative `VarInt`), so this can never collide
/// with a tracked network entity.
pub(super) const LOCAL_PLAYER_DRAW_ID: i32 = -1;

impl ThirdPersonBodyState {
    /// Bridge into the [`EntityDraw`] shape [`RenderState::prepare_entities`]
    /// and [`RenderState::prepare_item_geometry`] already know how to
    /// resolve, cull, pose, and (for equipment) hang an item off of.
    /// `type_path` is [`player_model_name`]'s output — a literal
    /// `"player_wide"`/`"player_slim"`, which
    /// `lodestone_render::entity::canonical_model_name` resolves through its
    /// corpus-name fallback with no new plumbing on the render side.
    pub(super) fn into_draw(self) -> EntityDraw {
        EntityDraw {
            // The local player's own body never reddens today, for the same
            // reason its `on_fire` overlay cannot: the local player has no
            // ingest entity carrying `HurtTime` (`apply_local_player_login`
            // gives it no `EntityKind`/`Position`), so there is nothing to read
            // — and with no third-person camera there is nothing to see either.
            // A `false` by construction, not by omission; see `docs/combat.md`.
            hurt: false,
            id: LOCAL_PLAYER_DRAW_ID,
            type_path: player_model_name(self.slim).to_string(),
            item: None,
            equipment: self.equipment,
            // The local player's own dye colours are not plumbed to
            // `ThirdPersonBodyState` yet — a separate gap from the network
            // path's, since this body's armour comes from the player's own
            // inventory slots (`sim.rs`'s `ARMOUR_NATIVE_SLOTS`), not
            // `EntitySnapshot::equipment_dye`. Empty by construction, not
            // omission: our own third-person leather armour draws undyed
            // until that source is wired too.
            equipment_dye: Vec::new(),
            equipment_trim: Vec::new(),
            feet: self.feet,
            yaw: self.body_yaw_deg,
            // Absolute head yaw, for API parity with a network `EntityDraw`
            // (see that field's doc comment) — nothing in `gpu.rs` actually
            // reads it back; only `anim.head_yaw_deg` (relative) feeds the
            // pose.
            head_yaw: self.body_yaw_deg + self.anim.head_yaw_deg,
            pitch: self.anim.head_pitch_deg,
            scale: self.scale,
            anim: self.anim,
            // The local player is neither a sheep nor a dropped item, so both of
            // these are their neutral values by construction rather than by
            // omission: `wool` is `None` for every non-`sheep` type path per
            // `entities::sheep_wool`'s gate, and `count` is meaningless when
            // `item` is `None`.
            wool: None,
            count: 1,
            foil: false,
            // The local third-person body does not draw its own nametag
            // (issue #100 scope: other entities/players, not the camera's
            // own body) — a deliberate gap, not an oversight; see
            // `docs/entity-nametags.md`.
            name_tag: None,
            // **The one place a variant is still flattened**, and the reason our
            // own bow draws slack while every remote player's and every mob's
            // does not. `ItemUse` is an *ingest* component and the local player
            // has no ingest entity, so this needs a session-level fold of the
            // same shape as `Vitals` — see `docs/item-variants.md` for the exact
            // three lines. `None` rather than a guess: the resolver then takes
            // `on_false` and draws the resting model, which is what shipped
            // before the variant axis existed.
            item_use: None,
            // Not a creeper: only a creeper ever swells.
            creeper_swelling: 0.0,
            // The local player's own body cannot report `on_fire` either, for
            // the same reason `hurt` above cannot: no ingest entity, hence no
            // `EntityFlags` to read (issue #434). `false` by construction, not
            // by omission.
            on_fire: false,
        }
    }
}

/// Where this frame's third-person self-body comes from, polled once per
/// frame exactly like [`EntityLightSource`]/[`SkyDarkenSource`].
///
/// There is no separate "camera mode" enum here on purpose: `f` returning
/// `None` **is** first person, and `Some` **is** third person, so the
/// caller's own camera-mode state is the only source of truth and this
/// module never has to be told about it directly. Unset — the default, and
/// every frame until a caller installs a source — reproduces exactly the
/// behaviour before this existed: the first-person arm draws unconditionally
/// and no extra entity is added to the frame. See
/// [`RenderState::set_third_person_body_source`].
/// Polled source for the targeted block's real outline shape, in world space.
///
/// Same idiom as [`ThirdPersonBodySource`]: the renderer cannot reach the
/// collision view, and threading it through [`RenderState::render`] would touch
/// every caller. Unset (the default) draws a unit cube, which is correct for the
/// demo palette and for any adapter with no outline census.
#[derive(Default)]
pub struct OutlineShapeSource(
    #[allow(clippy::type_complexity)]
    pub(super) Option<Box<dyn Fn([i32; 3]) -> Vec<lodestone_physics::Aabb> + Send + Sync>>,
);

impl OutlineShapeSource {
    #[must_use]
    pub(super) fn sample(&self, block: [i32; 3]) -> Vec<lodestone_physics::Aabb> {
        self.0.as_ref().map(|f| f(block)).unwrap_or_default()
    }
}

#[derive(Default)]
pub struct ThirdPersonBodySource(
    pub(super) Option<Box<dyn Fn() -> Option<ThirdPersonBodyState> + Send + Sync>>,
);

impl ThirdPersonBodySource {
    #[must_use]
    pub(super) fn sample(&self) -> Option<ThirdPersonBodyState> {
        self.0.as_ref().and_then(|f| f())
    }
}

/// Where this frame's first-person **arm-swing progress** comes from, polled once
/// per frame like [`SkyDarkenSource`] / [`EntityLightSource`].
///
/// The value is vanilla's `attackValue` — `Player.getAttackAnim(partialTick)`, in
/// `0.0..=1.0`, already interpolated for this frame's sub-tick alpha. It must come
/// from a **tick** clock read with a partial tick, not from anything derived from
/// frame time: `lodestone_entity::pose::EntityPose` advances the swing in
/// [`tick`](lodestone_entity::pose::EntityPose::tick) and interpolates in
/// [`attack_anim_lerp`](lodestone_entity::pose::EntityPose::attack_anim_lerp), and
/// `Sim::hand_swing_progress` is that pairing. Driving a swing per frame is the
/// defect `entities.rs`'s `limb_swing_tracks_per_tick_travel_not_the_interpolation_gap`
/// records for the walk cycle, where the phase ran up to 3x too fast and the
/// animation speed became frame-rate dependent.
///
/// Unset — the default, the offline demo, every headless test — is `0.0`, a fully
/// rested arm, which reproduces exactly the behaviour before the swing existed.
#[derive(Default)]
pub struct HandSwingSource(pub(super) Option<Box<dyn Fn() -> f32 + Send + Sync>>);

impl HandSwingSource {
    /// This frame's swing progress, clamped into `0.0..=1.0`. A source handing
    /// back a garbage or out-of-range value should look like the wrong moment of
    /// a swing, never like an arm flung off screen —
    /// [`lodestone_render::entity::first_person_arm_chain`]'s shaping functions
    /// are periodic, so extrapolating past `1.0` silently animates something else.
    #[must_use]
    pub(super) fn value(&self) -> f32 {
        // `clamp` panics on a NaN bound and *propagates* a NaN value, so NaN is
        // mapped to rest explicitly rather than left to reach a matrix.
        match self.0.as_ref().map(|f| f()) {
            Some(v) if v.is_finite() => v.clamp(0.0, 1.0),
            _ => 0.0,
        }
    }
}

impl std::fmt::Debug for HandSwingSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("HandSwingSource")
            .field(&if self.0.is_some() { "set" } else { "rest" })
            .finish()
    }
}

/// Where the **local player's main-hand item** comes from, polled once per frame
/// like [`HandSwingSource`].
///
/// This exists because `render` receives only `&[EntityDraw]` and the local player
/// is not in it — the same fact [`ThirdPersonBodySource`] and [`HandSwingSource`]
/// exist for. Everything else in the first-person path was already present:
/// [`lodestone_render::entity::first_person_item_mesh`] poses the geometry,
/// `DisplaySlot::FirstPersonRightHand` is selected by
/// [`Arm::display_slot`](lodestone_render::entity::Arm::display_slot), and
/// `BlockModels::items` has carried flat-sprite geometry since the extrusion
/// landed. The **only** missing link was that nothing told the renderer what the
/// player was holding.
///
/// The value is the item id **plus the enchantment-foil flag** (issue #452): the
/// held item's glint second pass is gated on it, so the source has to carry it
/// from the hotbar record that already computed it (`app/redraw.rs` builds it
/// from `stack_has_foil`), rather than re-derive it here where there is no stack.
///
/// Unset — the default, the offline demo, every headless test that does not opt in
/// — yields `None`, which draws the bare arm: exactly vanilla's empty-hand branch
/// and exactly the behaviour before this existed.
#[derive(Default)]
pub struct MainHandSource(
    #[allow(clippy::type_complexity)]
    pub(super)
    Option<Box<dyn Fn() -> Option<(lodestone_assets::ResourceLocation, bool)> + Send + Sync>>,
);

impl MainHandSource {
    #[must_use]
    pub(super) fn value(&self) -> Option<(lodestone_assets::ResourceLocation, bool)> {
        self.0.as_ref().and_then(|f| f())
    }
}

impl std::fmt::Debug for MainHandSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("MainHandSource")
            .field(&if self.0.is_some() { "set" } else { "empty" })
            .finish()
    }
}

impl std::fmt::Debug for OutlineShapeSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("OutlineShapeSource")
            .field(&if self.0.is_some() {
                "real-outline"
            } else {
                "unit-cube"
            })
            .finish()
    }
}

impl std::fmt::Debug for ThirdPersonBodySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ThirdPersonBodySource")
            .field(&if self.0.is_some() {
                "set"
            } else {
                "first-person"
            })
            .finish()
    }
}

/// Where this frame's block entities (chests, issue #23) come from.
///
/// A `Fn(Vec3) -> Vec<ChestSpawn>` taking the **camera position**, because
/// vanilla's own gate is per-block-entity distance from the camera
/// (`BlockEntityRenderer.shouldRender`, a flat 64 blocks against the block
/// *centre*) and the cheapest place to apply it is where the world is being
/// walked, not after a `Vec` of every chest in the world has been built.
///
/// Re-installed **every frame** rather than once at connect, like
/// [`MainHandSource`] and unlike [`EntityLightSource`]: a chest lid is
/// partial-tick-interpolated, and a closure captured once would freeze the
/// animation at whatever fraction of a tick it was installed on.
///
/// Unset — the offline demo, every headless test that does not opt in — yields an
/// empty vec, which draws nothing and reproduces this struct's behaviour before
/// block entities existed exactly.
#[derive(Default)]
pub struct BlockEntitySource(
    #[allow(clippy::type_complexity)]
    pub(super)  Option<
        Box<dyn Fn(glam::Vec3) -> Vec<lodestone_render::ChestSpawn> + Send + Sync>,
    >,
);

impl BlockEntitySource {
    #[must_use]
    pub(super) fn chests(&self, eye: glam::Vec3) -> Vec<lodestone_render::ChestSpawn> {
        self.0.as_ref().map(|f| f(eye)).unwrap_or_default()
    }
}

impl std::fmt::Debug for BlockEntitySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("BlockEntitySource")
            .field(&if self.0.is_some() { "set" } else { "empty" })
            .finish()
    }
}

/// Where this frame's skull/head block entities come from — same shape as
/// [`BlockEntitySource`], kept as an independent source rather than folded into
/// its closure's return type because chests and skulls are gathered by different
/// functions (`crate::block_entities::{chest_spawns, skull_spawns}`) with no
/// shared per-frame state: a skull has no lid-style animation clock, so it needs
/// no partial-tick capture.
#[derive(Default)]
pub struct SkullSource(
    #[allow(clippy::type_complexity)]
    pub(super)  Option<Box<dyn Fn(glam::Vec3) -> Vec<lodestone_render::SkullSpawn> + Send + Sync>>,
);

impl SkullSource {
    /// This frame's skulls, or none when unset — the same "unset means draw
    /// nothing" convention [`BlockEntitySource`] uses.
    #[must_use]
    pub(super) fn skulls(&self, eye: glam::Vec3) -> Vec<lodestone_render::SkullSpawn> {
        self.0.as_ref().map(|f| f(eye)).unwrap_or_default()
    }
}

impl std::fmt::Debug for SkullSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("SkullSource")
            .field(&if self.0.is_some() { "set" } else { "empty" })
            .finish()
    }
}

/// Where this frame's bells come from — same shape as [`SkullSource`], again
/// an independent source rather than folding a third spawn type into an
/// existing closure's return type: `crate::block_entities::bell_spawns` is
/// gathered by its own function with no state shared with chests or skulls.
///
/// **Unlike [`BlockEntitySource`]'s lid clock, nothing here needs
/// re-installing for a partial tick.** [`crate::block_entities::bell_spawn`]
/// always resolves [`lodestone_render::BellSpawn::shake`] to `None` today —
/// the `BLOCK_EVENT` shake trigger is not wired from any gather in this
/// workspace yet (see `docs/block-entity-renderers.md`'s Bell section) — so
/// a bell always draws at rest. That is a real, tracked gap, not a design
/// choice of this source: the day a shake-tick clock lands (its own map,
/// alongside [`crate::block_entities::ChestLids`], per that module's own
/// "How to change it" note), this source's contract does not need to change,
/// only the closure it is given.
#[derive(Default)]
pub struct BellSource(
    #[allow(clippy::type_complexity)]
    pub(super)  Option<Box<dyn Fn(glam::Vec3) -> Vec<lodestone_render::BellSpawn> + Send + Sync>>,
);

impl BellSource {
    /// This frame's bells, or none when unset — the same "unset means draw
    /// nothing" convention [`BlockEntitySource`] uses.
    #[must_use]
    pub(super) fn bells(&self, eye: glam::Vec3) -> Vec<lodestone_render::BellSpawn> {
        self.0.as_ref().map(|f| f(eye)).unwrap_or_default()
    }
}

impl std::fmt::Debug for BellSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("BellSource")
            .field(&if self.0.is_some() { "set" } else { "empty" })
            .finish()
    }
}

/// Where this frame's shulker boxes come from — same shape as [`SkullSource`],
/// and the thinnest of the family: the closure needs no partial tick and no
/// animation map at all, because a shulker box's whole appearance is a function
/// of its block state (issue #23).
#[derive(Default)]
pub struct ShulkerSource(
    #[allow(clippy::type_complexity)]
    pub(super)  Option<Box<dyn Fn(glam::Vec3) -> Vec<lodestone_render::ShulkerSpawn> + Send + Sync>>,
);

impl ShulkerSource {
    /// This frame's shulker boxes, or none when unset.
    #[must_use]
    pub(super) fn shulkers(&self, eye: glam::Vec3) -> Vec<lodestone_render::ShulkerSpawn> {
        self.0.as_ref().map(|f| f(eye)).unwrap_or_default()
    }
}

impl std::fmt::Debug for ShulkerSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ShulkerSource")
            .field(&if self.0.is_some() { "set" } else { "empty" })
            .finish()
    }
}

/// Where this frame's sign text comes from — same shape as [`SkullSource`],
/// again an independent source rather than a shared return type:
/// `crate::block_entities::sign_spawns` reads a different half of a block
/// entity's record than either chest or skull (the NBT, not just the block
/// state), but the gather-and-install contract is identical, so the source
/// itself does not need to know that.
#[derive(Default)]
pub struct SignSource(
    #[allow(clippy::type_complexity)]
    pub(super)  Option<Box<dyn Fn(glam::Vec3) -> Vec<lodestone_render::SignSpawn> + Send + Sync>>,
);

impl SignSource {
    /// This frame's signs, or none when unset — the same "unset means draw
    /// nothing" convention [`BlockEntitySource`] uses.
    #[must_use]
    pub(super) fn signs(&self, eye: glam::Vec3) -> Vec<lodestone_render::SignSpawn> {
        self.0.as_ref().map(|f| f(eye)).unwrap_or_default()
    }
}

impl std::fmt::Debug for SignSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("SignSource")
            .field(&if self.0.is_some() { "set" } else { "empty" })
            .finish()
    }
}

/// Where this frame's filled-map pictures come from (issue #184).
///
/// Unlike the block-entity sources this takes a **map id** rather than an eye
/// position, because a map is keyed by id and not by where it is: the same map
/// can be in a hand and in three item frames at once. `None` asks for the
/// lowest-numbered known map — see `Sim::map_source` for why that fallback exists
/// and what removes it.
///
/// Unset yields no picture and nothing draws, which is the behaviour before maps
/// rendered at all.
#[derive(Default)]
pub struct MapSource(
    #[allow(clippy::type_complexity)]
    pub(super)  Option<Box<dyn Fn(Option<i32>) -> Option<Vec<u8>> + Send + Sync>>,
);

impl MapSource {
    /// One map's raw 128×128 packed colour grid, or none when unset or unknown.
    #[must_use]
    pub(super) fn picture(&self, id: Option<i32>) -> Option<Vec<u8>> {
        self.0.as_ref().and_then(|f| f(id))
    }
}

impl std::fmt::Debug for MapSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("MapSource")
            .field(&if self.0.is_some() { "set" } else { "empty" })
            .finish()
    }
}

/// Where this frame's banners come from (issue #23) — the same shape as
/// [`ShulkerSource`], but the closure must be re-installed every frame for
/// [`BellSource`]'s reason: it captures the game tick and the partial tick, and a
/// stale one freezes every banner's sway.
#[derive(Default)]
pub struct BannerSource(
    #[allow(clippy::type_complexity)]
    pub(super)  Option<Box<dyn Fn(glam::Vec3) -> Vec<lodestone_render::BannerSpawn> + Send + Sync>>,
);

impl BannerSource {
    /// This frame's banners, or none when unset.
    #[must_use]
    pub(super) fn banners(&self, eye: glam::Vec3) -> Vec<lodestone_render::BannerSpawn> {
        self.0.as_ref().map(|f| f(eye)).unwrap_or_default()
    }
}

impl std::fmt::Debug for BannerSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("BannerSource")
            .field(&if self.0.is_some() { "set" } else { "empty" })
            .finish()
    }
}
