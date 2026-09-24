//! Posing a **living entity** into a 2-D GUI rect — vanilla's
//! inventory-screen entity-follows-mouse extraction function, i.e. the player
//! standing in the inventory panel with their head tracking the cursor.
//!
//! This is the entity counterpart of [`item_render`](crate::item_render): pure
//! matrix construction, no GPU, no state. The item path answers "where does a
//! mini-block go in a 16×16 slot"; this one answers "where does a 30-unit-tall
//! humanoid go in a 49×70 recess, and which way is it looking". Both land in the
//! same **GUI pixel space** ([`gui_ortho`](crate::gui_ortho)'s input: `x` right,
//! `y` **down**, larger `z` **nearer**), so the composed matrix drops straight
//! into [`EntityPipeline`](crate::EntityPipeline)'s group-0 `view_proj` with no
//! new pipeline, shader or bind group.
//!
//! # The record definition
//!
//! Vanilla's inventory-screen entity-placement function (26.2), read rather than summarised:
//!
//! ```text
//! centerX = (x0 + x1) / 2      xAngle = atan((centerX - mouseX) / 40)
//! centerY = (y0 + y1) / 2      yAngle = atan((centerY - mouseY) / 40)
//! rotation = rotateZ(PI).mul(rotateX(yAngle * 20°))
//! bodyRot  = 180 + xAngle * 20
//! yRot     =       xAngle * 20        // NOT absolute — see below
//! xRot     =     - yAngle * 20        // 0 when pose == FALL_FLYING
//! translation = (0, boundingBoxHeight / 2 + offsetY, 0)
//! graphics.entity(state, size, translation, rotation, rotation, x0, y0, x1, y1)
//! ```
//!
//! **`bodyRot` and `yRot` are not the same kind of number, and reading them as
//! two absolute yaws 180° apart is the trap.** Vanilla's living-entity render-state
//! extraction function
//! fills the render state with `state.yRot = wrapDegrees(headRot - state.bodyRot)`
//! — `yRot` is the head yaw *relative to the body* (vanilla's `netHeadYaw`),
//! `bodyRot` is absolute. So the assignment above puts the body at `180 + a` and
//! the head at `180 + 2a` in absolute terms: **the head really does track twice
//! as far as the body**, and that over-rotation plus [`GuiEntityLook::head_pitch_deg`]
//! is what produces the "the eyes follow you" read. Two absolute yaws differing
//! by a constant 180° would instead draw a player permanently looking over their
//! own shoulder.
//!
//! # Why there is no offscreen texture here
//!
//! 26.2 renders this through its picture-in-picture renderer base class: an offscreen
//! `(x1-x0) * guiScale × (y1-y0) * guiScale` colour+depth pair, an ortho over
//! *that* texture, then a premultiplied-alpha blit into the rect. Its model-view
//! is `translate(w/2, h/2, 0) · scale(s, s, -s)` with `s = guiScale * size`
//! (that base class's setup function, and vanilla's GUI-entity-renderer
//! translate-Y accessor).
//!
//! Every term there is proportional to `guiScale`, so the whole thing collapses
//! to one matrix in **logical** GUI pixels with `s = size` — which is the space
//! this crate's GUI path already works in. The offscreen target then buys exactly
//! one thing: clipping. A `set_scissor_rect` over the same rect gives the same
//! clip for an opaque/cutout entity pass, for one pass instead of a texture, a
//! blit pipeline and a resize dance. That is the deliberate divergence; the
//! matrices are vanilla's.
//!
//! # The two flips that cancel, and the one that must not
//!
//! [`gui_entity_pose`] composes vanilla's `rotateZ(PI)` over
//! [`entity_model_matrix`]'s `scale(-1, -1, 1)`, and `Rz(π) · S(-1,-1,1) = I` at
//! zero mouse offset. That is not a coincidence to optimise away — it is *why*
//! vanilla rotates by π at all: vanilla's living-entity renderer flips the rig so a
//! `+Y`-up world can draw a `Y`-**down** mesh, and the GUI is already `y`-down,
//! so the flip has to be undone. Cancelling them leaves the baked mesh's own
//! `Y`-down frame mapping directly onto GUI `y`-down: head up, feet down.
//!
//! The `z` flip must **not** cancel. `S(size, size, -size)` maps the rig's front
//! (mesh `-Z`, per [`entity_model_matrix`]'s yaw convention) onto a *larger* GUI
//! `z`, which [`gui_ortho`](crate::gui_ortho) makes *nearer*. Drop the minus —
//! the obvious `Mat4::from_scale(Vec3::splat(size))` — and the face loses the
//! depth test to the back of the skull: you see the inside of the far side of the
//! head, which reads as odd shading rather than as an obviously broken draw.
//! `the_pose_winds_like_the_world_camera` and
//! `the_face_is_nearer_than_the_back_of_the_head_in_both_arms` are what hold that
//! down, and both fail under the naive scale — see this module's tests.
//!
//! [`entity_model_matrix`]: crate::entity_model_matrix

use glam::{Mat4, Vec3};

use crate::entity::entity_model_matrix;
use crate::entity_anim::AnimInput;

/// The `40.0` in `atan((centerX - mouseX) / 40.0F)` — how many GUI pixels of
/// cursor travel amount to one radian of raw look angle before the `20`
/// multiplier. Vanilla's inventory-screen entity-placement function.
pub const MOUSE_ANGLE_DIVISOR: f32 = 40.0;

/// The `20.0F` every look angle is multiplied by. Vanilla multiplies the
/// **radian** output of `atan` by this and then treats the product as
/// *degrees* (same inventory-screen entity-placement function) — a genuine unit mix in the
/// original, reproduced verbatim rather than "corrected", because correcting it
/// would change the swivel by a factor of `180/π`.
pub const LOOK_ANGLE_SCALE_DEG: f32 = 20.0;

/// Vanilla's inventory screen's own recess: the avatar rect is
/// `(leftPos + 26, topPos + 8)` to `(leftPos + 75, topPos + 78)`
/// (same entity-placement function), i.e. this offset and [`INVENTORY_RECT_SIZE`].
pub const INVENTORY_RECT_OFFSET: [f32; 2] = [26.0, 8.0];

/// The size half of [`INVENTORY_RECT_OFFSET`]: `75 - 26` by `78 - 8`.
pub const INVENTORY_RECT_SIZE: [f32; 2] = [49.0, 70.0];

/// Vanilla's inventory screen's `size` argument, `30` (same entity-placement
/// function) — GUI
/// pixels per block of entity height.
pub const INVENTORY_SIZE: f32 = 30.0;

/// Vanilla's inventory screen's `offsetY`, `0.0625F` (same entity-placement
/// function) — one
/// sixteenth of a block of extra lift, in *entity* units, applied on top of
/// `boundingBoxHeight / 2`.
pub const INVENTORY_OFFSET_Y: f32 = 0.0625;

/// Where a GUI-posed living entity is looking, derived from the cursor.
///
/// Four numbers rather than two because they land in three different places:
/// [`body_yaw_deg`](Self::body_yaw_deg) selects the whole-body **placement**
/// (through [`entity_model_matrix`]), the two head angles drive the
/// **skeleton** (through [`AnimInput`]), and
/// [`camera_pitch_deg`](Self::camera_pitch_deg) tilts the **view** itself.
/// Collapsing any pair would silently drop one of the three effects.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GuiEntityLook {
    /// Absolute body yaw in degrees, Minecraft convention (`0` faces `+Z`) —
    /// vanilla's living-entity render state's body-rotation field, `180 + xAngle * 20`. `180`
    /// is "turned to face the viewer".
    pub body_yaw_deg: f32,
    /// Head yaw **relative to the body**, in degrees — vanilla's
    /// living-entity render state's y-rotation field, which
    /// vanilla's living-entity render-state extraction function defines as
    /// `wrapDegrees(headRot - bodyRot)`. Feeds [`AnimInput::head_yaw_deg`]
    /// directly, same convention, same sign.
    pub head_yaw_deg: f32,
    /// Head pitch in degrees, positive looking **down** — vanilla's
    /// living-entity render state's x-rotation field, `-yAngle * 20`, forced to `0` for a
    /// `FALL_FLYING` pose. Feeds [`AnimInput::head_pitch_deg`].
    ///
    /// This is the field that makes the eyes follow the cursor vertically, and
    /// the only one the `FALL_FLYING` branch touches.
    pub head_pitch_deg: f32,
    /// The `rotateX` folded into the *view*, in degrees: `+yAngle * 20`, the
    /// exact negation of [`head_pitch_deg`](Self::head_pitch_deg) — and
    /// **not** zeroed while fall-flying, because vanilla builds `rotation`
    /// before it ever looks at the pose (its inventory-screen entity-placement
    /// function).
    pub camera_pitch_deg: f32,
}

impl GuiEntityLook {
    /// Looking straight out of the screen: the value at a cursor exactly on the
    /// rect's centre, and the honest default for a caller with no cursor at all
    /// (a headless gate, a screen drawn before the first mouse event).
    pub const FORWARD: GuiEntityLook = GuiEntityLook {
        body_yaw_deg: 180.0,
        head_yaw_deg: 0.0,
        head_pitch_deg: 0.0,
        camera_pitch_deg: 0.0,
    };
}

/// The look angles for a cursor at `mouse_px` over the rect `rect_px`
/// (`[x, y, w, h]`, top-left origin, **logical GUI pixels**).
///
/// `fall_flying` is `renderState.pose == Pose.FALL_FLYING`, which zeroes the
/// head pitch and nothing else.
#[must_use]
pub fn gui_entity_look(rect_px: [f32; 4], mouse_px: [f32; 2], fall_flying: bool) -> GuiEntityLook {
    let [x, y, w, h] = rect_px;
    let centre_x = x + w * 0.5;
    let centre_y = y + h * 0.5;
    // Vanilla's own `Math.atan` of a pixel ratio: radians, then scaled by 20 and
    // read as degrees. See `LOOK_ANGLE_SCALE_DEG`.
    let x_angle = ((centre_x - mouse_px[0]) / MOUSE_ANGLE_DIVISOR).atan();
    let y_angle = ((centre_y - mouse_px[1]) / MOUSE_ANGLE_DIVISOR).atan();
    GuiEntityLook {
        body_yaw_deg: 180.0 + x_angle * LOOK_ANGLE_SCALE_DEG,
        head_yaw_deg: x_angle * LOOK_ANGLE_SCALE_DEG,
        head_pitch_deg: if fall_flying {
            0.0
        } else {
            -y_angle * LOOK_ANGLE_SCALE_DEG
        },
        camera_pitch_deg: y_angle * LOOK_ANGLE_SCALE_DEG,
    }
}

/// The **view** half: entity space (post-[`entity_model_matrix`], i.e. `+Y` up,
/// feet at the origin) → GUI pixel space.
///
/// ```text
/// T(centre of rect) · S(size, size, -size) · T(0, bb_height/2 + offset_y, 0) · Rz(π) · Rx(camera pitch)
/// ```
///
/// Read right to left that is vanilla's stack order: its GUI-entity-renderer's
/// `translate(translation)` then `mulPose(rotation)` sit *inside*
/// its picture-in-picture renderer base class's `translate(w/2, h/2, 0)` then
/// `scale(s, s, -s)`, and a `PoseStack` right-multiplies.
///
/// Separate from [`gui_entity_pose`] so a caller can pose something that is not
/// placed by [`entity_model_matrix`] (a block-entity rig, a bare model
/// as vanilla's GUI-graphics skin-drawing function does) against the same view.
#[must_use]
pub fn gui_entity_view(
    rect_px: [f32; 4],
    size: f32,
    offset_y: f32,
    bb_height: f32,
    look: &GuiEntityLook,
) -> Mat4 {
    let [x, y, w, h] = rect_px;
    let centre = Vec3::new(x + w * 0.5, y + h * 0.5, 0.0);
    Mat4::from_translation(centre)
        // The `z` negation is load-bearing and is not a uniform scale. See the
        // module docs: without it the back of the head wins the depth test.
        * Mat4::from_scale(Vec3::new(size, size, -size))
        * Mat4::from_translation(Vec3::new(0.0, bb_height * 0.5 + offset_y, 0.0))
        * Mat4::from_rotation_z(std::f32::consts::PI)
        * Mat4::from_rotation_x(look.camera_pitch_deg.to_radians())
}

/// The full **baked mesh → GUI pixel space** matrix for a living entity drawn
/// into `rect_px`: [`gui_entity_view`] over [`entity_model_matrix`].
///
/// `bb_height` is the entity's `boundingBoxHeight` in blocks (`1.8` for a
/// standing player) — vanilla divides it by the render state's `scale` and then
/// sets that scale to `1.0`, so a caller passing an already-unscaled height and
/// leaving the model scale at `1.0` reproduces both lines.
///
/// Composing with [`entity_model_matrix`] rather than restating its ops is the
/// point: the inventory avatar and every mob in the world then share one
/// definition of "where does a baked rig sit relative to its feet", so the
/// `MODEL_FEET_OFFSET` lift and the rig flip cannot drift between them. Feed the
/// result to [`gui_ortho`](crate::gui_ortho) as the projection and this as the
/// per-instance transform — or, since the entity pipeline is instanced per
/// *part*, premultiply it onto an [`EntityInstance`](crate::EntityInstance)
/// built at the origin with this look's [`body_yaw_deg`](GuiEntityLook::body_yaw_deg).
#[must_use]
pub fn gui_entity_pose(
    rect_px: [f32; 4],
    size: f32,
    offset_y: f32,
    bb_height: f32,
    look: &GuiEntityLook,
) -> Mat4 {
    gui_entity_view(rect_px, size, offset_y, bb_height, look)
        * entity_model_matrix(Vec3::ZERO, look.body_yaw_deg, 1.0)
}

/// `base` with this look's two head angles applied — the [`AnimInput`] half of
/// the pose, which is what actually turns the skull.
///
/// Takes a `base` rather than starting from [`AnimInput::REST`] so a caller that
/// has real crouch/swing/age state for the player keeps it: vanilla poses the
/// *live* render state, not a rest pose, and only overwrites the three rotation
/// fields.
#[must_use]
pub fn gui_entity_anim(look: &GuiEntityLook, base: AnimInput) -> AnimInput {
    AnimInput {
        head_yaw_deg: look.head_yaw_deg,
        head_pitch_deg: look.head_pitch_deg,
        ..base
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera::Camera;
    use crate::entity::{EntityModelSet, MODEL_FEET_OFFSET, player_model_name};
    use crate::gui_ortho;

    /// Vanilla's inventory screen's rect at a panel origin of `(0, 0)`, derived from the
    /// published constants rather than restated.
    const RECT: [f32; 4] = [
        INVENTORY_RECT_OFFSET[0],
        INVENTORY_RECT_OFFSET[1],
        INVENTORY_RECT_SIZE[0],
        INVENTORY_RECT_SIZE[1],
    ];
    /// A standing player's `boundingBoxHeight`.
    const BB: f32 = 1.8;
    /// The canvas every projection below is taken against. Any size works — the
    /// assertions are all either signs or ratios within the rect.
    const CANVAS: (u32, u32) = (854, 480);

    fn rect_centre() -> [f32; 2] {
        [RECT[0] + RECT[2] * 0.5, RECT[1] + RECT[3] * 0.5]
    }

    /// The pose a naive port produces: `scale(size, size, size)` instead of
    /// `scale(size, size, -size)`. Everything else identical, so any assertion
    /// that separates the two is separating *exactly* the `z` flip.
    fn wrong_z_pose(look: &GuiEntityLook) -> Mat4 {
        let centre = Vec3::new(rect_centre()[0], rect_centre()[1], 0.0);
        Mat4::from_translation(centre)
            * Mat4::from_scale(Vec3::splat(INVENTORY_SIZE))
            * Mat4::from_translation(Vec3::new(0.0, BB * 0.5 + INVENTORY_OFFSET_Y, 0.0))
            * Mat4::from_rotation_z(std::f32::consts::PI)
            * Mat4::from_rotation_x(look.camera_pitch_deg.to_radians())
            * entity_model_matrix(Vec3::ZERO, look.body_yaw_deg, 1.0)
    }

    fn pose(look: &GuiEntityLook) -> Mat4 {
        gui_entity_pose(RECT, INVENTORY_SIZE, INVENTORY_OFFSET_Y, BB, look)
    }

    // -----------------------------------------------------------------------
    // Winding
    // -----------------------------------------------------------------------

    /// **The winding gate.** `CLAUDE.md`: the composed GUI matrix's determinant
    /// must have the *same* sign as a real camera's `view_projection`, and that
    /// sign is not asserted as a polarity here — it is **read off a camera**,
    /// exactly as the rule demands.
    ///
    /// The arm this compares against is the tighter one available: not the bare
    /// camera, but the camera composed with [`entity_model_matrix`] — the
    /// world-path matrix that draws *this same mesh* through *this same*
    /// `EntityPipeline`. So the claim is "an inventory avatar winds the way a mob
    /// does", which is the property that matters, rather than a statement about
    /// a projection in isolation.
    #[test]
    fn the_pose_winds_like_the_world_camera() {
        let look = gui_entity_look(RECT, [40.0, 30.0], false);
        let camera = Camera {
            position: Vec3::new(0.0, 1.62, 4.0),
            yaw: 180.0,
            ..Camera::default()
        };
        let world = camera.view_projection() * entity_model_matrix(Vec3::ZERO, 0.0, 1.0);
        let gui = gui_ortho(CANVAS.0, CANVAS.1) * pose(&look);

        let world_det = world.determinant();
        let gui_det = gui.determinant();
        assert!(
            world_det.abs() > 0.0 && gui_det.abs() > 0.0,
            "a degenerate determinant proves nothing: world {world_det:+e}, gui {gui_det:+e}"
        );
        assert_eq!(
            world_det.is_sign_negative(),
            gui_det.is_sign_negative(),
            "the GUI avatar matrix must wind like the world entity path it shares a \
             pipeline with. world det {world_det:+e}, gui det {gui_det:+e}"
        );
    }

    /// The control for [`the_pose_winds_like_the_world_camera`]: the naive
    /// uniform `size` scale flips the determinant, so the assertion above is not
    /// vacuously true of any matrix built here.
    #[test]
    fn dropping_the_z_flip_reverses_the_winding() {
        let look = gui_entity_look(RECT, [40.0, 30.0], false);
        let ortho = gui_ortho(CANVAS.0, CANVAS.1);
        let right = (ortho * pose(&look)).determinant();
        let wrong = (ortho * wrong_z_pose(&look)).determinant();
        assert!(
            right.is_sign_negative() != wrong.is_sign_negative(),
            "the control must reverse the winding or the winding gate is measuring \
             nothing: right {right:+e}, wrong {wrong:+e}"
        );
    }

    // -----------------------------------------------------------------------
    // Depth: the face is nearer than the back of the head
    // -----------------------------------------------------------------------

    /// Clip-space depth of a mesh-space point through `m`, in this engine's
    /// `[0, 1]` DirectX-style range where **smaller is nearer**.
    fn clip_depth(m: Mat4, p: Vec3) -> f32 {
        let c = m * p.extend(1.0);
        c.z / c.w
    }

    /// **A cross-arm depth invariant, and the one that catches the `z` flip in
    /// terms of a visible symptom rather than a sign.**
    ///
    /// The expectation comes from neither arm: it is that a viewer looking at the
    /// *front* of a player sees the front of the head nearer than the back,
    /// whichever projection they are looking through. Arm A is a real world
    /// [`Camera`] placed in front of a yaw-`0` entity; arm B is the inventory
    /// pose at a centred cursor (`bodyRot == 180`, likewise face-on). Both are
    /// independent constructions of the same physical fact.
    ///
    /// Which mesh `z` *is* the front is derived, not assumed:
    /// [`entity_model_matrix`] at yaw `0` maps mesh `-Z` to world `+Z`, and
    /// Minecraft's yaw `0` faces `+Z`, so the front of the rig is at negative
    /// mesh `z`. The `0.25` is a quarter block — the head cuboid is
    /// `[-4, -8, -4] + [8, 8, 8]` in sixteenths (`entity.rs`'s `player_model`),
    /// so `±4/16` is exactly its front and back faces.
    /// Is `a` the depth of a surface **nearer the eye** than `b`?
    ///
    /// Derived from the real [`Camera`] rather than written as `a < b`, because
    /// which end of `[0, 1]` the near plane sits at is a property of
    /// [`Camera::projection_matrix`] and it has changed once already: the
    /// projection is reversed-Z, so nearer is *greater*. Three assertions below
    /// depend on this and all three read as obviously correct written the wrong
    /// way round.
    fn nearer(a: f32, b: f32) -> bool {
        let camera = Camera {
            position: Vec3::new(0.0, 0.0, 4.0),
            yaw: 180.0,
            ..Camera::default()
        };
        let vp = camera.view_projection();
        let close = clip_depth(vp, Vec3::new(0.0, 0.0, 1.0));
        let distant = clip_depth(vp, Vec3::ZERO);
        assert_ne!(
            close, distant,
            "premise: two points a block apart projected to one depth, so this \
             module cannot tell which way is toward the eye"
        );
        if close > distant { a > b } else { a < b }
    }

    #[test]
    fn the_face_is_nearer_than_the_back_of_the_head_in_both_arms() {
        // A point in the middle of the head, on the front face and on the back
        // face. Mesh frame: blocks, `Y` down, so `y = -0.25` is mid-skull.
        let front = Vec3::new(0.0, -0.25, -0.25);
        let back = Vec3::new(0.0, -0.25, 0.25);

        // Arm A: a world camera 4 blocks south (+Z) of the entity, looking north.
        // A yaw-0 entity faces +Z, so this camera sees its face.
        let camera = Camera {
            position: Vec3::new(0.0, 1.4, 4.0),
            yaw: 180.0,
            ..Camera::default()
        };
        let world = camera.view_projection()
            * entity_model_matrix(Vec3::ZERO, 0.0, 1.0)
            // The mesh sits in the rig frame; `entity_model_matrix` already
            // carries the lift, so these mesh points go in directly.
            * Mat4::IDENTITY;
        let world_front = clip_depth(world, front);
        let world_back = clip_depth(world, back);
        assert!(
            nearer(world_front, world_back),
            "premise-false control: this world camera does not even see the face \
             nearer than the back of the head (front {world_front}, back {world_back}), \
             so it cannot be the outside arm for the GUI claim"
        );

        // Arm B: the inventory pose, cursor dead centre.
        let look = gui_entity_look(RECT, rect_centre(), false);
        assert_eq!(
            look, GuiEntityLook::FORWARD,
            "a centred cursor must be the forward look, or arm B is not face-on"
        );
        let gui = gui_ortho(CANVAS.0, CANVAS.1) * pose(&look);
        let gui_front = clip_depth(gui, front);
        let gui_back = clip_depth(gui, back);
        assert!(
            nearer(gui_front, gui_back),
            "the inventory avatar must show its face, not the inside of the back of \
             its skull: front depth {gui_front}, back depth {gui_back}"
        );

        // And the control: the naive uniform scale reverses exactly this.
        let wrong = gui_ortho(CANVAS.0, CANVAS.1) * wrong_z_pose(&look);
        let wrong_front = clip_depth(wrong, front);
        let wrong_back = clip_depth(wrong, back);
        assert!(
            nearer(wrong_back, wrong_front),
            "the control must put the back of the head in front, or this gate is \
             insensitive to the z flip: front {wrong_front}, back {wrong_back}"
        );
    }

    // -----------------------------------------------------------------------
    // Location: the avatar lands inside its recess
    // -----------------------------------------------------------------------

    /// Project a mesh point to **logical GUI pixels** through `m`, by inverting
    /// [`gui_ortho`]'s NDC mapping. Asserting in pixels rather than NDC is what
    /// lets the failure message print a rect the reader can compare against
    /// vanilla's inventory screen's own numbers.
    fn to_gui_px(m: Mat4, p: Vec3) -> [f32; 2] {
        let c = m * p.extend(1.0);
        let ndc = c.truncate() / c.w;
        [
            (ndc.x + 1.0) * 0.5 * CANVAS.0 as f32,
            (1.0 - ndc.y) * 0.5 * CANVAS.1 as f32,
        ]
    }

    /// **Measure by location, and print a bounding box.** The whole player mesh,
    /// AABB corners projected, must land inside the 49×70 recess — and must fill
    /// a real fraction of it, so a collapsed or off-by-a-scale draw fails too.
    ///
    /// The subject is the *real* baked mesh's `local_min`/`local_max`, not a
    /// stand-in cuboid, which is what makes this a statement about what actually
    /// draws. Predicted, not merely bounded: the arithmetic is
    /// `S(30) · T(0, 0.9625 - 1.501, 0)` over a rig spanning `y ∈ [-0.5, 1.5]`,
    /// i.e. roughly 60 of the 70 rows.
    #[test]
    fn the_whole_avatar_lands_inside_the_inventory_recess() {
        let models = EntityModelSet::load();
        let name = player_model_name(false);
        let mesh = models
            .get(name)
            .unwrap_or_else(|| panic!("the corpus must carry {name}"));
        let look = gui_entity_look(RECT, rect_centre(), false);
        let m = gui_ortho(CANVAS.0, CANVAS.1) * pose(&look);

        let (lo, hi) = (mesh.local_min, mesh.local_max);
        let mut min = [f32::MAX, f32::MAX];
        let mut max = [f32::MIN, f32::MIN];
        for i in 0..8 {
            let corner = Vec3::new(
                if i & 1 == 0 { lo.x } else { hi.x },
                if i & 2 == 0 { lo.y } else { hi.y },
                if i & 4 == 0 { lo.z } else { hi.z },
            );
            let px = to_gui_px(m, corner);
            min[0] = min[0].min(px[0]);
            min[1] = min[1].min(px[1]);
            max[0] = max[0].max(px[0]);
            max[1] = max[1].max(px[1]);
        }
        let bbox = format!(
            "drawn bbox x {:.2}..{:.2}, y {:.2}..{:.2}; recess x {:.2}..{:.2}, y {:.2}..{:.2}",
            min[0],
            max[0],
            min[1],
            max[1],
            RECT[0],
            RECT[0] + RECT[2],
            RECT[1],
            RECT[1] + RECT[3]
        );
        assert!(
            min[0] >= RECT[0] && max[0] <= RECT[0] + RECT[2],
            "the avatar overflows its recess horizontally — {bbox}"
        );
        assert!(
            min[1] >= RECT[1] && max[1] <= RECT[1] + RECT[3],
            "the avatar overflows its recess vertically — {bbox}"
        );
        // Vertical fill: the rig spans `[-0.5, 1.5]` blocks plus the hat/overlay
        // grow, at 30 px per block, in a 70 px recess. Anything under 0.75 means
        // the size or the lift was dropped.
        let fill = (max[1] - min[1]) / RECT[3];
        assert!(
            fill > 0.75 && fill <= 1.0,
            "the avatar should fill most of the recess vertically, measured {fill:.3} — {bbox}"
        );
        // And the head is above the feet on screen, which is the `Rz(PI)` /
        // `scale(-1,-1,1)` cancellation the module docs describe. Feet are mesh
        // `y == MODEL_FEET_OFFSET - 1.501 == 0` after the lift, so use the rig's
        // own extremes: the *smallest* mesh `y` is the top of the head.
        let head = to_gui_px(m, Vec3::new(0.0, lo.y, 0.0));
        let feet = to_gui_px(m, Vec3::new(0.0, hi.y, 0.0));
        assert!(
            head[1] < feet[1],
            "the avatar is upside down: head at y {:.2}, feet at y {:.2}. \
             MODEL_FEET_OFFSET is {MODEL_FEET_OFFSET}",
            head[1],
            feet[1]
        );
    }

    // -----------------------------------------------------------------------
    // The head tracks the cursor
    // -----------------------------------------------------------------------

    /// **The feature Matthew asked for, asserted on the posed head part rather
    /// than on the angle field.**
    ///
    /// A point in front of the face (the nose, mesh `-Z`) is projected through
    /// the *real* [`Skeleton::pose`](crate::Skeleton::pose) output for the `head`
    /// part, composed with the real GUI pose. Moving the cursor down must move
    /// that point down on screen; moving it up must move it up.
    ///
    /// The magnitude is **predicted from outside constants**, not merely signed:
    /// with the cursor 20 px below centre the head pitch is
    /// `-atan(-20/40) * 20 = +9.272°` (positive = looking down), so the nose,
    /// `0.25` blocks in front of a pivot `0.25` blocks below the skull top, drops
    /// by `0.25 * sin(9.272°) = 0.0403` blocks at 30 px/block ≈ `1.21` px, on top
    /// of whatever the view's own `Rx` contributes. Both hypotheses are computed
    /// below and the measurement must land on the head-pitch one.
    #[test]
    fn the_head_pitch_follows_the_cursor() {
        let models = EntityModelSet::load();
        let name = player_model_name(false);
        let mesh = models
            .get(name)
            .unwrap_or_else(|| panic!("the corpus must carry {name}"));
        let head = mesh
            .skeleton
            .index_of("head")
            .expect("the player rig must have a head part");

        // The tip of the nose: on the front face of the head cuboid, at eye
        // height within it. Head cuboid is `[-4,-8,-4] + [8,8,8]` sixteenths.
        let nose = Vec3::new(0.0, -0.25, -0.25);
        let centre = rect_centre();

        let screen_y = |mouse: [f32; 2]| -> f32 {
            let look = gui_entity_look(RECT, mouse, false);
            let anim = gui_entity_anim(&look, AnimInput::REST);
            let parts = mesh.skeleton.pose(&anim);
            let m = gui_ortho(CANVAS.0, CANVAS.1) * pose(&look) * parts[head];
            to_gui_px(m, nose)[1]
        };

        let rest = screen_y(centre);
        let below = screen_y([centre[0], centre[1] + 20.0]);
        let above = screen_y([centre[0], centre[1] - 20.0]);

        assert!(
            below > rest,
            "cursor below centre must tip the nose down the screen: rest {rest:.3}, \
             below {below:.3}"
        );
        assert!(
            above < rest,
            "cursor above centre must tip the nose up the screen: rest {rest:.3}, \
             above {above:.3}"
        );

        // Magnitude, against the two hypotheses. `head_pitch` is the correct one;
        // `no_head_pitch` is the suspected-wrong "the view tilts but the skull
        // does not" build, i.e. `gui_entity_anim` never applied.
        let look_below = gui_entity_look(RECT, [centre[0], centre[1] + 20.0], false);
        assert!(
            (look_below.head_pitch_deg - 9.2729_f32).abs() < 1e-3,
            "the record definition gives -atan(-20/40)*20 = 9.2729 deg; got {}",
            look_below.head_pitch_deg
        );
        let no_head_pitch = {
            let parts = mesh.skeleton.pose(&AnimInput::REST);
            let m = gui_ortho(CANVAS.0, CANVAS.1) * pose(&look_below) * parts[head];
            to_gui_px(m, nose)[1]
        };
        let d_measured = below - rest;
        let d_wrong = no_head_pitch - rest;
        assert!(
            d_measured > d_wrong + 0.5,
            "the skull itself must contribute to the tilt, not just the view: with \
             head pitch the nose moved {d_measured:.3} px, without it {d_wrong:.3} px. \
             A difference under half a pixel means `gui_entity_anim` is not reaching \
             the pose."
        );
    }

    /// The horizontal half: the head yaw is applied *relative to the body*, so
    /// the head's absolute swivel is **twice** the body's. This is the claim the
    /// "two absolute yaws 180° apart" misreading gets wrong, and it is checked
    /// against the pose, not the field.
    #[test]
    fn the_head_yaw_doubles_the_body_yaw() {
        let models = EntityModelSet::load();
        let mesh = models.get(player_model_name(false)).expect("player rig");
        let head = mesh.skeleton.index_of("head").expect("head part");
        let body = mesh.skeleton.index_of("body").expect("body part");
        let centre = rect_centre();
        let mouse = [centre[0] + 25.0, centre[1]];

        let look = gui_entity_look(RECT, mouse, false);
        // 180 + a and a, with a < 0 for a cursor right of centre.
        assert!(
            look.head_yaw_deg < 0.0,
            "a cursor right of centre gives a negative relative head yaw \
             (atan is applied to centre - mouse); got {}",
            look.head_yaw_deg
        );
        assert!(
            (look.body_yaw_deg - (180.0 + look.head_yaw_deg)).abs() < 1e-4,
            "bodyRot must be 180 + yRot: {} vs {}",
            look.body_yaw_deg,
            look.head_yaw_deg
        );

        // Now the *pose*: the nose swings further horizontally than the chest.
        let anim = gui_entity_anim(&look, AnimInput::REST);
        let posed = mesh.skeleton.pose(&anim);
        let rest_look = GuiEntityLook::FORWARD;
        let rest_posed = mesh.skeleton.pose(&gui_entity_anim(&rest_look, AnimInput::REST));

        let m = gui_ortho(CANVAS.0, CANVAS.1) * pose(&look);
        let m_rest = gui_ortho(CANVAS.0, CANVAS.1) * pose(&rest_look);
        let nose = Vec3::new(0.0, -0.25, -0.25);
        let chest = Vec3::new(0.0, 0.375, -0.125);

        let nose_dx = to_gui_px(m * posed[head], nose)[0] - to_gui_px(m_rest * rest_posed[head], nose)[0];
        let chest_dx =
            to_gui_px(m * posed[body], chest)[0] - to_gui_px(m_rest * rest_posed[body], chest)[0];
        assert!(
            nose_dx.abs() > chest_dx.abs() * 1.5,
            "the head must swivel further than the body — that is what makes the \
             eyes follow the cursor. nose moved {nose_dx:.3} px, chest {chest_dx:.3} px"
        );
    }

    /// `FALL_FLYING` zeroes the **head pitch** and leaves the view's own `Rx`
    /// alone. Reading the record's `else` branch as "no tilt at all" would drop
    /// the elytra pose's whole camera tilt.
    #[test]
    fn fall_flying_zeroes_only_the_head_pitch() {
        let mouse = [40.0, 60.0];
        let normal = gui_entity_look(RECT, mouse, false);
        let flying = gui_entity_look(RECT, mouse, true);
        assert_ne!(normal.head_pitch_deg, 0.0, "the fixture must produce a pitch");
        assert_eq!(flying.head_pitch_deg, 0.0);
        assert_eq!(flying.camera_pitch_deg, normal.camera_pitch_deg);
        assert_eq!(flying.body_yaw_deg, normal.body_yaw_deg);
        assert_eq!(flying.head_yaw_deg, normal.head_yaw_deg);
    }

    /// The `Rz(PI)` and `entity_model_matrix`'s `scale(-1,-1,1)` cancel exactly
    /// at a centred cursor, which is *why* vanilla rotates by π. Pinned because
    /// an "optimisation" that removes either one alone would flip the avatar
    /// upside down and mirror it.
    #[test]
    fn the_pi_roll_cancels_the_rig_flip() {
        let look = GuiEntityLook::FORWARD;
        let composed = pose(&look);
        let expected = Mat4::from_translation(Vec3::new(
            rect_centre()[0],
            rect_centre()[1],
            0.0,
        )) * Mat4::from_scale(Vec3::new(INVENTORY_SIZE, INVENTORY_SIZE, -INVENTORY_SIZE))
            * Mat4::from_translation(Vec3::new(
                0.0,
                BB * 0.5 + INVENTORY_OFFSET_Y - MODEL_FEET_OFFSET,
                0.0,
            ));
        let a = composed.to_cols_array();
        let b = expected.to_cols_array();
        for i in 0..16 {
            assert!(
                (a[i] - b[i]).abs() < 1e-4,
                "element {i}: composed {} vs the cancelled form {}. \
                 Rz(PI) . S(-1,-1,1) must be the identity.",
                a[i],
                b[i]
            );
        }
    }
}
