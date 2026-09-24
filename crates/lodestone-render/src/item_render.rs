//! The **pose** half of the 3-D item-model GUI path: turning a model's
//! `display.gui` transform into the matrices that put a mini-block in a slot.
//!
//! Geometry for a block item is baked once against the block atlas (see
//! [`BlockModels::item_forms`](crate::BlockModels::item_forms)); what makes it
//! *look* like an inventory icon rather than a block sitting at the world origin
//! is entirely the pair of matrices this module builds. There is no GPU here and
//! no state — every function is a pure matrix construction, so the fidelity
//! questions ("is the rotation order right?", "does the winding flip?") are
//! answered by unit tests instead of by squinting at a screenshot.
//!
//! # The three spaces
//!
//! 1. **Model space** — what the baker emits: a `0..1` cube (occasionally poking
//!    outside it), `+X` east, `+Y` up, `+Z` south, exactly as in the world.
//! 2. **GUI pixel space** — `x` right, `y` **down** from the top-left of the
//!    render target, `z` growing *towards the viewer*. This is vanilla's GUI
//!    space: its GUI-graphics item-render function does `translate(x + 8, y + 8, 150)` then
//!    `scale(16, -16, 16)`, and vanilla's GUI ortho is set up so a larger `z` is
//!    nearer. [`gui_item_pose`] maps model space here.
//! 3. **Clip space** — `wgpu` NDC with `y` up and depth in `0..1`, `0` nearest.
//!    [`gui_ortho`] maps GUI pixel space here.
//!
//! Feed `gui_ortho(w, h)` to the model pipeline as its `view_proj` and
//! `gui_item_pose(rect, transform)` to
//! [`mesh_item_quads`](crate::mesh_item_quads), and the existing
//! [`ModelPipeline`](crate::ModelPipeline) draws the item with no new pipeline,
//! shader, or atlas.
//!
//! # Handedness: two flips that must cancel
//!
//! [`gui_item_pose`] contains vanilla's `scale(16, -16, 16)` — the `y` flip that
//! reconciles "model `+Y` is up" with "screen `+Y` is down". Its determinant is
//! **negative**, which reverses triangle winding. [`ModelPipeline`] culls back
//! faces with `FrontFace::Ccw`, so on its own that flip would render every block
//! inside-out: you would see the *inside* of the three far faces, which still
//! reads as a plausible-looking isometric cube in a screenshot. [`gui_ortho`]
//! carries the compensating flip (screen `y` down → NDC `y` up), so the composed
//! `gui_ortho * gui_item_pose` has the **same** screen-space orientation as the
//! world path's `Camera::view_projection`, and the same three faces survive
//! culling as are nearest under the depth test. `winding_matches_the_world_camera`
//! in this module's tests is what holds that down.
//!
//! # Why the `/16` and the clamps live here
//!
//! [`DisplayTransform`] stores the *raw JSON numbers*
//! (`lodestone_assets`'s parser is deliberately verbatim, and its tests assert
//! exactly that). Vanilla's `ItemTransform` deserializer multiplies the
//! translation by `1/16` and clamps it to `±5`, and clamps the scale to `±4`.
//! Applying those here keeps the parsed struct honest — a field that silently
//! means "the JSON value ÷ 16" is worse than one that means the JSON value — and
//! keeps every render-time convention in the renderer.
//!
//! # And the *which geometry* half
//!
//! [`ItemStateContext`] is the other thing a draw needs to decide: **which** of an
//! item's baked variants this pass is drawing. It lives here rather than in
//! `lodestone_assets` because the property values are live game state, which that
//! GPU-free data crate deliberately does not own — it supplies only the
//! `ItemPropertyContext` *trait*. See
//! [`ItemVariants`](crate::ItemVariants) and `docs/item-variants.md`.

use glam::{Mat4, Quat, Vec3};
use lodestone_assets::{
    DISPLAY_CONTEXT_PROPERTY, DisplaySlot, DisplayTransform, ItemNodeTransform, ItemPropertyContext,
};

/// Model-space units per block: a model JSON's `display` translation is in
/// sixteenths of a block, matching the `0..16` element coordinate grid.
pub const UNITS_PER_BLOCK: f32 = 16.0;

/// Vanilla's clamp on a display translation, in **blocks** (i.e. applied after
/// the `/16`). From `ItemTransform`'s deserializer.
pub const TRANSLATION_LIMIT: f32 = 5.0;

/// Vanilla's clamp on a display scale. From `ItemTransform`'s deserializer.
pub const SCALE_LIMIT: f32 = 4.0;

/// Half the depth range [`gui_ortho`] maps into `0..1` clip depth, in GUI
/// pixels. GUI z is tiny in practice (a `0.625`-scaled block spans under ±9 px),
/// so this only has to be comfortably larger than any HUD layering offset;
/// vanilla's GUI ortho spans a similarly generous `1000..21000`.
pub const GUI_DEPTH_HALF_RANGE: f32 = 1000.0;

/// The `display.gui` (or any other slot's) transform as a model-space matrix.
///
/// ```text
/// T(translation/16) · Rx · Ry · Rz · S(scale) · T(-0.5, -0.5, -0.5)
/// ```
///
/// Read right to left, that is vanilla's order: `ItemRenderer` pushes
/// `translate(-0.5, -0.5, -0.5)` **after** `ItemTransform.apply` has pushed
/// translate → rotate → scale, and a `PoseStack` right-multiplies, so the
/// innermost operation is the centring. The model is centred on the origin
/// first, then scaled, then rotated, then translated — which is why the
/// transformed centre of the unit cube lands exactly on `translation/16`
/// regardless of the rotation.
///
/// The rotation is JOML `Quaternionf.rotationXYZ`, i.e. `Rx · Ry · Rz`.
#[must_use]
pub fn display_matrix(transform: &DisplayTransform) -> Mat4 {
    display_matrix_for_hand(transform, false)
}

/// [`display_matrix`], with vanilla's **left-hand fix** optionally applied.
///
/// `ItemTransform.apply(applyLeftHandFix, pose)` (26.2's decompiled
/// item-transform apply function) negates exactly three
/// numbers when the display context is a left-hand one: `translation.x`,
/// `rotation.y` and `rotation.z`. Everything else is untouched.
///
/// # This is a *second*, independent rule from the slot fallback
///
/// [`DisplayTransforms::get`](lodestone_assets::DisplayTransforms::get) already
/// answers an undeclared `thirdperson_lefthand` with the *right*-hand transform
/// ([`DisplaySlot::left_hand_fallback`](lodestone_assets::DisplaySlot::left_hand_fallback)).
/// It is tempting to read that as "the left hand is handled", but vanilla's
/// `ItemDisplayContext.leftHand()` is `true` for **every** left-hand context,
/// declared or not — so the mirror is applied on top of a *declared*
/// `thirdperson_lefthand` too. Skipping it puts a sword through the back of an
/// off hand rather than out of the front of it.
///
/// Negating two Euler angles is still a rotation, so this does **not** change the
/// determinant's sign: an off-hand item winds exactly like a main-hand one.
#[must_use]
pub fn display_matrix_for_hand(transform: &DisplayTransform, left_hand: bool) -> Mat4 {
    let [tx, ty, tz] = transform.translation;
    let [rx, ry, rz] = transform.rotation;
    let (tx, ry, rz) = if left_hand {
        (-tx, -ry, -rz)
    } else {
        (tx, ry, rz)
    };

    let translation = (Vec3::new(tx, ty, tz) / UNITS_PER_BLOCK).clamp(
        Vec3::splat(-TRANSLATION_LIMIT),
        Vec3::splat(TRANSLATION_LIMIT),
    );
    let scale =
        Vec3::from(transform.scale).clamp(Vec3::splat(-SCALE_LIMIT), Vec3::splat(SCALE_LIMIT));
    let rotation = Mat4::from_rotation_x(rx.to_radians())
        * Mat4::from_rotation_y(ry.to_radians())
        * Mat4::from_rotation_z(rz.to_radians());

    Mat4::from_translation(translation)
        * rotation
        * Mat4::from_scale(scale)
        * Mat4::from_translation(Vec3::splat(-0.5))
}

/// The full model-space → **GUI pixel space** pose for an item drawn into
/// `rect_px` (`[x, y, w, h]`, top-left origin, in target pixels).
///
/// ```text
/// T(centre of rect) · S(w, -h, min(w, h)) · display_matrix(transform)
/// ```
///
/// For the 16×16 slot every real hotbar/inventory cell uses, `S(w, -h, …)` is
/// vanilla's `scale(16, -16, 16)` exactly. The `y` flip is what turns model-up
/// into screen-up once [`gui_ortho`] flips back; see the module docs on winding.
/// The `z` scale is `min(w, h)` so a non-square rect keeps the block's depth
/// proportional to its smaller on-screen dimension rather than shearing it.
#[must_use]
pub fn gui_item_pose(rect_px: [f32; 4], transform: &DisplayTransform) -> Mat4 {
    let [x, y, w, h] = rect_px;
    let centre = Vec3::new(x + w * 0.5, y + h * 0.5, 0.0);
    Mat4::from_translation(centre)
        * Mat4::from_scale(Vec3::new(w, -h, w.min(h)))
        * display_matrix(transform)
}

/// A `minecraft:special` node's own `"transformation"` field
/// ([`ItemNodeTransform`]) as a model-space matrix.
///
/// ```text
/// T(translation) · Q(left_rotation) · S(scale) · Q(right_rotation)
/// ```
///
/// Vanilla's `com.mojang.math.Transformation`'s private `compose` builds
/// exactly this product (`result.translation(t); result.rotate(left);
/// result.scale(s); result.rotate(right);`, each JOML call right-multiplying
/// the running matrix) — no `/16`, no clamp, unlike [`display_matrix`]: this
/// is a different vanilla type (`Transformation`, not `ItemTransform`) with
/// its own codec and no such deserializer-side massaging.
#[must_use]
pub fn node_transform_matrix(t: &ItemNodeTransform) -> Mat4 {
    let [lx, ly, lz, lw] = t.left_rotation;
    let [rx, ry, rz, rw] = t.right_rotation;
    Mat4::from_translation(Vec3::from(t.translation))
        * Mat4::from_quat(Quat::from_xyzw(lx, ly, lz, lw))
        * Mat4::from_scale(Vec3::from(t.scale))
        * Mat4::from_quat(Quat::from_xyzw(rx, ry, rz, rw))
}

/// Composes a `minecraft:special` node's own transformation on top of an
/// already-built outer placement — the GUI icon pose, the held-item hand
/// chain, or a dropped/other-entity-hand/item-frame world placement.
///
/// # Why right-multiply, not left
///
/// Vanilla's unbaked special-model-wrapper bake function computes
/// `Transformation.compose(transformation, this.transformation)`, which is
/// vanilla's transformation-record compose function (`Matrix4fc parent, Optional<Transformation>
/// transform`) → `parent.mul(transform.getMatrix())`. JOML's `Matrix4f.mul`
/// is `this * other`; applied to a column vector right-to-left, `other` (the
/// node's own transform) acts on the model *first*, `parent` (the
/// already-built outer placement — `display.<context>`, or the world/hand/
/// GUI chain built on top of it) acts *second*. `glam::Mat4` uses the same
/// column-vector, right-to-left convention, so the direct translation is
/// `outer * node_transform_matrix`, not the other way round.
///
/// # Why a slice
///
/// `"transformation"` is a field of every item-definition node, not of
/// `special` alone, and `bake` threads the accumulated matrix down the tree —
/// so what reaches a `special` renderer is the whole root-to-node chain,
/// outermost first. Folding left to right (`outer * m[0] * m[1] * …`)
/// reproduces vanilla's repeated `parent.mul(child)`. An empty slice leaves
/// `outer` unchanged, vanilla's `Optional::isEmpty` short-circuit.
///
/// Passing only the `special` node's own entry is what drew every shield
/// back-to-front: its `scale [1, -1, -1]` lives on the enclosing
/// `minecraft:condition` node.
#[must_use]
pub fn compose_special_node_transform(outer: Mat4, transformation: &[ItemNodeTransform]) -> Mat4 {
    transformation
        .iter()
        .fold(outer, |acc, t| acc * node_transform_matrix(t))
}

/// Composes an item-definition `minecraft:special` form into its outer pose.
///
/// This recovers the canonical item-definition wrapper when a pack selects the
/// raw `minecraft:head` renderer but supplies no node transformation. The 26.2
/// `SkullSpecialRenderer` submits the Y-down skull mesh without a local pose;
/// vanilla's `items/*_head.json` definitions supply
/// `T(+0.5, 0, +0.5) * Rx(180°)`. A pack that replaces `player_head.json` with
/// an empty generic-head node otherwise loses both terms.
///
/// The fallback is right-multiplied after the complete node chain, yielding
/// `outer * node[0] * … * fallback`. Therefore it acts in model space before a
/// GUI pose, first-person hand pose, or world/third-person placement, rather
/// than drifting along those surfaces' axes. It applies only to an *empty*
/// generic-head chain: all parsed transforms, including legacy
/// `minecraft:player_head` and ordinary vanilla `minecraft:head` definitions,
/// remain authoritative and cannot be doubled.
#[must_use]
pub fn compose_special_item_transform(
    outer: Mat4,
    kind: &str,
    transformation: &[ItemNodeTransform],
) -> Mat4 {
    let placement = compose_special_node_transform(outer, transformation);
    if kind == "minecraft:head" && transformation.is_empty() {
        placement
            * Mat4::from_translation(Vec3::new(0.5, 0.0, 0.5))
            * Mat4::from_scale(Vec3::new(1.0, -1.0, -1.0))
    } else {
        placement
    }
}

/// The **GUI pixel space → clip space** projection for a `width_px × height_px`
/// render target.
///
/// * `x`: `0..width` → `-1..1`.
/// * `y`: `0..height` → `1..-1` (top-left origin; NDC `y` is up).
/// * `z`: `-`[`GUI_DEPTH_HALF_RANGE`]`..`[`GUI_DEPTH_HALF_RANGE`] → `0..1`, so a
///   **larger** GUI `z` is **nearer** — vanilla's convention, and the one
///   [`gui_item_pose`]'s positive `z` scale needs for the faces that survive
///   back-face culling to also be the faces nearest under the pipeline's depth
///   compare ([`DEPTH_COMPARE_NEARER_OR_EQUAL`](crate::DEPTH_COMPARE_NEARER_OR_EQUAL)
///   — nearest still wins; only exactly-coincident faces distinguish it from
///   the strict form).
///
/// # The `z` direction is the world projection's, not a free choice
///
/// A GUI item is drawn through the same [`ModelPipeline`](crate::ModelPipeline)
/// and the same [`DEPTH_COMPARE_NEARER_OR_EQUAL`](crate::DEPTH_COMPARE_NEARER_OR_EQUAL)
/// as world geometry, into a depth attachment cleared to
/// [`DEPTH_CLEAR`](crate::DEPTH_CLEAR). So "larger GUI `z` is nearer" only holds
/// if this matrix agrees with [`Camera::projection_matrix`](crate::Camera::projection_matrix)
/// about which end of `0..1` the near plane is — which since that projection
/// became **reversed-Z** is `1`. The `z` scale is therefore `+0.5 / half-range`
/// where a forward projection needed `-0.5 / half-range`.
///
/// That sign also carries the winding invariant: flipping it flips this
/// matrix's determinant, which is the same flip reversing the world projection
/// makes, so `sign(det(gui_ortho * gui_item_pose))` still equals
/// `sign(det(Camera::view_projection()))`. Reversing one and not the other
/// breaks that silently — the icons keep rasterising, they just draw their far
/// faces.
///
/// The `y` flip here is the counterpart to [`gui_item_pose`]'s: the two cancel,
/// so triangle winding is preserved. See the module docs.
#[must_use]
pub fn gui_ortho(width_px: u32, height_px: u32) -> Mat4 {
    let w = width_px.max(1) as f32;
    let h = height_px.max(1) as f32;
    Mat4::from_translation(Vec3::new(-1.0, 1.0, 0.5))
        * Mat4::from_scale(Vec3::new(2.0 / w, -2.0 / h, 0.5 / GUI_DEPTH_HALF_RANGE))
}

// ---------------------------------------------------------------------------
// Which variant: the live item property context
// ---------------------------------------------------------------------------

/// `minecraft:crossbow/pull`'s denominator: `CrossbowItem.getChargeDuration` with
/// **no Quick Charge**, `floor(1.25 * 20)`.
///
/// Read from vanilla's crossbow-item charge-duration function
/// (`Mth.floor(EnchantmentHelper.modifyCrossbowChargingTime(stack, user, 1.25F) * 20.0F)`),
/// not guessed. The enchantment level is not modelled anywhere on this side of the
/// wire — `RenderEquipment` narrows a stack to a bare item id long before a draw —
/// so an enchanted crossbow winds visually slower than it really does. That is the
/// same approximation `lodestone-shell`'s `arm_pose_for` already makes for the
/// *arm* pose, and the two must agree or a crossbow's arms and its model would
/// disagree about how far along the wind is.
pub const CROSSBOW_CHARGE_TICKS: f32 = 25.0;

/// The live state an item's definition tree is resolved against for one draw —
/// the runtime half of [`ItemVariants`](crate::ItemVariants).
///
/// # What it can answer, and why only these
///
/// Every property here is sourced from state the client actually holds; nothing is
/// guessed. Anything unsourced falls through to the trait's "unset" answer
/// (`false` / `None` / `0.0`), which routes a `condition` to its `on_false` and a
/// `select`/`range_dispatch` to its `fallback` — i.e. to the item's default
/// appearance, which is what the whole path did before the variant axis existed.
/// A wrong *guess* would be worse than the default, loudly and everywhere.
///
/// | property | source |
/// |---|---|
/// | `minecraft:display_context` | [`Self::display`] — static per pass |
/// | `minecraft:using_item` | `LivingEntity`'s flags byte, via `lodestone_ecs`'s `ItemUse` |
/// | `minecraft:use_duration` | `ItemUse::ticks` |
/// | `minecraft:crossbow/pull` | `ItemUse::ticks / `[`CROSSBOW_CHARGE_TICKS`] |
///
/// Deliberately **unsourced**, each because the datum genuinely is not decoded:
/// `trim_material`, `bundle/has_selected_item`, `block_state`, `local_time`,
/// `compass`, `has_component`, `use_cycle`, `time`, `context_dimension`,
/// `charge_type`, `broken`, `fishing_rod/cast`, `damage`, `count`, `cooldown`.
/// Most need per-stack components; `custom_model_data` is the exception and is
/// sourced below. `time`
/// and `local_time` need a level clock this type is not given.
///
/// # `use_duration` counts UP, `use_cycle` counts DOWN — do not unify them
///
/// Vanilla's `UseDuration.get` returns
/// `stack.getUseDuration(owner) - owner.getUseItemRemainingTicks()`, i.e.
/// `getTicksUsingItem()`, which **increases** from 0 as the bow is drawn. Our
/// `ItemUse::ticks` already *is* that number (it counts up from the rising edge of
/// the using-item bit precisely so no per-item `getUseDuration` lookup is needed),
/// so [`Self::use_ticks`] is fed in **directly, with no inversion**.
///
/// `UseCycle` in the same package is `getUseItemRemainingTicks() % period` — the
/// *other* direction — and it needs the per-item `getUseDuration` we do not model
/// (a brush's is 200 ticks). It is therefore listed as unsourced above, and an
/// "obvious" `duration - ticks` inversion applied to `use_duration` to make the two
/// look alike would pin a drawn bow at `bow_pulling_0` forever while reading, from
/// the property name alone, perfectly correct.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ItemStateContext {
    /// Which of vanilla's nine `ItemDisplayContext`s this pass draws in. The one
    /// property that is a property of the *pass* rather than of the stack, and the
    /// one that fixes 26 items with no live state at all.
    pub display: DisplaySlot,
    /// Whether the holder is using **this** item right now — vanilla's
    /// `owner.isUsingItem() && owner.getUseItem() == itemStack`. The second half is
    /// why a caller must check the *hand*: an entity drawing a bow in the main hand
    /// is not using the shield in its off hand.
    pub using: bool,
    /// `getTicksUsingItem()` — ticks elapsed since the use began, counting **up**
    /// from 0. Meaningless while `!using`, and read as `0` there, matching
    /// vanilla's `getUseItem() != itemStack` early return.
    pub use_ticks: u32,
    /// `minecraft:custom_model_data` numeric selector at index zero. The
    /// network component is a list; vanilla's range property reads this first
    /// element and defaults to zero when it is absent.
    pub custom_model_data: f32,
}

impl ItemStateContext {
    /// A context for `display` with no item in use — the resting form, and the
    /// right thing for every pass that has no using-item state to offer.
    #[must_use]
    pub const fn new(display: DisplaySlot) -> Self {
        Self {
            display,
            using: false,
            use_ticks: 0,
            custom_model_data: 0.0,
        }
    }

    /// [`Self::new`] plus the using-item state, for a holder we know about.
    ///
    /// `using` should already be narrowed to *this* item: pass
    /// `item_use.using && (item_use.off_hand == arm.is_left())`, not
    /// `item_use.using`, or an entity drawing a bow would also draw its off-hand
    /// item mid-use.
    ///
    /// For [`Self::display`] in a held-item pass, use
    /// [`Arm::display_slot`](crate::entity::Arm::display_slot) — the same
    /// expression [`hand_transform`](crate::entity::hand_transform) reads the pose
    /// from, so the variant and the transform cannot disagree about which hand.
    #[must_use]
    pub const fn with_use(mut self, using: bool, use_ticks: u32) -> Self {
        self.using = using;
        self.use_ticks = use_ticks;
        self
    }

    /// Adds the stack's `minecraft:custom_model_data` index-zero number.
    #[must_use]
    pub const fn with_custom_model_data(mut self, value: f32) -> Self {
        self.custom_model_data = value;
        self
    }
}

impl ItemPropertyContext for ItemStateContext {
    fn condition(&self, property: &str, _component: Option<&str>) -> bool {
        match property {
            "minecraft:using_item" => self.using,
            _ => false,
        }
    }

    fn select(&self, property: &str) -> Option<String> {
        if property == DISPLAY_CONTEXT_PROPERTY {
            Some(self.display.json_name().to_string())
        } else {
            None
        }
    }

    fn range(&self, property: &str) -> f32 {
        // `CustomModelData` is a property of the stack, not of an active use.
        // Check it before the use-state gate below: a gun in an inventory or a
        // resting hand must select its model just as it does while being used.
        if property == "minecraft:custom_model_data" {
            return self.custom_model_data;
        }
        if !self.using {
            // Both sourced ranges are gated on `getUseItem() == itemStack` in
            // vanilla and return `0.0F` otherwise. Gating here rather than at each
            // arm keeps a future property from silently forgetting it.
            return 0.0;
        }
        match property {
            // `getTicksUsingItem()` verbatim; see the type docs for why there is
            // no inversion here.
            "minecraft:use_duration" => self.use_ticks as f32,
            // `useDuration / getChargeDuration`. Vanilla additionally returns 0
            // when the crossbow is already **charged**, which reads the
            // `minecraft:charged_projectiles` component we do not decode — so a
            // charged crossbow keeps whatever wind fraction its last using tick
            // left, instead of snapping back to the slack model.
            "minecraft:crossbow/pull" => self.use_ticks as f32 / CROSSBOW_CHARGE_TICKS,
            _ => 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera::Camera;
    use lodestone_assets::Direction;

    /// Vanilla's `block/block` `display.gui` transform: the isometric pose every
    /// block item is drawn with.
    fn vanilla_block_gui() -> DisplayTransform {
        DisplayTransform {
            rotation: [30.0, 225.0, 0.0],
            translation: [0.0, 0.0, 0.0],
            scale: [0.625, 0.625, 0.625],
        }
    }

    #[test]
    fn unwrapped_generic_head_reinstates_the_canonical_wrapper_in_gui_and_third_person() {
        // 26.2's SkullSpecialRenderer submits the raw Y-down skull box. The
        // canonical items/*_head.json definitions supply the missing local
        // wrapper T(.5, 0, .5) * Rx(180°). This server pack retargets a player
        // head to `minecraft:head` but leaves that node empty, so the complete
        // wrapper — not a translation-only nudge — has to be restored under
        // the GUI and held-item placements.
        let template_skull_third_person = DisplayTransform {
            rotation: [45.0, 45.0, 0.0],
            translation: [0.0, 3.0, 0.0],
            scale: [0.5, 0.5, 0.5],
        };
        let poses = [
            gui_item_pose([40.0, 20.0, 16.0, 16.0], &vanilla_block_gui()),
            crate::entity::held_item_matrix(
                Mat4::IDENTITY,
                crate::entity::Arm::Right,
                false,
                &template_skull_third_person,
            ),
        ];
        let legacy_wrapper = [ItemNodeTransform {
            translation: [0.5, 0.0, 0.5],
            left_rotation: [1.0, 0.0, 0.0, 0.0],
            ..ItemNodeTransform::default()
        }];
        let canonical_wrapper = node_transform_matrix(&legacy_wrapper[0]);

        for outer in poses {
            assert_eq!(
                compose_special_item_transform(outer, "minecraft:head", &[]),
                outer * canonical_wrapper,
                "an empty generic-head node needs the complete standard item wrapper"
            );
            assert_eq!(
                compose_special_item_transform(
                    outer,
                    "minecraft:player_head",
                    &legacy_wrapper,
                ),
                outer * canonical_wrapper,
                "legacy player head's wrapper is already its one centre correction"
            );
            assert_eq!(
                compose_special_item_transform(outer, "minecraft:head", &legacy_wrapper),
                outer * canonical_wrapper,
                "a parsed generic-head wrapper must remain authoritative rather than double"
            );
            let raw_y = outer.transform_vector3(Vec3::Y);
            let wrapped_y = compose_special_item_transform(outer, "minecraft:head", &[])
                .transform_vector3(Vec3::Y);
            assert!(
                raw_y.dot(wrapped_y) < 0.0,
                "Rx(180°) must reverse the raw skull's Y-down landmark rather than leave Steve upside-down"
            );
        }
    }

    // --- display_matrix: the multiplication order ------------------------

    #[test]
    fn centring_is_innermost_so_the_cube_centre_lands_on_the_translation() {
        // The sharpest statement of "T(-0.5) is applied FIRST": whatever the
        // rotation and scale, the model's centre is the rotation pivot, so it
        // maps to exactly translation/16. Any other order (rotating before
        // centring, or translating before rotating) breaks this.
        let t = DisplayTransform {
            rotation: [30.0, 225.0, 15.0],
            translation: [8.0, -16.0, 4.0], // → (0.5, -1.0, 0.25) blocks
            scale: [0.625, 0.5, 2.0],
        };
        let centre = display_matrix(&t).transform_point3(Vec3::splat(0.5));
        assert!(
            (centre - Vec3::new(0.5, -1.0, 0.25)).length() < 1e-5,
            "cube centre must map to translation/16, got {centre}"
        );
    }

    #[test]
    fn rotation_then_translation_maps_a_known_point_exactly() {
        // A hand-computable case. rotation Y=90°, translation (16,0,0) → 1 block.
        // Model point (1, 0.5, 0.5) centres to (0.5, 0, 0); Ry(90) sends
        // +X to -Z, so it becomes (0, 0, -0.5); the translation lands it at
        // (1, 0, -0.5).
        let t = DisplayTransform {
            rotation: [0.0, 90.0, 0.0],
            translation: [16.0, 0.0, 0.0],
            scale: [1.0, 1.0, 1.0],
        };
        let p = display_matrix(&t).transform_point3(Vec3::new(1.0, 0.5, 0.5));
        assert!(
            (p - Vec3::new(1.0, 0.0, -0.5)).length() < 1e-5,
            "expected (1, 0, -0.5), got {p}"
        );

        // The order actually matters: translating *before* rotating would send
        // the same point to Ry(90)·(1.5, 0, 0) = (0, 0, -1.5) instead.
        let wrong = Mat4::from_rotation_y(90f32.to_radians())
            * Mat4::from_translation(Vec3::new(1.0, 0.0, 0.0))
            * Mat4::from_translation(Vec3::splat(-0.5));
        let wrong_p = wrong.transform_point3(Vec3::new(1.0, 0.5, 0.5));
        assert!(
            (wrong_p - p).length() > 0.5,
            "the wrong order must be distinguishable, got {wrong_p} vs {p}"
        );
    }

    #[test]
    fn rotation_is_x_then_y_then_z_in_that_multiplication_order() {
        // Rx·Ry·Rz and Rz·Ry·Rx disagree unless two of the angles are zero, so a
        // single triple pins the JOML `rotationXYZ` order down.
        let t = DisplayTransform {
            rotation: [90.0, 90.0, 0.0],
            translation: [0.0; 3],
            scale: [1.0; 3],
        };
        // Rx(90)·Ry(90) applied to the centred corner offset (0.5, 0, 0):
        // Ry(90): +X → -Z → (0, 0, -0.5); Rx(90): -Z → +Y → (0, 0.5, 0).
        let p = display_matrix(&t).transform_point3(Vec3::new(1.0, 0.5, 0.5));
        assert!(
            (p - Vec3::new(0.0, 0.5, 0.0)).length() < 1e-5,
            "expected Rx·Ry ordering to give (0, 0.5, 0), got {p}"
        );
        // The reversed order Rz·Ry·Rx would give Ry(90)·Rx(90)·(0.5,0,0) =
        // Ry(90)·(0.5,0,0) = (0,0,-0.5) — a different point.
        assert!((p - Vec3::new(0.0, 0.0, -0.5)).length() > 0.4);
    }

    // --- display_matrix: the /16 and the two clamps ----------------------

    #[test]
    fn translation_is_divided_by_sixteen_and_clamped_to_five_blocks() {
        let at_limit = DisplayTransform {
            translation: [80.0, -80.0, 0.0], // exactly ±5 blocks
            ..DisplayTransform::default()
        };
        let o = display_matrix(&at_limit).transform_point3(Vec3::splat(0.5));
        assert!((o - Vec3::new(5.0, -5.0, 0.0)).length() < 1e-5, "got {o}");

        let past_limit = DisplayTransform {
            translation: [1600.0, -1600.0, 0.0], // 100 blocks before clamping
            ..DisplayTransform::default()
        };
        let o = display_matrix(&past_limit).transform_point3(Vec3::splat(0.5));
        assert!(
            (o - Vec3::new(5.0, -5.0, 0.0)).length() < 1e-5,
            "translation must clamp to ±5 blocks, got {o}"
        );
    }

    #[test]
    fn scale_is_clamped_to_four() {
        let t = DisplayTransform {
            scale: [10.0, -10.0, 1.0],
            ..DisplayTransform::default()
        };
        // The +X corner offset of 0.5 scales to 0.5 * 4 = 2.0, and the clamped
        // negative scale to -2.0 — not 5.0 / -5.0.
        let p = display_matrix(&t).transform_point3(Vec3::new(1.0, 1.0, 0.5));
        assert!(
            (p - Vec3::new(2.0, -2.0, 0.0)).length() < 1e-5,
            "scale must clamp to ±4, got {p}"
        );
    }

    // --- the left-hand fix ------------------------------------------------

    #[test]
    fn the_left_hand_fix_negates_exactly_translation_x_and_rotation_yz() {
        // Hand-derived from `ItemTransform.apply`: with rotation [0, 90, 0] and
        // translation [16, 32, 48] the right hand puts the cube centre at
        // (1, 2, 3) blocks and the left hand at (-1, 2, 3) — only x flips.
        let t = DisplayTransform {
            rotation: [0.0, 90.0, 0.0],
            translation: [16.0, 32.0, 48.0],
            scale: [1.0, 1.0, 1.0],
        };
        let right = display_matrix_for_hand(&t, false).transform_point3(Vec3::splat(0.5));
        let left = display_matrix_for_hand(&t, true).transform_point3(Vec3::splat(0.5));
        assert!((right - Vec3::new(1.0, 2.0, 3.0)).length() < 1e-5, "{right}");
        assert!((left - Vec3::new(-1.0, 2.0, 3.0)).length() < 1e-5, "{left}");

        // Ry is negated, so model +X goes the other way about the pivot: right
        // hand sends it to -Z, left hand to +Z.
        let dir = |m: Mat4| m.transform_point3(Vec3::new(1.0, 0.5, 0.5)) - m.transform_point3(Vec3::splat(0.5));
        assert!(dir(display_matrix_for_hand(&t, false)).z < -0.4);
        assert!(dir(display_matrix_for_hand(&t, true)).z > 0.4);
    }

    #[test]
    fn the_left_hand_fix_is_orientation_preserving() {
        // Negating two Euler angles is a rotation, not a reflection: an off-hand
        // item must wind exactly like a main-hand one, or every off-hand sword
        // renders inside-out while still looking like a sword.
        let t = DisplayTransform {
            rotation: [0.0, 90.0, -55.0],
            translation: [4.0, 8.0, -2.0],
            scale: [0.68, 0.68, 0.68],
        };
        let r = display_matrix_for_hand(&t, false).determinant();
        let l = display_matrix_for_hand(&t, true).determinant();
        assert!((r - l).abs() < 1e-6, "right {r} vs left {l}");
        assert!(r > 0.0, "a positive-scale display transform must not flip winding");
    }

    #[test]
    fn display_matrix_is_the_right_hand_case() {
        let t = DisplayTransform {
            rotation: [10.0, 20.0, 30.0],
            translation: [3.0, -4.0, 5.0],
            scale: [0.5, 0.5, 0.5],
        };
        assert_eq!(
            display_matrix(&t),
            display_matrix_for_hand(&t, false),
            "every existing caller must keep the unmirrored behaviour"
        );
    }

    #[test]
    fn the_identity_transform_only_centres() {
        let m = display_matrix(&DisplayTransform::default());
        assert!(
            (m.transform_point3(Vec3::splat(0.5)) - Vec3::ZERO).length() < 1e-6,
            "a default transform still centres the model on the origin"
        );
    }

    // --- The vanilla block pose: silhouette ------------------------------

    /// Axis-aligned half-extents of the transformed unit cube.
    fn cube_half_extents(m: Mat4) -> Vec3 {
        let mut lo = Vec3::splat(f32::MAX);
        let mut hi = Vec3::splat(f32::MIN);
        for i in 0..8u32 {
            let c = Vec3::new((i & 1) as f32, ((i >> 1) & 1) as f32, ((i >> 2) & 1) as f32);
            let p = m.transform_point3(c);
            lo = lo.min(p);
            hi = hi.max(p);
        }
        (hi - lo) * 0.5
    }

    #[test]
    fn vanilla_block_pose_nearly_fills_a_sixteen_pixel_slot() {
        // For a cube of half-extent h rotated by R, the bbox half-extent on axis
        // i is h * sum_j |R_ij|. With h = 0.625/2 and R = Rx(30)·Ry(225):
        //   x: 0.3125 * (0.70711 + 0 + 0.70711)       = 0.44194
        //   y: 0.3125 * (0.35355 + 0.86603 + 0.35355) = 0.49160
        //   z: 0.3125 * (0.61237 + 0.5     + 0.61237) = 0.53898
        let e = cube_half_extents(display_matrix(&vanilla_block_gui()));
        assert!((e.x - 0.44194).abs() < 1e-4, "x half-extent {}", e.x);
        assert!((e.y - 0.49160).abs() < 1e-4, "y half-extent {}", e.y);
        assert!((e.z - 0.53898).abs() < 1e-4, "z half-extent {}", e.z);

        // In a 16 px slot that is 14.14 px wide by 15.73 px tall: the icon fills
        // the slot without overflowing it, which is the visible signature of the
        // vanilla pose being right.
        let e = cube_half_extents(gui_item_pose([0.0, 0.0, 16.0, 16.0], &vanilla_block_gui()));
        assert!((e.x * 2.0 - 14.142).abs() < 1e-2, "width px {}", e.x * 2.0);
        assert!((e.y * 2.0 - 15.731).abs() < 1e-2, "height px {}", e.y * 2.0);
        assert!(
            e.x < 8.0 && e.y < 8.0,
            "the posed block must stay inside its 16 px slot"
        );
    }

    #[test]
    fn the_item_is_centred_on_its_slot_rect() {
        let m = gui_item_pose([40.0, 20.0, 16.0, 16.0], &vanilla_block_gui());
        let c = m.transform_point3(Vec3::splat(0.5));
        assert!(
            (c - Vec3::new(48.0, 28.0, 0.0)).length() < 1e-4,
            "the model centre must land on the rect centre, got {c}"
        );
    }

    #[test]
    fn model_up_becomes_screen_up() {
        // Model +Y through the pose *and* the ortho must end up with a larger NDC
        // y (up). Checking only the pose would read the flipped intermediate.
        let m = gui_ortho(256, 256) * gui_item_pose([0.0, 0.0, 16.0, 16.0], &vanilla_block_gui());
        let top = m.transform_point3(Vec3::new(0.5, 1.0, 0.5));
        let bottom = m.transform_point3(Vec3::new(0.5, 0.0, 0.5));
        assert!(
            top.y > bottom.y,
            "the model's top must render above its bottom: {top} vs {bottom}"
        );
    }

    // --- gui_ortho -------------------------------------------------------

    #[test]
    fn gui_ortho_maps_the_pixel_rect_onto_ndc_with_a_top_left_origin() {
        let m = gui_ortho(320, 240);
        let tl = m.transform_point3(Vec3::new(0.0, 0.0, 0.0));
        let br = m.transform_point3(Vec3::new(320.0, 240.0, 0.0));
        assert!((tl - Vec3::new(-1.0, 1.0, 0.5)).length() < 1e-6, "{tl}");
        assert!((br - Vec3::new(1.0, -1.0, 0.5)).length() < 1e-6, "{br}");
    }

    /// Which clip depth belongs to the surface nearer the eye, asked of the
    /// real world projection.
    ///
    /// `Camera::projection_matrix` is reversed-Z, so nearer is *greater* — but
    /// the point of deriving it is that a GUI item is drawn through the same
    /// pipeline and the same depth comparison as world geometry, so "larger GUI
    /// z is nearer" is a claim about agreeing with that projection and not a
    /// free choice. Written as a literal `<`, this test certified a `gui_ortho`
    /// pointing the opposite way to the world it shares a pipeline with.
    fn world_nearer_is_greater_depth() -> bool {
        let camera = Camera {
            position: Vec3::new(0.0, 0.0, 4.0),
            yaw: 180.0,
            ..Camera::default()
        };
        let vp = camera.view_projection();
        let depth = |z: f32| {
            let c = vp * Vec3::new(0.0, 0.0, z).extend(1.0);
            c.z / c.w
        };
        let (close, distant) = (depth(1.0), depth(0.0));
        assert_ne!(close, distant, "premise: the projection is degenerate in z");
        close > distant
    }

    #[test]
    fn larger_gui_z_is_nearer() {
        // The depth convention `gui_item_pose`'s positive z scale depends on,
        // and it has to be the *world* projection's, because a GUI item draws
        // through the same pipeline into a depth buffer cleared the same way.
        let m = gui_ortho(320, 240);
        let near = m.transform_point3(Vec3::new(0.0, 0.0, 100.0)).z;
        let far = m.transform_point3(Vec3::new(0.0, 0.0, -100.0)).z;
        if world_nearer_is_greater_depth() {
            assert!(near > far, "larger GUI z must be nearer: {near} vs {far}");
        } else {
            assert!(near < far, "larger GUI z must be nearer: {near} vs {far}");
        }
        assert!((0.0..=1.0).contains(&near) && (0.0..=1.0).contains(&far));
    }

    #[test]
    fn a_degenerate_target_size_does_not_produce_nans() {
        let m = gui_ortho(0, 0);
        assert!(m.to_cols_array().iter().all(|v| v.is_finite()));
    }

    // --- Winding: the bug class that survives visual review ---------------

    /// A unit-cube face with vanilla's outward winding: `cross(v1-v0, v2-v0)`
    /// equals the outward normal (counter-clockwise seen from outside), which is
    /// the invariant [`crate::face_winding_is_outward`] asserts for the packed
    /// path and the baker holds to for model quads.
    fn outward_face(dir: Direction) -> [Vec3; 4] {
        let n = match dir {
            Direction::East => Vec3::X,
            Direction::West => -Vec3::X,
            Direction::Up => Vec3::Y,
            Direction::Down => -Vec3::Y,
            Direction::South => Vec3::Z,
            Direction::North => -Vec3::Z,
        };
        // Any tangent u perpendicular to n, with v = n × u, gives u × v = n.
        let u = if n.x.abs() < 0.5 { Vec3::X } else { Vec3::Y };
        let v = n.cross(u);
        let centre = Vec3::splat(0.5) + n * 0.5;
        [
            centre - u * 0.5 - v * 0.5,
            centre + u * 0.5 - v * 0.5,
            centre + u * 0.5 + v * 0.5,
            centre - u * 0.5 + v * 0.5,
        ]
    }

    const ALL_FACES: [Direction; 6] = [
        Direction::East,
        Direction::West,
        Direction::Up,
        Direction::Down,
        Direction::South,
        Direction::North,
    ];

    #[test]
    fn the_test_cube_really_is_wound_outward() {
        // Guard the guard: if this helper's winding were inward, every winding
        // assertion below would be measuring the wrong thing.
        for dir in ALL_FACES {
            let q = outward_face(dir);
            let n = (q[1] - q[0]).cross(q[2] - q[0]).normalize();
            let expect = q.iter().fold(Vec3::ZERO, |a, p| a + *p) / 4.0 - Vec3::splat(0.5);
            assert!(
                n.dot(expect.normalize()) > 0.99,
                "{dir:?} face must be wound counter-clockwise from outside"
            );
        }
    }

    /// The signed screen area of a quad's first triangle after `m`. Its **sign**
    /// is what `FrontFace::Ccw` + `cull_mode: Back` acts on; positions come from
    /// `mesh_item_quads`' `[0, 1, 2]` triangle.
    fn screen_area(m: Mat4, q: [Vec3; 4]) -> f32 {
        let p: Vec<Vec3> = q.iter().map(|v| m.project_point3(*v)).collect();
        let a = p[1] - p[0];
        let b = p[2] - p[0];
        a.x * b.y - a.y * b.x
    }

    /// The mean clip-space depth of a quad after `m`.
    fn mean_depth(m: Mat4, q: [Vec3; 4]) -> f32 {
        q.iter().map(|v| m.project_point3(*v).z).sum::<f32>() / 4.0
    }

    #[test]
    fn winding_matches_the_world_camera() {
        // Ground truth: the world path renders correctly today through the same
        // `ModelPipeline` (`FrontFace::Ccw`, cull `Back`) with the same
        // outward-wound quads. So whatever screen-area sign a *known visible*
        // face has under `Camera::view_projection` is the front-facing sign, and
        // the GUI matrix must reproduce it. Deriving the reference instead of
        // hardcoding "positive" means this test cannot be fooled by a wgpu/glam
        // convention we misremembered.
        let camera = Camera {
            position: Vec3::new(0.5, 0.5, 4.0),
            yaw: 180.0, // forward = (0, 0, -1): looking at the cube's +Z face
            pitch: 0.0,
            ..Camera::default()
        };
        let world = camera.view_projection();
        let front_sign = screen_area(world, outward_face(Direction::South)).signum();
        // Sanity: the face pointing away from the camera has the other sign.
        assert_eq!(
            screen_area(world, outward_face(Direction::North)).signum(),
            -front_sign,
            "the reference camera must disagree about the far face"
        );

        // The GUI matrix, composed exactly as the draw path composes it.
        let gui = gui_ortho(256, 256) * gui_item_pose([0.0, 0.0, 16.0, 16.0], &vanilla_block_gui());

        // With rotation [30, 225, 0] the three faces turned towards the viewer
        // are up, east and north — which is why a furnace item shows its front
        // (a north face on `block/orientable`) in the inventory.
        let visible = [Direction::Up, Direction::East, Direction::North];
        let hidden = [Direction::Down, Direction::West, Direction::South];

        for dir in visible {
            assert_eq!(
                screen_area(gui, outward_face(dir)).signum(),
                front_sign,
                "{dir:?} must survive back-face culling in the GUI pose"
            );
        }
        for dir in hidden {
            assert_eq!(
                screen_area(gui, outward_face(dir)).signum(),
                -front_sign,
                "{dir:?} must be culled in the GUI pose"
            );
        }

        // ...and the surviving faces must also be the *nearest* ones, or the
        // depth test would hide exactly what culling kept. This is the half of
        // the inside-out bug that a still screenshot cannot show.
        let nearest_hidden = if world_nearer_is_greater_depth() {
            hidden
                .iter()
                .map(|d| mean_depth(gui, outward_face(*d)))
                .fold(f32::MIN, f32::max)
        } else {
            hidden
                .iter()
                .map(|d| mean_depth(gui, outward_face(*d)))
                .fold(f32::MAX, f32::min)
        };
        for dir in visible {
            let d = mean_depth(gui, outward_face(dir));
            let in_front = if world_nearer_is_greater_depth() {
                d > nearest_hidden
            } else {
                d < nearest_hidden
            };
            assert!(
                in_front,
                "{dir:?} (depth {d}) must be in front of every culled face ({nearest_hidden})"
            );
        }
    }

    #[test]
    fn the_two_y_flips_cancel_against_the_world_convention() {
        // The determinant restates the winding result compactly. The invariant
        // is "**same sign** as the world path", never a fixed polarity: the
        // projection's own 4x4 sign follows which end of `[0, 1]` the near plane
        // is at (negative under a forward projection, positive under
        // reversed-Z), and neither is the one the rasterizer reads.
        let camera = Camera::default();
        let world = camera.view_projection().determinant();
        let gui = (gui_ortho(256, 256)
            * gui_item_pose([0.0, 0.0, 16.0, 16.0], &vanilla_block_gui()))
        .determinant();
        assert_eq!(
            world.signum(),
            gui.signum(),
            "GUI and world matrices must agree on handedness (world {world}, gui {gui})"
        );

        // And the flips are genuinely two, stated as the axis reversals they
        // are rather than as determinant polarities — a determinant folds the
        // `y` flip together with `gui_ortho`'s `z` direction, so its sign moves
        // when the depth convention does and says nothing about `y`.
        let ortho = gui_ortho(256, 256);
        assert!(
            ortho.transform_vector3(Vec3::Y).y < 0.0,
            "gui_ortho must send +y (down the screen) to -y in NDC"
        );
        let pose = gui_item_pose([0.0, 0.0, 16.0, 16.0], &vanilla_block_gui());
        assert!(
            pose.transform_vector3(Vec3::Y).y < 0.0,
            "the pose's scale(w, -h, d) must send model +y (up) to -y in pixels"
        );
        assert!(
            pose.determinant() < 0.0,
            "the pose's scale(w, -h, d) is orientation-reversing"
        );
        // And the cancellation is load-bearing rather than incidental: undo just
        // one of the two flips and the composition stops agreeing with the world
        // path. This is the control, and it is what "genuinely two" means — an
        // assertion on either half's own 4x4 determinant is not, because
        // `gui_ortho` folds its y flip together with its depth direction and so
        // changes sign whenever the depth convention does.
        let unflipped = ortho * Mat4::from_scale(Vec3::new(1.0, -1.0, 1.0)) * pose;
        assert_eq!(
            unflipped.determinant().signum(),
            -world.signum(),
            "control failed: removing one y flip must invert the composition's \
             handedness, or this test cannot see a missing flip"
        );
    }

    // -----------------------------------------------------------------------
    // ItemStateContext: which variant a draw resolves to
    // -----------------------------------------------------------------------

    /// `assets/minecraft/items/bow.json`, byte-for-byte from the 26.2 client jar.
    ///
    /// The fixture is the *authority*, not a restatement of our resolver: the
    /// thresholds (`0.65`, `0.9`) and the `scale` (`0.05`) are Mojang's numbers,
    /// and the expected tick crossings below are derived from them by hand
    /// (`0.65 / 0.05 = 13`, `0.9 / 0.05 = 18`) rather than by running the code.
    const BOW_JSON: &[u8] = br#"{
      "model": {
        "type": "minecraft:condition",
        "on_false": { "type": "minecraft:model", "model": "minecraft:item/bow" },
        "on_true": {
          "type": "minecraft:range_dispatch",
          "entries": [
            { "model": { "type": "minecraft:model", "model": "minecraft:item/bow_pulling_1" }, "threshold": 0.65 },
            { "model": { "type": "minecraft:model", "model": "minecraft:item/bow_pulling_2" }, "threshold": 0.9 }
          ],
          "fallback": { "type": "minecraft:model", "model": "minecraft:item/bow_pulling_0" },
          "property": "minecraft:use_duration",
          "scale": 0.05
        },
        "property": "minecraft:using_item"
      }
    }"#;

    /// Which model `bow.json` names, resolved through a real [`ItemStateContext`].
    fn bow_model_at(ctx: ItemStateContext) -> String {
        let def = lodestone_assets::ItemModel::parse(BOW_JSON).expect("parse bow.json");
        match def.resolve(&ctx).as_slice() {
            [lodestone_assets::ItemModelOutput::Model { model, .. }] => model.to_string(),
            other => panic!("expected exactly one model output, got {other:?}"),
        }
    }

    /// The bow's four forms and the exact ticks they cross at.
    ///
    /// This is the gate the existing pose gates cannot be: every pose and every
    /// variant is the identity at `using == false`, so
    /// `first_person_hand_light_pixels` and `thrown_and_held_item_pixels` pass
    /// whether or not this wiring exists. `using` is driven **true** here.
    #[test]
    fn the_bow_crosses_at_thirteen_and_eighteen_ticks() {
        let drawing = |ticks: u32| {
            bow_model_at(ItemStateContext::new(DisplaySlot::FirstPersonRightHand).with_use(true, ticks))
        };
        // Not using at all: the slack bow, whatever the counter says. `ItemUse`
        // holds `ticks` at 0 while `!using`, but the gate must not depend on that.
        assert_eq!(
            bow_model_at(
                ItemStateContext::new(DisplaySlot::FirstPersonRightHand).with_use(false, 40)
            ),
            "minecraft:item/bow"
        );
        // 0..=12: 12 * 0.05 = 0.60, below the 0.65 threshold -> the fallback.
        for ticks in [0, 1, 6, 12] {
            assert_eq!(drawing(ticks), "minecraft:item/bow_pulling_0", "at {ticks} ticks");
        }
        // 13..=17: 13 * 0.05 = 0.65 exactly, 17 * 0.05 = 0.85 < 0.9.
        for ticks in [13, 14, 17] {
            assert_eq!(drawing(ticks), "minecraft:item/bow_pulling_1", "at {ticks} ticks");
        }
        // >= 18: 18 * 0.05 = 0.90 exactly. A full draw is 20 ticks and a bow's
        // `getUseDuration` is 72000, so the top entry has to hold indefinitely.
        for ticks in [18, 19, 20, 72_000] {
            assert_eq!(drawing(ticks), "minecraft:item/bow_pulling_2", "at {ticks} ticks");
        }
    }

    /// A server custom item can retain the base item id (a diamond sword here)
    /// and differ only by `minecraft:custom_model_data`. The numeric property is
    /// deliberately checked while the item is *not* in use: otherwise a hand
    /// test would pass while GUI and resting-hand guns still resolved to the
    /// ordinary sword.
    #[test]
    fn custom_model_data_range_dispatch_distinguishes_a_gun_from_a_plain_sword() {
        let definition = lodestone_assets::ItemModel::parse(
            br#"{
              "model": {
                "type": "minecraft:range_dispatch",
                "property": "minecraft:custom_model_data",
                "entries": [{
                  "threshold": 4545,
                  "model": { "type": "minecraft:model", "model": "democracycraft:item/gun" }
                }],
                "fallback": { "type": "minecraft:model", "model": "minecraft:item/diamond_sword" }
              }
            }"#,
        )
        .expect("parse item definition");
        let resolved = |data| match definition.resolve(&data).as_slice() {
            [lodestone_assets::ItemModelOutput::Model { model, .. }] => model.to_string(),
            other => panic!("expected exactly one model output, got {other:?}"),
        };

        assert_eq!(
            resolved(ItemStateContext::new(DisplaySlot::Gui)),
            "minecraft:item/diamond_sword",
            "a plain diamond sword must keep the fallback model"
        );
        assert_eq!(
            resolved(
                ItemStateContext::new(DisplaySlot::FirstPersonRightHand)
                    .with_custom_model_data(4545.0),
            ),
            "democracycraft:item/gun",
            "the metadata-carrying diamond sword must select the gun model"
        );
    }

    /// The inversion trap, as a control: had `use_duration` been fed
    /// `getUseItemRemainingTicks()`-style (counting **down** from a duration)
    /// instead of `ItemUse::ticks`, every crossing above would land on a different
    /// model. Asserting the wrong hypothesis *fails* is what makes the right one
    /// evidence rather than a coincidence.
    #[test]
    fn feeding_the_counter_backwards_would_pin_the_bow_at_full_draw() {
        // A bow's `getUseDuration` is 72000; `duration - ticks` at 6 ticks in is
        // 71994, which sails past every threshold.
        let inverted = ItemStateContext::new(DisplaySlot::FirstPersonRightHand)
            .with_use(true, 72_000 - 6);
        assert_eq!(bow_model_at(inverted), "minecraft:item/bow_pulling_2");
        // Where the correct feed says a barely-drawn bow.
        let correct =
            ItemStateContext::new(DisplaySlot::FirstPersonRightHand).with_use(true, 6);
        assert_eq!(bow_model_at(correct), "minecraft:item/bow_pulling_0");
    }

    /// `crossbow/pull` is `useDuration / getChargeDuration`, so its own thresholds
    /// (0.58 and 1.0, from `items/crossbow.json`) land at ticks 15 and 25.
    #[test]
    fn the_crossbow_pull_fraction_divides_by_the_charge_duration() {
        let ctx = |ticks| ItemStateContext::new(DisplaySlot::ThirdPersonRightHand).with_use(true, ticks);
        // 0.58 * 25 = 14.5, so 14 ticks is still below and 15 is above.
        assert!(ctx(14).range("minecraft:crossbow/pull") < 0.58);
        assert!(ctx(15).range("minecraft:crossbow/pull") >= 0.58);
        // Fully wound at the charge duration itself, not before it.
        assert!(ctx(24).range("minecraft:crossbow/pull") < 1.0);
        assert!(ctx(25).range("minecraft:crossbow/pull") >= 1.0);
        // Not in use: zero, matching vanilla's `getUseItem() != itemStack` return.
        assert_eq!(
            ItemStateContext::new(DisplaySlot::ThirdPersonRightHand)
                .range("minecraft:crossbow/pull"),
            0.0
        );
    }

    /// The context must report each slot's **own** JSON key for
    /// `minecraft:display_context`, because that string is the `when` a `select`
    /// case matches on: one wrong key silently sends every branching item down its
    /// `fallback`, which looks exactly like the flattening this replaced.
    ///
    /// The slots themselves come from
    /// [`Arm::display_slot`](crate::entity::Arm::display_slot) at the call sites —
    /// the *same* expression `hand_transform` uses to read the pose — so there is
    /// deliberately no second mapping here to disagree with it.
    #[test]
    fn every_display_context_reports_its_own_key() {
        use std::collections::BTreeSet;
        let mut names = BTreeSet::new();
        for slot in DisplaySlot::ALL {
            let reported = ItemStateContext::new(slot).select(DISPLAY_CONTEXT_PROPERTY);
            assert_eq!(
                reported.as_deref(),
                Some(slot.json_name()),
                "{slot:?} must report its own key"
            );
            names.insert(slot.json_name());
        }
        assert_eq!(
            names.len(),
            DisplaySlot::ALL.len(),
            "the nine contexts have nine distinct keys"
        );
        // Spot-check against `ItemDisplayContext.getSerializedName()` verbatim, so
        // the loop above cannot be satisfied by nine consistently wrong keys.
        assert!(names.contains("firstperson_righthand"));
        assert!(names.contains("thirdperson_righthand"));
        assert!(names.contains("gui"));
        assert!(names.contains("ground"));
        assert!(names.contains("on_shelf"));
    }

    /// Every property this context cannot source must read as *unset*, so a
    /// `condition` takes `on_false` and a `select` takes `fallback` — never a
    /// plausible-looking guess. Named explicitly so adding a source to one of them
    /// is a deliberate act with a test to update.
    #[test]
    fn unsourced_properties_read_as_unset() {
        let ctx = ItemStateContext::new(DisplaySlot::Gui).with_use(true, 40);
        for property in [
            "minecraft:broken",
            "minecraft:bundle/has_selected_item",
            "minecraft:fishing_rod/cast",
            "minecraft:has_component",
        ] {
            assert!(!ctx.condition(property, None), "{property} must be unset");
        }
        for property in [
            "minecraft:trim_material",
            "minecraft:block_state",
            "minecraft:charge_type",
            "minecraft:context_dimension",
        ] {
            assert_eq!(ctx.select(property), None, "{property} must be unset");
        }
        for property in [
            "minecraft:use_cycle",
            "minecraft:time",
            "minecraft:local_time",
            "minecraft:compass",
            "minecraft:damage",
            "minecraft:count",
            "minecraft:cooldown",
        ] {
            assert_eq!(ctx.range(property), 0.0, "{property} must be unset");
        }
        // But the two that *are* sourced must not be swept up in that: a context
        // where everything reads unset would pass the block above vacuously.
        assert_eq!(ctx.range("minecraft:use_duration"), 40.0);
        assert!(ctx.condition("minecraft:using_item", None));
    }
}
