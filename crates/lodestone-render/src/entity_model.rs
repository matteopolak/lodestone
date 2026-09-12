use super::*;

/// A contiguous vertex/index range belonging to one animated model part.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartRange {
    /// First index belonging to this part.
    pub index_start: u32,
    /// Number of indices in this part.
    pub index_count: u32,
    /// First vertex belonging to this part.
    pub vertex_start: u32,
    /// Number of vertices in this part.
    pub vertex_count: u32,
}

/// A baked entity model split into animatable parts.
///
/// Vertices are in **part-local** space: the part's own rest pose is *not* folded
/// in, so a joint stays a joint. Multiplying a part's vertices by its matrix from
/// [`Skeleton::rest_pose`] reproduces the whole-model bake exactly — asserted
/// over the entire corpus by `lodestone-assets`' `part_bake_recomposes_to_the_
/// whole_model_bake`. That equivalence is what lets the renderer keep one static
/// vertex buffer per model *type* and move only matrices per frame.
#[derive(Debug, Clone)]
pub struct EntityMesh {
    /// Four vertices per quad, in the shared model-vertex format, part-local.
    pub vertices: Vec<ModelVertex>,
    /// Six indices per quad, wound so front faces point outward.
    pub indices: Vec<u32>,
    /// One index sub-range per part, in [`Skeleton`] part order.
    pub parts: Vec<PartRange>,
    /// The part hierarchy and its animator.
    pub skeleton: Skeleton,
    /// Local-space AABB minimum (model frame, blocks), at rest.
    pub local_min: Vec3,
    /// Local-space AABB maximum (model frame, blocks), at rest.
    pub local_max: Vec3,
    /// This model's own hand-translate override, if vanilla's subclass departs
    /// from the base humanoid model's. See [`HandPoseOverride`] and
    /// [`hand_pose_override_for`]; consumed by [`EntityInstance::new`] to fill
    /// [`EntityInstance::hand_transforms`].
    pub hand_override: HandPoseOverride,
}

impl EntityMesh {
    /// Bake a model definition into a renderable mesh.
    ///
    /// Each [`EntityQuad`](lodestone_assets::entity::EntityQuad) becomes four
    /// [`ModelVertex`] and six indices. The winding is chosen per quad from the
    /// baked outward normal so front faces are counter-clockwise (matching the
    /// pipeline's `Ccw`/back-cull), independent of the quad's corner order or
    /// mirror flag.
    #[must_use]
    pub fn from_model(def: &EntityModelDef) -> Self {
        Self::from_named_model("", def)
    }

    /// Bake a model definition into a renderable mesh, applying the arm rig
    /// [`humanoid_arms_for`] assigns to `model_name`.
    ///
    /// The name has to be known *here* rather than at pose time because a zombie
    /// rig moves the arms in its **resting** pose, and the mesh's local AABB is
    /// taken from that resting pose. Choosing the rig later would leave every
    /// zombie with a culling box drawn around a mob standing to attention while
    /// the drawn one has its arms out in front — the classic "correct until it
    /// clips at the screen edge" bug.
    #[must_use]
    pub fn from_named_model(model_name: &str, def: &EntityModelDef) -> Self {
        let baked = bake_entity_parts(def);
        let skeleton =
            Skeleton::from_parts(&baked).with_humanoid_arms(humanoid_arms_for(model_name));
        let rest = skeleton.rest_pose();

        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        let mut parts = Vec::with_capacity(baked.len());
        let mut local_min = Vec3::splat(f32::INFINITY);
        let mut local_max = Vec3::splat(f32::NEG_INFINITY);

        for (part_index, part) in baked.iter().enumerate() {
            let index_start = indices.len() as u32;
            let vertex_start = vertices.len() as u32;
            // The rest matrix is used only for the local AABB: the vertices
            // themselves stay part-local so the animator can rotate the joint.
            let rest_m = rest[part_index];
            for quad in &part.quads {
                for p in &quad.positions {
                    let posed = rest_m.transform_point3(Vec3::from(*p));
                    local_min = local_min.min(posed);
                    local_max = local_max.max(posed);
                }
            }
            push_part_quads(&part.quads, &mut vertices, &mut indices);
            parts.push(PartRange {
                index_start,
                index_count: indices.len() as u32 - index_start,
                vertex_start,
                vertex_count: vertices.len() as u32 - vertex_start,
            });
        }

        if indices.is_empty() {
            local_min = Vec3::ZERO;
            local_max = Vec3::ZERO;
        }

        // A creeper's drawn size is not its rest size: `pose_swelling` scales the
        // whole model by up to `MAX_SWELL_SCALE` (~41% horizontally) about the
        // model-space feet plane while its fuse burns. Everything above derives the
        // box from `rest_pose` alone, which is this function's own doc comment's
        // "correct until it clips at the screen edge" bug — a swelling creeper at
        // the frustum edge would be culled while still visibly on screen.
        //
        // Padded once here, at bake time, rather than recomputed per frame in
        // `EntityInstance::placed`: one constant box that always contains the drawn
        // model costs a slightly conservative cull and cannot drift from the pose,
        // where a per-frame exact box is a second derivation of the same geometry.
        //
        // The y term is conjugated about `MODEL_FEET_OFFSET` because that is what
        // `swell_root_affine` does — a plain scale about the model origin would let
        // the padded box sink below the feet plane rather than grow upward. `min`/
        // `max` over both corners, so the widening is correct whatever the signs.
        if model_name == "creeper" {
            let s = crate::entity_anim::MAX_SWELL_SCALE;
            let swollen = |v: Vec3| {
                Vec3::new(
                    v.x * s,
                    MODEL_FEET_OFFSET + (v.y - MODEL_FEET_OFFSET) * s,
                    v.z * s,
                )
            };
            let (a, b) = (swollen(local_min), swollen(local_max));
            local_min = local_min.min(a.min(b));
            local_max = local_max.max(a.max(b));
        }

        EntityMesh {
            vertices,
            indices,
            parts,
            skeleton,
            local_min,
            local_max,
            hand_override: hand_pose_override_for(model_name),
        }
    }

    /// Number of quads in the mesh.
    #[must_use]
    pub fn quad_count(&self) -> usize {
        self.indices.len() / 6
    }
}

/// Append one part's baked quads to a shared vertex/index buffer as
/// **part-local** geometry, winding each triangle pair from the quad's own baked
/// outward normal.
///
/// The one implementation of that winding rule, shared by [`EntityMesh`],
/// [`ArmourMesh`] and [`crate::block_entity::BlockEntityMesh`]. It has to be
/// shared rather than copied: an armour layer whose winding disagreed with the
/// mob it sits on would be invisible from exactly the half of the angles the mob
/// is visible from, and only once back-face culling is eventually turned on — a
/// defect that cannot be seen today and would land later, on somebody else's
/// change. A chest whose winding disagreed with the mobs beside it would have
/// the same property, which is why `block_entity` reaches in here rather than
/// keeping a "simple" local copy.
pub(crate) fn push_part_quads(
    quads: &[lodestone_assets::entity::EntityQuad],
    vertices: &mut Vec<ModelVertex>,
    indices: &mut Vec<u32>,
) {
    for quad in quads {
        let base = vertices.len() as u32;
        for i in 0..4 {
            vertices.push(ModelVertex {
                position: quad.positions[i],
                uv: quad.uvs[i],
                ao: 1.0,
                // The entity shader does **not** read this byte: entity light is
                // per *instance* (one lightmap sample per mob, as vanilla does),
                // so it arrives on the instance buffer, not here. The field is
                // filled anyway because the vertex layout is shared with
                // terrain, and a full-bright value keeps a mis-wired reader
                // honest rather than rendering every mob black.
                light: ENTITY_FULLBRIGHT,
                tint: 255,
                anim: 0,
                cutout_bypass: 0,
                // No biome tint on entities (see `models` module docs' D1
                // note: entities share `ModelVertex`'s layout but carry no
                // tint), so this override is always inert.
                tint_rgb_override: [0, 0, 0, 0],
            });
        }
        // Wind the two triangles so the geometric normal agrees with the baked
        // outward normal; otherwise back-face culling would drop the visible
        // side.
        let n = Vec3::from(quad.normal);
        let p0 = Vec3::from(quad.positions[0]);
        let p1 = Vec3::from(quad.positions[1]);
        let p2 = Vec3::from(quad.positions[2]);
        if (p1 - p0).cross(p2 - p0).dot(n) >= 0.0 {
            indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        } else {
            indices.extend_from_slice(&[base, base + 2, base + 1, base, base + 3, base + 2]);
        }
    }
}

/// The world placement transform for a standing mob, matching vanilla's
/// living-entity pose-stack order exactly (see the module docs).
///
/// `feet` is the entity's world position (its feet, as the protocol reports it),
/// `body_yaw_deg` its body yaw in degrees (Minecraft convention: `0` faces `+Z`),
/// and `scale` a uniform size multiplier (`1.0` for a normal adult; babies and
/// scaled mobs pass a smaller value). Applying this to a baked model vertex
/// yields its world position.
#[must_use]
pub fn entity_model_matrix(feet: Vec3, body_yaw_deg: f32, scale: f32) -> Mat4 {
    dying_entity_model_matrix(feet, body_yaw_deg, scale, 0.0)
}

/// [`entity_model_matrix`] with the **death fall-over** — vanilla's
/// living-entity pose-stack setup rotation's Z-axis term, in degrees, from
/// [`death_fall_over_degrees`](crate::entity_anim::death_fall_over_degrees).
///
/// # The roll's position in the product is the whole of this function
///
/// It sits between the body yaw and the Y-down flip, because that is where
/// vanilla's pose stack puts it:
///
/// ```text
///   setup rotations:  rotate about Y by (180 - bodyRot)          // `rotate`
///                     rotate about Z by (fall * 90)               // this term
///   render:           scale(-1, -1, 1)                            // `flip_scale`
///                     translate(0, -1.501, 0)                     // `lift`
/// ```
///
/// Two consequences that a "just multiply a Z rotation on" reading gets wrong:
///
/// * It is applied **before** the `lift`, so the mob rotates about the plane its
///   feet stand on and topples sideways. Composing the roll on the *outside*
///   (`T(feet) · Rz · Ry · …`) rotates about the same point but in the wrong frame,
///   so the fall direction stops tracking the body yaw; composing it after the lift
///   swings the mob about its own mid-height and leaves its feet in the air.
/// * `Rz` commutes with `flip_scale` (a `diag(-s, -s, s)` is `diag(-1,-1,1)` times a
///   uniform scale, and the sign flips cancel across the xy block), so it is *only*
///   the `lift` that fixes the position — which is exactly why the roll cannot be
///   folded into the caller's matrix afterwards and this is a separate function
///   rather than a multiply at the call site.
///
/// `fall_over_deg` of `0.0` is an exact identity (`Mat4::from_rotation_z(0)` is the
/// identity), so every living entity gets the bit-identical matrix
/// [`entity_model_matrix`] returned before this existed.
#[must_use]
pub fn dying_entity_model_matrix(
    feet: Vec3,
    body_yaw_deg: f32,
    scale: f32,
    fall_over_deg: f32,
) -> Mat4 {
    let translate_feet = Mat4::from_translation(feet);
    let rotate = Mat4::from_rotation_y((180.0 - body_yaw_deg).to_radians());
    let fall_over = Mat4::from_rotation_z(fall_over_deg.to_radians());
    // scale(-1,-1,1) folded with the uniform entity scale.
    let flip_scale = Mat4::from_scale(Vec3::new(-scale, -scale, scale));
    let lift = Mat4::from_translation(Vec3::new(0.0, -MODEL_FEET_OFFSET, 0.0));
    translate_feet * rotate * fall_over * flip_scale * lift
}

/// The vertical bob and extra spin a **non-living vehicle** rig needs in place
/// of [`MODEL_FEET_OFFSET`], keyed by model name — the second switch beside
/// [`projectile_pitch_offset_deg`] that decides which of three placements a
/// corpus model gets. `None` for every model that really is drawn through the
/// living-entity renderer (every mob, the player, and — despite the name —
/// `armor_stand`, which is a living entity in vanilla and keeps the
/// 1.501 lift).
///
/// Read from the 26.2 decompile, not inferred:
///
/// * `boat`/`chest_boat`/`raft`/`chest_raft` — the boat renderer's submit
///   step does `translate(0, 0.375, 0)`, `rotateY(180 - yRot)`,
///   `scale(-1, -1, 1)`, **then a fixed `rotateY(90)`** — a boat is drawn
///   through the plain entity renderer, not the living-entity one, so there
///   is no 1.501 lift at all, and the model (hull length along local `+X`,
///   matching the boat model's own pivots) needs that trailing spin to face
///   the right way once the yaw and flip are applied. Dropping it would
///   leave every boat floating at the right height but broadside to its
///   heading.
/// * `minecart` — the minecart renderer's submit step also does a `0.375`
///   bob before its own `scale(-1, -1, 1)` and no lift. Vanilla composes the
///   cart's yaw as a bare `rotateY(yRot)` (no `180 -`, no rail curve
///   tracking, both because this engine has no per-tick rail-curve state to
///   feed it) rather than the mob convention this crate already applies
///   elsewhere; reusing the existing `180 - yaw` term here rather than
///   porting that difference keeps the change scoped to the lift bug this
///   function exists to fix, so the extra spin is `0.0` (an exact identity)
///   rather than a second unverified rotation formula.
///
/// `end_crystal` is deliberately **not** in this table: its renderer has no
/// `scale(-1, -1, 1)` flip at all, so it is not a small variation on this
/// placement the way the vehicles are — fixing it needs its own
/// investigation into whether the corpus geometry was even authored for the
/// flipped frame, not a table entry here.
#[must_use]
pub fn non_living_vehicle_placement(model_name: &str) -> Option<(f32, f32)> {
    match model_name {
        // `"boat_water_patch"` joins this arm rather than getting its own:
        // the boat renderer's submit step submits the water-patch geometry
        // **inside the same pushed pose-stack block**
        // as the main model, after the identical bob/rotate/flip/spin
        // sequence — so the patch's placement transform is not merely
        // *similar* to the boat's, it is the same pose-stack state the boat
        // model itself just submitted through. Omitting it here would leave
        // the mask floating at the wrong height and facing broadside, right
        // back to the "water shows through the bottom" symptom this exists
        // to fix, just from a mask sitting nowhere near the hull instead of
        // no mask at all.
        "boat" | "chest_boat" | "raft" | "chest_raft" | "boat_water_patch" => Some((0.375, 90.0)),
        "minecart" => Some((0.375, 0.0)),
        // A leash knot's renderer flips the model and submits it, and does
        // nothing else — no bob, no yaw, and (because it is not a living-entity
        // renderer) no 1.501 feet lift. So it belongs in this table rather than
        // on the mob placement, which would bury it 1.501 blocks under the fence
        // post it is tied to; the bob is a genuine `0.0` rather than a stand-in.
        //
        // The `180 - yaw` this placement applies is a harmless surplus here: the
        // entity's yaw is always zero and the knot is a 6×6 box centred on its
        // own pivot, so the half-turn only mirrors the sheet across X.
        "leash_knot" => Some((0.0, 0.0)),
        // A wither skull's renderer applies the flip and *nothing else*: the
        // skull's facing comes from its model's own head rotation, set from the
        // entity's yaw. Composed under the flip that is `Ry(-yaw)`, and this
        // placement's `Ry(180 - yaw)` reaches it exactly at an extra spin of
        // 180° — an identity, not a fitted number. The head's matching *pitch*
        // is dropped: this placement has no pitch term, so a skull climbing or
        // diving stays level.
        "wither_skull" => Some((0.0, 180.0)),
        // A shulker bullet is lifted `0.15` and then tumbled on all three axes
        // at three rates off its own age, over which a second translucent copy
        // is drawn at 1.5×. Neither the tumble nor the halo is available to a
        // rig with one mesh, one sheet and no clock, so the bullet gets a fixed
        // orientation — tolerable only because its three slabs make it symmetric
        // under any quarter turn. The bob is real and is kept.
        "shulker_bullet" => Some((0.15, 0.0)),
        // The wind charge's renderer applies neither a flip nor a rotation —
        // the dispatcher's bare translate is all it gets, so vanilla's box union
        // never turns to face travel direction, only its (unported) internal
        // counter-spin moves. There is no "translate only" placement in this
        // table to route it through instead, so it takes the ordinary flip and a
        // zero extra yaw, same as `wither_skull`/`shulker_bullet` above. See
        // `wind_charge_model`'s doc for why that is tolerable here: both boxes
        // are close to rotationally symmetric, so the wrong flip and the
        // yaw-following are not an obvious mirroring defect.
        "wind_charge" => Some((0.0, 0.0)),
        _ => None,
    }
}

/// The world placement transform for a **non-living vehicle** — a model
/// [`non_living_vehicle_placement`] recognises — matching the vanilla pose-stack
/// order that function documents: bob, yaw, flip, then the model's own extra
/// spin. `vertical_offset` is the bob (in world-Y, applied before the yaw
/// rotate — the two commute since the bob is Y-only and the rotation is about
/// Y) and `extra_yaw_deg` is the trailing spin, `0.0` for models with none.
#[must_use]
pub fn non_living_vehicle_matrix(
    feet: Vec3,
    yaw_deg: f32,
    scale: f32,
    vertical_offset: f32,
    extra_yaw_deg: f32,
) -> Mat4 {
    let translate_feet = Mat4::from_translation(feet);
    let bob = Mat4::from_translation(Vec3::new(0.0, vertical_offset, 0.0));
    let rotate = Mat4::from_rotation_y((180.0 - yaw_deg).to_radians());
    let flip_scale = Mat4::from_scale(Vec3::new(-scale, -scale, scale));
    let spin = Mat4::from_rotation_y(extra_yaw_deg.to_radians());
    translate_feet * bob * rotate * flip_scale * spin
}

/// The extra pitch, in degrees, a projectile rig needs on top of the entity's
/// own pitch — or `None` for a model that is **not** placed by
/// [`projectile_model_matrix`].
///
/// This is the one switch that decides which of the two placements a corpus
/// model gets, so it is also the thing that would put every arrow 1.5 blocks
/// **above** where it belongs and mirrored if it returned `None` by mistake — see
/// [`projectile_model_matrix`] for why the offset points *up* and not down,
/// which is a direction that was initially recorded backwards. It is keyed on
/// the *model name*, not the entity type path, because that is what
/// [`EntityModelSet`] already keys everything else by, and because vanilla's
/// own distinction is which renderer *type* draws the entity:
///
/// * `arrow`, `spectral_arrow` — a shared arrow renderer (used by both the
///   tippable and spectral arrow variants). Pitch about the Z axis with
///   **no** offset: the arrow model's shaft already lies along `+X`.
/// * `trident` — the thrown-trident renderer applies a Z-axis rotation of
///   `pitch + 90`. The trident model's pole lies along `Y` with the spikes at
///   negative `Y`; the `+90°` is exactly what rotates that axis onto the
///   arrow's `+X`, so one matrix serves both rigs and the whole difference
///   between them is this number.
///
/// Every other model — every mob, the player, and the block-entity rigs — is
/// drawn through the living-entity renderer (or a block entity) and returns
/// `None`.
#[must_use]
pub fn projectile_pitch_offset_deg(model_name: &str) -> Option<f32> {
    match model_name {
        "arrow" | "spectral_arrow" => Some(0.0),
        "trident" => Some(90.0),
        // A llama spit's renderer is this placement term for term —
        // `Ry(yaw - 90°)` then `Rz(pitch)`, no flip and no feet lift — so it
        // belongs here rather than on the mob path, with no offset for the same
        // reason as the arrow: its cluster is authored around the shot axis.
        //
        // One deviation, and it is not expressible here: vanilla lifts the spit
        // `0.15` blocks in **world** space *before* the two rotations. A mesh
        // offset would rotate with the spit instead of staying vertical, and
        // this matrix has no pre-rotation translation, so the spit draws 0.15
        // blocks low. Fixing it means a bob term on this placement, which every
        // other user would pass zero for.
        "llama_spit" => Some(0.0),
        _ => None,
    }
}

/// The world placement transform for a **projectile**, matching vanilla's
/// arrow-renderer submit step's pose-stack order.
///
/// ```text
///   translate(pos)                       // move to the entity's position
///   rotateY(yaw - 90°)                   // face travel direction
///   rotateZ(pitch + pitch_offset)        // pitch about Z, not X
/// ```
///
/// # Why this is not [`entity_model_matrix`] with a pitch bolted on
///
/// A projectile is drawn through the plain entity renderer, **not** the
/// living-entity one. The plain entity renderer applies no scale at all; the
/// `scale(-1, -1, 1)` and the `translate(0, -1.501, 0)` that
/// [`entity_model_matrix`] carries both belong to the living-entity renderer
/// alone. So a projectile gets **neither**, and there is consequently no flip
/// here: the projectile meshes in
/// [`entity_models`](lodestone_assets::entity_models) are authored `+Y` **up**
/// rather than in the mob rigs' `Y`-down frame.
///
/// Reusing the mob matrix would draw every arrow [`MODEL_FEET_OFFSET`] = 1.501
/// blocks **above** its reported position, and pointing along a reflected axis.
/// Note the direction: the lift is applied *before* the `scale(-1, -1, 1)`, so
/// `-1.501` comes back out as `+1.501` — an earlier note here said "below",
/// and so did the first draft of the test that now pins it
/// (`reusing_the_mob_matrix_would_lift_an_arrow_and_reverse_it`). Either way it
/// reads as a texture bug rather than a placement bug, which is why it is worth
/// the separate function.
///
/// # Rotations, and why the axis matters
///
/// `pos` is the entity's world position, `yaw_deg` its own yaw and `pitch_deg` its
/// own pitch — both as the server reports them, both derived by vanilla from
/// its own `atan2` on the projectile's own velocity when it is shot, which is *not*
/// the yaw convention a mob's body uses: vanilla's shoot step sets
/// `yaw = atan2(mx, mz)`, so a projectile fired by a player looking at yaw
/// `Y` carries `yaw = -Y`. `Ry(yaw - 90°)` maps model
/// `+X` to `(sin yRot, 0, cos yRot)`, which is exactly that motion direction —
/// the two conventions agree only because both halves are taken from vanilla
/// together.
///
/// Both signs are the **opposite** of a player's, so they were measured against
/// Mojang's own 26.2 server over RCON rather than only read: `+X` motion gives
/// `yaw = +90` (a player facing `-X` has yaw `+90`), and *rising* motion gives a
/// **positive** pitch (a player looking up has a *negative* pitch). Nine
/// direction cases, nine exact matches — see `docs/projectile-renderers.md`,
/// which also records why the first run of that probe read zero for all nine.
///
/// Pitch is a rotation about **`Z`**, not `X`, because the shaft runs along `+X`.
/// A pitch applied about `X` would spin the arrow about its own axis and leave
/// the silhouette almost unchanged while every arrow flew level — the "looks
/// plausible, is wrong" shape this file's module docs warn about for the mob
/// flip.
#[must_use]
pub fn projectile_model_matrix(pos: Vec3, yaw_deg: f32, pitch_deg: f32, scale: f32) -> Mat4 {
    Mat4::from_translation(pos)
        * Mat4::from_rotation_y((yaw_deg - 90.0).to_radians())
        * Mat4::from_rotation_z(pitch_deg.to_radians())
        * Mat4::from_scale(Vec3::splat(scale))
}

/// A single entity to render: which model type draws it, its world transform,
/// and its world-space AABB for frustum culling.
