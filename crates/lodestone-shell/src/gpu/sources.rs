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

/// Samples the world's packed sky/block light (`sky << 4 | block`) at **an
/// arbitrary world position** — one sample per entity, and the caller decides
/// where.
///
/// This is deliberately position-agnostic. It used to document itself as
/// sampling "an entity's feet … exactly as vanilla lights it", and that was
/// wrong on both halves: vanilla probes at the entity's **eye**
/// (`EntityRenderer.getPackedLightCoords` → `Entity.getLightProbePosition` →
/// `getEyePosition`), and it forces the block half to 15 for a burning entity
/// (`EntityRenderer.getBlockLightLevel`). Both of those belong to the caller,
/// because both depend on the entity rather than on the world — see
/// `super::entity_passes::entity_light`, which is where every entity pass gets
/// its light and the only place either rule is applied. The first-person arm
/// samples at `camera.position`, which already *is* the eye, and needs neither.
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
    /// Packed light at `probe`, or [`ENTITY_FULLBRIGHT`] when there is no sampler
    /// or the position is outside loaded chunks. A `None` here is deliberately
    /// **not** darkness: an unloaded neighbour should not black out a mob, the
    /// same call the particle path makes (`Sim::extract_particles`).
    ///
    /// `probe` is a world position and the sampler floors it into a block cell,
    /// which is vanilla's `BlockPos.containing` — so an eye-height offset is
    /// added *before* this call and truncated once, here.
    #[must_use]
    pub(super) fn sample(&self, probe: Vec3) -> u8 {
        self.0
            .as_ref()
            .and_then(|f| f(probe))
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

/// Where this frame's per-dimension **ambient light colour** comes from:
/// `EnvironmentAttributes.AMBIENT_LIGHT_COLOR`, the floor `lightmap.fsh` seeds
/// its accumulator with before either light half is added — see
/// `lodestone_render::light::light_color_from_levels`'s `ambient` parameter
/// and `DimensionType::ambient_light_color` for the wire source.
///
/// Separate from [`SkyDarkenSource`] because it is a property of the current
/// *dimension type*, not of the clock: it changes only on a portal trip, not
/// every frame, and the server sends it once, during Configuration, in
/// `registry_data` — there is nothing to poll beyond "which dimension is the
/// player in right now".
///
/// Unset — the offline demo, a headless test, pre-login, or a dimension whose
/// registry entry omitted the attribute — reads as the overworld's own colour
/// ([`lodestone_render::light::OVERWORLD_AMBIENT_LIGHT`]), i.e. exactly the
/// behaviour before per-dimension colour existed. That default never
/// brightens a dimension that should be dimmer, only a genuinely-unknown one
/// — the same "never invent light" rule `lodestone_data::light_props::emission`
/// follows for an unresolved block-state id.
#[derive(Default)]
pub struct AmbientLightSource(pub(super) Option<Box<dyn Fn() -> Option<[f32; 3]> + Send + Sync>>);

impl AmbientLightSource {
    /// This frame's ambient colour, or the overworld's own when there is no
    /// source or the current dimension is not known yet.
    #[must_use]
    pub(super) fn value(&self) -> [f32; 3] {
        self.0
            .as_ref()
            .and_then(|f| f())
            .unwrap_or(lodestone_render::light::OVERWORLD_AMBIENT_LIGHT)
    }
}

impl std::fmt::Debug for AmbientLightSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("AmbientLightSource")
            .field(&if self.0.is_some() {
                "set"
            } else {
                "overworld-default"
            })
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
    /// `Mth.lerp(partialTick, swimAmountO, swimAmount)` — the body-pitch
    /// ramp `gpu::entity_passes::apply_swim_rotation` reads to tip the whole
    /// body toward horizontal while swimming, **not**
    /// [`AnimInput::swim_amount`] on [`Self::anim`] (that one drives the
    /// arm-stroke pose only). `sim/camera.rs::third_person_body_state` fills
    /// this from the local player's own physics-integrated
    /// `PlayerState::swim_amount`/`swim_amount_o`. This used to have no
    /// source at all — [`Self::into_draw`] hardcoded `EntityDraw::swim_amount`
    /// to `0.0` — which is why a remote player's body leaned into a swim and
    /// the local player's own body stood bolt upright doing the same stroke.
    pub swim_amount: f32,
    /// Which rig to draw: `true` for `player_slim` ("Alex" arms), `false` for
    /// `player_wide` ("Steve" arms) — see [`player_model_name`].
    /// `sim/camera.rs::third_person_body_state` fills this from
    /// `crate::skin_fetch::current_model`, the same signed-in-profile fetch
    /// the inventory avatar already draws. The first-person arm
    /// (`RenderState::prepare_first_person_hand`, in `gpu/first_person.rs`)
    /// is a separate pass and still draws `player_wide` unconditionally —
    /// see that method's own doc.
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
            // And the same construction, for the same reason, keeps the local
            // player's own body from toppling: no ingest entity means no
            // `DeathTime` to read. The local player's death is drawn by
            // `camera_rig`'s *camera* roll (`GameRenderer.bobHurt`, a different
            // vanilla expression on the same tick count) rather than by tipping
            // this body over, which is also what vanilla does in first person.
            death_time: 0.0,
            id: LOCAL_PLAYER_DRAW_ID,
            type_path: std::sync::Arc::from(player_model_name(self.slim)),
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
            block_state: None,
            // Meaningless for the same reason `count` is just above: `item` is
            // always `None` for the local player's own body draw, which never
            // represents a dropped item or a thrown projectile.
            item_dyed_color: None,
            item_potion_color: None,
            // The local player is not an experience orb either — `None` is what
            // stops `prepare_orbs` claiming our own body as one.
            experience_orb_value: None,
            count: 1,
            foil: false,
            // The local third-person body does not draw its own nametag
            // (scope: other entities/players, not the camera's
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
            // A `Player`'s main-arm setting is a client-side option synced on
            // its own metadata byte, separate from `Mob.DATA_MOB_FLAGS_ID`
            // (which only exists on `Mob`, not `Player`), and this build does
            // not decode it — so the local player's own body always draws
            // right-handed. Not a guess in the `MobFlags` sense: there is no
            // decoded bit to be wrong about, unlike `item_use` above.
            main_arm_left: false,
            // Not a creeper: only a creeper ever swells.
            creeper_swelling: 0.0,
            // The local player's own swim ramp, now plumbed from
            // `ThirdPersonBodyState::swim_amount` (see that field's doc) —
            // `sim/camera.rs::third_person_body_state` reads it straight off
            // `PlayerState::swim_amount`/`swim_amount_o`, the same
            // physics-integrated value `AnimInput::swim_amount` already used
            // for the arm stroke, rather than the `SwimRamp`/`tick_swim_ramp`
            // reconstruction `entities.rs` needs for a *remote* player (whose
            // ingest entity carries a synced `Pose` and nothing else).
            swim_amount: self.swim_amount,
            // The local player's own body cannot report `on_fire` either, for
            // the same reason `hurt` above cannot: no ingest entity, hence no
            // `EntityFlags` to read. `false` by construction, not by omission.
            on_fire: false,
            // Same reasoning as `on_fire`: no ingest entity, hence no
            // `EntityFlags` to read invisible off — and the local player is
            // never an armour stand.
            invisible: false,
            armor_stand: None,
            // **The rig is already chosen** — `type_path` above is
            // `player_model_name(self.slim)` — so this field would be redundant
            // for the rig and is only ever the *sheet*. `None` therefore means
            // "our own third-person body still draws the pack's default sheet",
            // which is the gap `docs/player-skins.md` records: our own fetched
            // skin reaches the inventory avatar, not this body. `ThirdPersonBodyState`
            // is where the URL would have to be plumbed.
            player_skin: None,
            variant_sheet: None,
            // No cape reaches this draw for the same reason `player_skin`
            // above is `None`: nothing plumbs our own fetched cape URL to
            // the third-person body, so there is nothing to sway even if
            // this were non-zero.
            cape_sway: (0.0, 0.0, 0.0),
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

/// This frame's in-progress eat or drink, for
/// `ItemInHandRenderer.applyEatTransform` — `(currUsageTime, useDuration)`, already
/// interpolated with the frame's partial tick.
///
/// # Why the *interpolated* usage time and not `(ticks, partial)`
///
/// Vanilla's `applyEatTransform` computes `currUsageTime` once
/// (`getUseItemRemainingTicks() - frameInterp + 1.0F`) and then uses it for both the
/// bob's phase and the jiggle's fraction. Handing the renderer the combined value
/// keeps that single derivation
/// ([`eat_usage_time`](lodestone_render::entity::eat_usage_time)) in one place; two
/// fields would let the phase and the fraction be assembled differently and be one
/// tick apart, which shows up as a bob that never quite reaches its peak.
///
/// Like every other source here it must be re-installed **every frame**, because it
/// carries a partial-tick interpolation. Unset — the default, the demo, every
/// headless test — is `None`, which is the plain held-item pose: exactly the
/// behaviour before eating animated.
#[derive(Default)]
pub struct ItemUseSource(pub(super) Option<Box<dyn Fn() -> Option<(f32, u32)> + Send + Sync>>);

impl ItemUseSource {
    /// This frame's `(currUsageTime, useDuration)`, or `None` when nothing is being
    /// consumed.
    ///
    /// A non-finite usage time is mapped to `None` rather than clamped: unlike a
    /// swing, there is no sensible "moment of an eat" to fall back to, and a NaN
    /// reaching `Math.pow` produces a NaN matrix and an item that vanishes. A zero
    /// duration is refused for the same reason — it is the divisor.
    #[must_use]
    pub(super) fn sample(&self) -> Option<(f32, u32)> {
        match self.0.as_ref().and_then(|f| f()) {
            Some((usage, duration)) if usage.is_finite() && duration > 0 => Some((usage, duration)),
            _ => None,
        }
    }
}

impl std::fmt::Debug for ItemUseSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ItemUseSource")
            .field(&if self.0.is_some() { "set" } else { "unset" })
            .finish()
    }
}

/// What the local player is holding in the main hand, for
/// [`MainHandSource`]/[`super::first_person::HeldItemEquip`].
///
/// A named struct rather than growing the old `(ResourceLocation, bool)` tuple
/// to four elements — [`Self::dyed_color`]/[`Self::potion_color`] are the same
/// pair `lodestone_shell::hud::HotbarSlot` already carries (see that type's
/// doc), threaded here so the first-person hand can resolve a dyed leather
/// item's or a mixed potion's real tint instead of the item definition's plain
/// default — the gap `lodestone_render::stamp_live_item_tint`'s own doc names
/// as the first-person half of the fix `sprite_layer_tint` landed for the GUI.
#[derive(Debug, Clone, PartialEq)]
pub struct MainHandItem {
    /// The held item's id.
    pub item: lodestone_assets::ResourceLocation,
    /// Whether the stack is enchanted — the glint second-pass gate.
    pub foil: bool,
    /// The stack's `minecraft:dyed_color`, straight off
    /// `lodestone_game::item::ItemStack::dyed_color`. `None` for an undyed
    /// stack or any non-dyeable item.
    pub dyed_color: Option<u32>,
    /// The stack's already-mixed `minecraft:potion_contents` colour, straight
    /// off `lodestone_game::item::ItemStack::potion_color`. `None` for a
    /// non-potion item or one with no potion contents.
    pub potion_color: Option<u32>,
    /// The stack's `minecraft:banner_patterns`, straight off
    /// `lodestone_game::item::ItemStack::banner_patterns`. Empty for every
    /// non-banner item and for a plain banner carrying no loom patterns.
    pub banner_patterns: Vec<lodestone_model::BannerPatternLayer>,
    /// The stack's `minecraft:base_color`, straight off
    /// `lodestone_game::item::ItemStack::base_color`. `None` for a
    /// never-dyed shield and for every non-shield item.
    pub base_color: Option<String>,
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
/// The value is a [`MainHandItem`]: the item id, the enchantment-foil flag (the
/// held item's glint second pass is gated on it), and its dye/potion colour —
/// all four sourced from the hotbar record that already computed them
/// (`app/redraw.rs` builds it from the same `HotbarSlot` the HUD draws),
/// rather than re-derived here where there is no stack.
///
/// Unset — the default, the offline demo, every headless test that does not opt in
/// — yields `None`, which draws the bare arm: exactly vanilla's empty-hand branch
/// and exactly the behaviour before this existed.
#[derive(Default)]
pub struct MainHandSource(
    #[allow(clippy::type_complexity)]
    pub(super)
    Option<Box<dyn Fn() -> Option<MainHandItem> + Send + Sync>>,
);

impl MainHandSource {
    #[must_use]
    pub(super) fn value(&self) -> Option<MainHandItem> {
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

/// Where this frame's block entities (chests, that fix) come from.
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

/// Where this frame's spawner/trial-spawner display mobs come from — same
/// shape as [`BellSource`]: an independent source, since
/// `crate::block_entities::spawner_mob_spawns` shares no state with any of
/// the block-entity families above it and is not itself a
/// [`BlockEntitySource`] consumer (it feeds the ordinary mob `EntityPipeline`
/// batch, not [`lodestone_render::BlockEntityModelSet`] — see
/// `gpu/entity_passes.rs`'s `prepare_spawner_mobs`).
///
/// **Needs re-installing every frame**, exactly like [`BellSource`]: the
/// spin/oSpin pair lives in `crate::block_entities::SpawnerSpins`, ticked
/// once per client tick, and a stale install freezes every cage's spin at
/// whatever partial-tick fraction the closure was built on.
#[derive(Default)]
pub struct SpawnerSource(
    #[allow(clippy::type_complexity)]
    pub(super)  Option<Box<dyn Fn(glam::Vec3) -> Vec<lodestone_render::SpawnerMobSpawn> + Send + Sync>>,
);

impl SpawnerSource {
    /// This frame's spawner display mobs, or none when unset.
    #[must_use]
    pub(super) fn spawner_mobs(&self, eye: glam::Vec3) -> Vec<lodestone_render::SpawnerMobSpawn> {
        self.0.as_ref().map(|f| f(eye)).unwrap_or_default()
    }
}

impl std::fmt::Debug for SpawnerSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("SpawnerSource")
            .field(&if self.0.is_some() { "set" } else { "empty" })
            .finish()
    }
}

/// Where this frame's beacon beams come from — same "unset means draw
/// nothing" convention as [`SkullSource`]. Unlike every other animated
/// source in this file, the closure behind this needs **no** per-position
/// tracker alongside it: `levels`/`beamSections` are pure functions of
/// current block state (`Sim::beacon_source`'s doc explains why — the same
/// client-side block-entity ticker vanilla itself runs), so there is
/// nothing to advance in `Sim::step`, only current world state to read
/// fresh each frame.
#[derive(Default)]
pub struct BeaconSource(
    #[allow(clippy::type_complexity)]
    pub(super)  Option<Box<dyn Fn(glam::Vec3) -> Vec<lodestone_render::BeaconSpawn> + Send + Sync>>,
);

impl BeaconSource {
    /// This frame's beacon beams, or none when unset.
    #[must_use]
    pub(super) fn beacons(&self, eye: glam::Vec3) -> Vec<lodestone_render::BeaconSpawn> {
        self.0.as_ref().map(|f| f(eye)).unwrap_or_default()
    }
}

impl std::fmt::Debug for BeaconSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("BeaconSource")
            .field(&if self.0.is_some() { "set" } else { "empty" })
            .finish()
    }
}

/// Where this frame's end portals come from — same "unset means draw
/// nothing" convention as [`SkullSource`]. No per-position tracker behind
/// it, for the same reason [`BeaconSource`] needs none: `TheEndPortalBlockEntity.
/// shouldRenderFace` reads no world state and no NBT at all (always `{Up,
/// Down}`), so there is nothing to advance in `Sim::step`.
#[derive(Default)]
pub struct EndPortalSource(
    #[allow(clippy::type_complexity)]
    pub(super)  Option<Box<dyn Fn(glam::Vec3) -> Vec<lodestone_render::EndPortalSpawn> + Send + Sync>>,
);

impl EndPortalSource {
    /// This frame's end portals, or none when unset.
    #[must_use]
    pub(super) fn portals(&self, eye: glam::Vec3) -> Vec<lodestone_render::EndPortalSpawn> {
        self.0.as_ref().map(|f| f(eye)).unwrap_or_default()
    }
}

impl std::fmt::Debug for EndPortalSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("EndPortalSource")
            .field(&if self.0.is_some() { "set" } else { "empty" })
            .finish()
    }
}

/// Where this frame's end gateways come from — same shape as
/// [`EndPortalSource`]. `faces` (the resolved neighbor-occlusion list) is
/// current world state re-read fresh every call, same as `BeaconSource`'s
/// `levels`/`beamSections`; the gateway's *teleport beam* is a deliberate,
/// documented gap this source does not carry — see
/// `lodestone_render::end_portal`'s module doc.
#[derive(Default)]
pub struct EndGatewaySource(
    #[allow(clippy::type_complexity)]
    pub(super)  Option<Box<dyn Fn(glam::Vec3) -> Vec<lodestone_render::EndGatewaySpawn> + Send + Sync>>,
);

impl EndGatewaySource {
    /// This frame's end gateways, or none when unset.
    #[must_use]
    pub(super) fn gateways(&self, eye: glam::Vec3) -> Vec<lodestone_render::EndGatewaySpawn> {
        self.0.as_ref().map(|f| f(eye)).unwrap_or_default()
    }
}

impl std::fmt::Debug for EndGatewaySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("EndGatewaySource")
            .field(&if self.0.is_some() { "set" } else { "empty" })
            .finish()
    }
}

/// Where this frame's shulker boxes come from — same shape as [`SkullSource`],
/// and the thinnest of the family: the closure needs no partial tick and no
/// animation map at all, because a shulker box's whole appearance is a function
/// of its block state.
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

/// Where this frame's decorated pots come from — same shape as
/// [`ShulkerSource`]: no partial tick and no animation map, because the
/// hit-wobble is a `BLOCK_EVENT` this workspace does not decode yet (see
/// [`lodestone_render::DecoratedPotSpawn`]'s doc), so a pot always draws at
/// rest.
#[derive(Default)]
pub struct DecoratedPotSource(
    #[allow(clippy::type_complexity)]
    pub(super)  Option<
        Box<dyn Fn(glam::Vec3) -> Vec<lodestone_render::DecoratedPotSpawn> + Send + Sync>,
    >,
);

impl DecoratedPotSource {
    /// This frame's decorated pots, or none when unset.
    #[must_use]
    pub(super) fn decorated_pots(&self, eye: glam::Vec3) -> Vec<lodestone_render::DecoratedPotSpawn> {
        self.0.as_ref().map(|f| f(eye)).unwrap_or_default()
    }
}

impl std::fmt::Debug for DecoratedPotSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("DecoratedPotSource")
            .field(&if self.0.is_some() { "set" } else { "empty" })
            .finish()
    }
}

/// Where this frame's conduits come from.
///
/// **Not** as thin as [`ShulkerSource`]: a conduit's `isActive`/`isHunting` and
/// its two tick counters are `ConduitBlockEntity.clientTick`'s own
/// **client-computed** state (a 3×3×3-then-5×5×5 block-store scan, never sent
/// over the wire — see `lodestone_render::block_entity::conduit_frame_scan`'s
/// doc), so the closure this wraps has to carry a per-position tick tracker the
/// same way [`BellSource`] carries `BellShakes`. Installed per frame anyway, for
/// [`ShulkerSource`]'s reason: a source that outlived a disconnect would hand
/// out spawns from a dead world's handle.
#[derive(Default)]
pub struct ConduitSource(
    #[allow(clippy::type_complexity)]
    pub(super)  Option<Box<dyn Fn(glam::Vec3) -> Vec<lodestone_render::ConduitSpawn> + Send + Sync>>,
);

impl ConduitSource {
    /// This frame's conduits, or none when unset.
    #[must_use]
    pub(super) fn conduits(&self, eye: glam::Vec3) -> Vec<lodestone_render::ConduitSpawn> {
        self.0.as_ref().map(|f| f(eye)).unwrap_or_default()
    }
}

impl std::fmt::Debug for ConduitSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ConduitSource")
            .field(&if self.0.is_some() { "set" } else { "empty" })
            .finish()
    }
}

/// Where this frame's lectern books come from — same shape as
/// [`ShulkerSource`], and for the same reason: a lectern book's pose is a
/// compile-time constant, so the closure needs neither a partial tick nor an
/// animation map.
#[derive(Default)]
pub struct LecternSource(
    #[allow(clippy::type_complexity)]
    pub(super)  Option<Box<dyn Fn(glam::Vec3) -> Vec<lodestone_render::LecternSpawn> + Send + Sync>>,
);

impl LecternSource {
    /// This frame's lectern books, or none when unset.
    #[must_use]
    pub(super) fn lecterns(&self, eye: glam::Vec3) -> Vec<lodestone_render::LecternSpawn> {
        self.0.as_ref().map(|f| f(eye)).unwrap_or_default()
    }
}

impl std::fmt::Debug for LecternSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("LecternSource")
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

/// Where this frame's filled-map pictures come from.
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

/// Where this frame's banners come from — the same shape as
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

/// Where this frame's enchanting-table books come from — the same
/// shape as [`LecternSource`], sharing its very mesh, but it must be
/// **re-installed every frame** for [`BellSource`]'s reason and more strongly:
/// it captures the animation fold *and* the partial tick, and all four animated
/// values are client-simulated, so a stale closure freezes every book with no
/// packet whose absence could explain it.
#[derive(Default)]
pub struct EnchantingTableSource(
    #[allow(clippy::type_complexity)]
    pub(super)  Option<
        Box<dyn Fn(glam::Vec3) -> Vec<lodestone_render::EnchantingTableSpawn> + Send + Sync>,
    >,
);

impl EnchantingTableSource {
    /// This frame's enchanting-table books, or none when unset.
    #[must_use]
    pub(super) fn enchanting_tables(
        &self,
        eye: glam::Vec3,
    ) -> Vec<lodestone_render::EnchantingTableSpawn> {
        self.0.as_ref().map(|f| f(eye)).unwrap_or_default()
    }
}

impl std::fmt::Debug for EnchantingTableSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("EnchantingTableSource")
            .field(&if self.0.is_some() { "set" } else { "empty" })
            .finish()
    }
}

/// Where this frame's campfire cooking items come from.
///
/// **The odd one out of this family**: every other block-entity source above
/// feeds `prepare_block_entities` and the entity pipeline, and this one feeds
/// [`RenderState::prepare_item_geometry`](crate::gpu::RenderState) and the
/// *model* pipeline, because `CampfireRenderer` draws item models rather than a
/// cuboid rig. Adding it to `prepare_block_entities`' emptiness condition would
/// be wrong for exactly that reason — it has no `BlockEntityBatch` to contribute.
#[derive(Default)]
pub struct CampfireSource(
    #[allow(clippy::type_complexity)]
    pub(super)  Option<
        Box<dyn Fn(glam::Vec3) -> Vec<lodestone_render::CampfireItemSpawn> + Send + Sync>,
    >,
);

impl CampfireSource {
    /// This frame's campfire cooking items, or none when unset.
    #[must_use]
    pub(super) fn campfire_items(
        &self,
        eye: glam::Vec3,
    ) -> Vec<lodestone_render::CampfireItemSpawn> {
        self.0.as_ref().map(|f| f(eye)).unwrap_or_default()
    }
}

impl std::fmt::Debug for CampfireSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("CampfireSource")
            .field(&if self.0.is_some() { "set" } else { "empty" })
            .finish()
    }
}

/// Where this frame's brushable-block revealed items come from.
///
/// **The same odd one out as [`CampfireSource`], for the same reason**:
/// `BrushableBlockRenderer.submit` draws a single item model, not a cuboid
/// rig — the suspicious sand/gravel a player sees is the ordinary block
/// model, real geometry the terrain mesher already draws — so this feeds
/// [`RenderState::prepare_item_geometry`](crate::gpu::RenderState) and the
/// *model* pipeline, not `prepare_block_entities`.
///
/// No clock captured, like [`CampfireSource`]: nothing about a revealed item
/// animates, so this needs no per-frame re-install for staleness (only for
/// [`SkullSource`]'s reason — a source outliving a disconnect).
#[derive(Default)]
pub struct BrushableSource(
    #[allow(clippy::type_complexity)]
    pub(super)  Option<
        Box<dyn Fn(glam::Vec3) -> Vec<lodestone_render::BrushableItemSpawn> + Send + Sync>,
    >,
);

impl BrushableSource {
    /// This frame's brushable-block revealed items, or none when unset.
    #[must_use]
    pub(super) fn brushable_items(
        &self,
        eye: glam::Vec3,
    ) -> Vec<lodestone_render::BrushableItemSpawn> {
        self.0.as_ref().map(|f| f(eye)).unwrap_or_default()
    }
}

impl std::fmt::Debug for BrushableSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("BrushableSource")
            .field(&if self.0.is_some() { "set" } else { "empty" })
            .finish()
    }
}

/// Where this frame's shelved items come from.
///
/// **The same odd one out as [`CampfireSource`]/[`BrushableSource`], for the
/// same reason**: `ShelfRenderer.submit` draws up to three item models, not
/// a cuboid rig — a shelf's board/back/sides are all real block-model
/// geometry the terrain mesher already draws — so this feeds
/// [`RenderState::prepare_item_geometry`](crate::gpu::RenderState) and the
/// *model* pipeline, not `prepare_block_entities`.
///
/// No clock captured: nothing about a shelved item animates.
#[derive(Default)]
pub struct ShelfSource(
    #[allow(clippy::type_complexity)]
    pub(super)
        Option<Box<dyn Fn(glam::Vec3) -> Vec<lodestone_render::ShelfItemSpawn> + Send + Sync>>,
);

impl ShelfSource {
    /// This frame's shelved items, or none when unset.
    #[must_use]
    pub(super) fn shelf_items(&self, eye: glam::Vec3) -> Vec<lodestone_render::ShelfItemSpawn> {
        self.0.as_ref().map(|f| f(eye)).unwrap_or_default()
    }
}

impl std::fmt::Debug for ShelfSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ShelfSource")
            .field(&if self.0.is_some() { "set" } else { "empty" })
            .finish()
    }
}

/// Where this frame's vault display-item clusters come from.
///
/// **The same odd one out as [`CampfireSource`], for the same reason**:
/// `VaultRenderer.submit` draws an item cluster, not a cuboid rig — the
/// vault's cage/door/base are all real block-model geometry the terrain
/// mesher already draws (`blockstates/vault.json` is a plain `variants`
/// map) — so this feeds
/// [`RenderState::prepare_item_geometry`](crate::gpu::RenderState) and the
/// *model* pipeline, not `prepare_block_entities`.
///
/// **Must be re-installed every frame**, like [`BeaconSource`]: the spin
/// advances every tick, and a stale closure freezes it — the same
/// `game_time`/`partial_tick` capture `Sim::beacon_source` uses, since a
/// vault's spin needs no per-position tracker either (see
/// `lodestone_render::entity::vault_spin_degrees`'s doc).
#[derive(Default)]
pub struct VaultSource(
    #[allow(clippy::type_complexity)]
    pub(super)  Option<Box<dyn Fn(glam::Vec3) -> Vec<lodestone_render::VaultSpawn> + Send + Sync>>,
);

impl VaultSource {
    /// This frame's vault display-item clusters, or none when unset.
    #[must_use]
    pub(super) fn vaults(&self, eye: glam::Vec3) -> Vec<lodestone_render::VaultSpawn> {
        self.0.as_ref().map(|f| f(eye)).unwrap_or_default()
    }
}

impl std::fmt::Debug for VaultSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("VaultSource")
            .field(&if self.0.is_some() { "set" } else { "empty" })
            .finish()
    }
}

/// Where this frame's moving pistons come from — vanilla's
/// `PistonHeadRenderer`.
///
/// **The odd one out twice over.** Like [`CampfireSource`] it does not feed
/// `prepare_block_entities`, because `PistonHeadRenderer`'s constructor calls no
/// `bakeLayer` and so it owns no cuboid rig; and unlike `CampfireSource` it does
/// not feed the item path either. It feeds
/// [`RenderState::prepare_moving_blocks`](crate::gpu::RenderState), the
/// moving-block-model seam falling blocks already use — whole *block* models posed
/// somewhere other than their own cell.
///
/// **Must be re-installed every frame**, and for the sharpest reason in this file
/// after [`EnchantingTableSource`]: the closure captures both a snapshot of the
/// client-side progress tracker *and* the partial tick, and the entire animation
/// lasts **two ticks** (`PistonMovingBlockEntity.TICKS_TO_EXTEND`, `progress +=
/// 0.5` per tick). A stale closure does not merely freeze it — it freezes it at
/// `progress` 0, which places the head one whole cell back *inside* the piston
/// base, so the degradation is overlapping geometry rather than a still frame.
///
/// Unset — the offline demo, every headless test, and any session against a server
/// that does not send `moving_piston` block entities — yields an empty vec. That
/// leaves a **hole** for the duration of the push, not a missing decoration:
/// `moving_piston` is `RenderShape.INVISIBLE` and has no block model for the
/// terrain mesher to draw, exactly as chest and shulker box do not.
#[derive(Default)]
pub struct MovingPistonSource(
    #[allow(clippy::type_complexity)]
    pub(super)  Option<
        Box<dyn Fn(glam::Vec3) -> Vec<lodestone_render::MovingPistonSpawn> + Send + Sync>,
    >,
);

impl MovingPistonSource {
    /// This frame's moving pistons, or none when unset.
    #[must_use]
    pub(super) fn pistons(&self, eye: glam::Vec3) -> Vec<lodestone_render::MovingPistonSpawn> {
        self.0.as_ref().map(|f| f(eye)).unwrap_or_default()
    }
}

impl std::fmt::Debug for MovingPistonSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("MovingPistonSource")
            .field(&if self.0.is_some() { "set" } else { "empty" })
            .finish()
    }
}
