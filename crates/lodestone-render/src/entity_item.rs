use super::*;

/// Minimum vertical clearance used when placing a dropped item above its origin.
pub const ITEM_MIN_HOVER_HEIGHT: f32 = 0.0625;

/// Vanilla's dropped-item renderer constant: a posed model thinner than this
/// in `z` is treated as a flat sprite and a stack of them is
/// fanned along `z` rather than jittered in three axes.
pub const FLAT_ITEM_DEPTH_THRESHOLD: f32 = 0.0625;

/// Bob amplitude in blocks (`… * 0.1F + 0.1F`), so the bob spans `0.0..=0.2`.
pub const ITEM_BOB_AMPLITUDE: f32 = 0.1;

/// Ticks per radian of bob phase (`sin(ageInTicks / 10.0F + bobOffs)`).
pub const ITEM_BOB_TICKS_PER_RADIAN: f32 = 10.0;

/// Ticks per radian of spin (`getSpin = ageInTicks / 20.0F + bobOffs`).
pub const ITEM_SPIN_TICKS_PER_RADIAN: f32 = 20.0;

/// `display.ground` of `minecraft:block/block`, which **every** block item model
/// inherits (verified against 26.2's `client.jar`).
///
/// # This is now a *fallback*, not the only source
///
/// It used to be the only one: `icon.rs` did `resolved.display.get("gui")` and
/// dropped every other slot, so [`ItemGeometry`](crate::ItemGeometry) carried
/// the isometric inventory pose and nothing else. The asset layer now carries
/// all nine slots ([`ItemGeometry::display`](crate::ItemGeometry::display)), and
/// [`ground_transform`] reads the real declared `ground` in preference to this.
///
/// The constants stay because the *fallback still has to be right*: an item
/// whose model chain declares no `ground` at all would otherwise be posed with
/// the identity, i.e. a full-size 1×1×1 block lying in the grass. Being wrong by
/// a factor of four in scale is the visible signature.
///
/// Verified against 26.2's `client.jar`: `models/block/block.json` declares
/// `ground` as `translation [0, 3, 0]`, `scale 0.25`.
pub const BLOCK_ITEM_GROUND: DisplayTransform = DisplayTransform {
    rotation: [0.0, 0.0, 0.0],
    translation: [0.0, 3.0, 0.0],
    scale: [0.25, 0.25, 0.25],
};

/// `display.ground` of `minecraft:item/generated`, the parent of every flat
/// sprite item. See [`BLOCK_ITEM_GROUND`] for why this is a constant.
pub const GENERATED_ITEM_GROUND: DisplayTransform = DisplayTransform {
    rotation: [0.0, 0.0, 0.0],
    translation: [0.0, 2.0, 0.0],
    scale: [0.5, 0.5, 0.5],
};

/// The `display.ground` transform to pose an item under, chosen by its GUI
/// lighting mode: `side` is the block-model family (`block/block`), `front` the
/// flat-sprite family (`item/generated`). Vanilla makes the same split — the two
/// `gui_light` values partition the item models almost exactly along the same
/// line — and it is the only signal reachable from a baked
/// [`ItemGeometry`](crate::ItemGeometry) today.
#[must_use]
pub fn ground_transform_for(gui_light: GuiLight) -> DisplayTransform {
    match gui_light {
        GuiLight::Side => BLOCK_ITEM_GROUND,
        GuiLight::Front => GENERATED_ITEM_GROUND,
    }
}

/// The `display.ground` transform to pose a **dropped** item under: the one the
/// item's own model chain declares, falling back to
/// [`ground_transform_for`]`(gui_light)` when it declares none.
///
/// This is the accessor a drop should use.
/// [`DisplayTransforms::declared`] rather than `get` is the whole point: `get`
/// answers an undeclared slot with the identity, which for `ground` means a
/// full-size block lying in the grass rather than vanilla's quarter-scale one.
/// Distinguishing "the pack said identity" from "we found nothing" is what makes
/// the [`GuiLight`]-keyed guess a fallback instead of dead code.
///
/// # How to change it
///
/// The other slots want exactly this shape — a `hand_transform(&DisplayTransforms,
/// Arm, /* first person */ bool)` for held items, reading
/// `thirdperson_righthand`/`firstperson_righthand` with
/// [`DisplaySlot::left_hand_fallback`](lodestone_assets::DisplaySlot::left_hand_fallback)
/// already handled inside `DisplayTransforms::get`. There is **no** sensible
/// `GuiLight`-keyed fallback for those (`block/block` and `item/generated`
/// disagree on far more than scale), so an undeclared hand slot should draw the
/// identity and be counted, not guessed at.
#[must_use]
pub fn ground_transform(display: &DisplayTransforms, gui_light: GuiLight) -> DisplayTransform {
    display
        .declared(DisplaySlot::Ground)
        .unwrap_or_else(|| ground_transform_for(gui_light))
}

/// A stable per-entity bob/spin phase in `[0, 2π)`, standing in for vanilla's
/// `bobOffs = random.nextFloat() * PI * 2`.
///
/// Vanilla seeds it from the client's RNG at spawn; we cannot observe that, and
/// re-rolling it every frame would make an item jitter instead of spin. Hashing
/// the server-assigned entity id gives the same *property* that matters — two
/// items dropped together do not bob in lockstep — while staying a pure function
/// of data both the renderer and a test can see.
#[must_use]
pub fn item_bob_offset(entity_id: EntityNetworkId) -> f32 {
    // A single multiplicative-hash round over the id, taken as a fraction.
    let mixed = (entity_id.raw() as u32).wrapping_mul(0x9E37_79B9);
    let frac = f32::from(u16::try_from(mixed >> 16).unwrap_or(0)) / 65536.0;
    frac * std::f32::consts::TAU
}

/// Vanilla's vertical bob at `age_ticks`: `sin(age / 10 + offs) * 0.1 + 0.1`,
/// so the result is in `0.0..=0.2` blocks and never negative.
#[must_use]
pub fn item_bob_height(age_ticks: f32, bob_offset: f32) -> f32 {
    (age_ticks / ITEM_BOB_TICKS_PER_RADIAN + bob_offset).sin() * ITEM_BOB_AMPLITUDE
        + ITEM_BOB_AMPLITUDE
}

/// Vanilla's dropped-item spin function: the item's yaw in **radians** at `age_ticks`.
#[must_use]
pub fn item_spin_radians(age_ticks: f32, bob_offset: f32) -> f32 {
    age_ticks / ITEM_SPIN_TICKS_PER_RADIAN + bob_offset
}

/// The model-space `y` extent of `quads` once posed by `ground`, as
/// `(min_y, max_y)`. `(0, 0)` for an empty quad list.
///
/// This is vanilla's posed-model bounding box for the `y` axis: it is
/// measured on the **posed** model, which is why it cannot be a constant — a
/// scaled-down cube and a full-size one hover differently.
#[must_use]
pub fn posed_item_y_extent(quads: &[BakedQuad], ground: &DisplayTransform) -> (f32, f32) {
    let pose = display_matrix(ground);
    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    for quad in quads {
        for p in &quad.positions {
            let y = pose.transform_point3(Vec3::from(*p)).y;
            min = min.min(y);
            max = max.max(y);
        }
    }
    if min > max { (0.0, 0.0) } else { (min, max) }
}

/// The posed model's `z` extent, the mirror of [`posed_item_y_extent`].
///
/// This is the input to vanilla's flat-versus-solid branch in its
/// multi-copy item-cluster submit step: a model whose depth exceeds
/// [`FLAT_ITEM_DEPTH_THRESHOLD`] is a block-ish thing whose extra stack copies
/// jitter in all three axes, while a flat sprite instead fans its copies evenly
/// along `z`. Measured on the *posed* model for the same reason the `y` version
/// is — the branch is about the drawn depth, not the model's declared one.
#[must_use]
pub fn posed_item_z_extent(quads: &[BakedQuad], ground: &DisplayTransform) -> (f32, f32) {
    let pose = display_matrix(ground);
    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    for quad in quads {
        for p in &quad.positions {
            let z = pose.transform_point3(Vec3::from(*p)).z;
            min = min.min(z);
            max = max.max(z);
        }
    }
    if min > max { (0.0, 0.0) } else { (min, max) }
}

/// How many copies of a stack vanilla draws — its item-cluster render-state
/// count: 1, then 2 above 1, 3 above 16, 4 above 32, 5 above 48.
#[must_use]
pub fn rendered_amount(count: u32) -> u32 {
    match count {
        0..=1 => 1,
        2..=16 => 2,
        17..=32 => 3,
        33..=48 => 4,
        _ => 5,
    }
}

/// Per-copy scatter for a stack's extra copies, in the idiom
/// [`item_bob_offset`] set.
///
/// Vanilla seeds this from a random source keyed on the item's registry id
/// plus its damage value, which we cannot observe. So this hashes `(entity_id, copy)` for
/// the same *property* — no two drops and no two copies scatter in lockstep —
/// rather than chasing bytes we have no way to reproduce. `copy == 0` is exactly
/// zero, matching vanilla's unperturbed first submit call.
///
/// `extent` is the half-range on each axis: `0.15` for a solid model (all three
/// axes), `0.075` for a flat sprite (x and y only, hence the zero `z` the caller
/// discards).
#[must_use]
pub fn item_cluster_jitter(entity_id: EntityNetworkId, copy: u32, extent: f32) -> Vec3 {
    item_cluster_jitter_from_seed(entity_id.raw() as u32, copy, extent)
}

/// Per-copy scatter for a stack when the caller has an item-render seed rather
/// than an entity identity. Item previews and block-entity displays use the
/// item registry seed because they do not have a network entity.
#[must_use]
pub fn item_cluster_jitter_from_seed(seed: u32, copy: u32, extent: f32) -> Vec3 {
    if copy == 0 {
        return Vec3::ZERO;
    }
    // Three decorrelated hash rounds over the same (id, copy) key — one per
    // axis, so a copy does not move along the diagonal.
    let key = seed
        .wrapping_mul(0x9E37_79B9)
        .wrapping_add(copy.wrapping_mul(0x85EB_CA6B));
    let axis = |salt: u32| {
        let mixed = key.wrapping_add(salt).wrapping_mul(0xC2B2_AE35);
        let frac = f32::from(u16::try_from(mixed >> 16).unwrap_or(0)) / 65536.0;
        (frac * 2.0 - 1.0) * extent
    };
    Vec3::new(axis(0x1656_67B1), axis(0x27D4_EB2F), axis(0x1656_67B5))
}

/// Vanilla's lowest-point offset: the lift that puts the posed model's
/// lowest point exactly [`ITEM_MIN_HOVER_HEIGHT`] above the entity's own
/// position.
#[must_use]
pub fn item_hover_lift(quads: &[BakedQuad], ground: &DisplayTransform) -> f32 {
    -posed_item_y_extent(quads, ground).0 + ITEM_MIN_HOVER_HEIGHT
}

/// The world placement matrix for a dropped item, matching vanilla's
/// dropped-item submit step's pose-stack order exactly:
///
/// ```text
/// T(position) · T(0, bob + hover_lift, 0) · Ry(spin) · display_matrix(ground)
/// ```
///
/// `position` is the item entity's reported world position, `age_ticks` its
/// continuous age (vanilla's own fractional age, between server ticks), `bob_offset`
/// its per-entity phase ([`item_bob_offset`]) and `hover_lift`
/// [`item_hover_lift`] for the same quads and transform.
///
/// The determinant is **positive** (a translation, a rotation and a positive
/// uniform scale), so this composes with `Camera::view_projection` to the same
/// winding as terrain. See the section note above for why "negative" is the
/// tempting wrong answer.
#[must_use]
pub fn dropped_item_matrix(
    position: Vec3,
    age_ticks: f32,
    bob_offset: f32,
    ground: &DisplayTransform,
    hover_lift: f32,
) -> Mat4 {
    let bob = item_bob_height(age_ticks, bob_offset);
    let spin = item_spin_radians(age_ticks, bob_offset);
    Mat4::from_translation(position)
        * Mat4::from_translation(Vec3::new(0.0, bob + hover_lift, 0.0))
        * Mat4::from_rotation_y(spin)
        * display_matrix(ground)
}

/// Mesh one dropped item's baked geometry into a world-space [`ModelMesh`],
/// ready for [`GpuModelMesh::upload`](crate::GpuModelMesh) and a draw through
/// the ordinary [`ModelPipeline`](crate::ModelPipeline) with a *world* camera
/// uniform (`section_origin` zero).
///
/// The geometry and the shading come from [`mesh_item_quads`], which the hotbar
/// already uses, so a dropped stone and a stone in slot 0 are textured and shaded
/// from the identical quads. The one thing overridden afterwards is the packed
/// light byte: `mesh_item_quads` nails every vertex to
/// [`GUI_ITEM_LIGHT`](crate::GUI_ITEM_LIGHT) because an inventory slot is
/// full-bright by definition, and a dropped item in a dark cave is emphatically
/// not. Pass the world sample (see [`EntityLightSource`](crate::EntityLightSource)
/// on the shell side); pass `GUI_ITEM_LIGHT` when there is no world to sample.
#[must_use]
pub fn dropped_item_mesh(
    quads: &[BakedQuad],
    gui_light: GuiLight,
    ground: &DisplayTransform,
    position: Vec3,
    age_ticks: f32,
    bob_offset: f32,
    light: u8,
) -> ModelMesh {
    let lift = item_hover_lift(quads, ground);
    let pose = dropped_item_matrix(position, age_ticks, bob_offset, ground, lift);
    mesh_item_quads_with_light(quads, pose, gui_light, light)
}

/// Vanilla's vault display-item rotation speed: degrees per client tick the
/// display item spins, unbounded (vanilla wraps the stored angle before
/// storing it, but the rotation applied from it is periodic mod 360, so the
/// unwrapped running total below is the same rotation and needs no wrap).
pub const VAULT_SPIN_DEGREES_PER_TICK: f32 = 10.0;

/// Vanilla's per-tick vault display-item spin update, evaluated at a
/// continuous tick: a shortest-path rotation lerp between the previous and
/// current spin, where `currentSpin = previousSpin + 10°` every tick, which
/// for a constant per-tick step is exactly the unwrapped linear form below
/// (the shortest-path wrap only matters when the two ends are more than 180°
/// apart, and adjacent ticks here are always exactly 10° apart).
///
/// # A deliberate simplification: tied to absolute world time, not per-vault age
///
/// Real vanilla starts each vault's own counter at `0` when its block entity
/// is constructed — effectively when its chunk first loads — so two vaults
/// loaded at different moments spin out of phase with each other. This client
/// has no record of *when* a given vault's block entity was constructed, only
/// the world's current game time, so every vault here shares one clock instead
/// (the same limitation `crate::beacon`'s rotating core and
/// `block_entity::banner_phase` already accept, per those functions' docs).
/// Decorative only — nothing about a vault's function reads this phase.
#[must_use]
pub fn vault_spin_degrees(game_time: i64, partial_tick: f32) -> f32 {
    (game_time as f32 + partial_tick) * VAULT_SPIN_DEGREES_PER_TICK
}

/// The world placement matrix for one copy of a vault's floating display-item
/// cluster, matching vanilla's vault-renderer submit step's pose stack —
///
/// ```text
/// T(block_pos) · T(0.5, 0.4, 0.5) · Ry(spin) · T(offset) · display_matrix(ground)
/// ```
///
/// — composed with vanilla's own per-copy translate for a multi-copy item
/// cluster (`offset`, zero for the first copy) and the item's own
/// `display.ground` transform on the right, the same composition
/// [`dropped_item_matrix`] uses for the identical reason: vanilla applies the
/// display transform *inside* its item render-state submit step, after every
/// pose this function's caller pushes.
#[must_use]
pub fn vault_display_item_matrix(
    block_pos: Vec3,
    spin_deg: f32,
    offset: Vec3,
    ground: &DisplayTransform,
) -> Mat4 {
    Mat4::from_translation(block_pos)
        * Mat4::from_translation(Vec3::new(0.5, 0.4, 0.5))
        * Mat4::from_rotation_y(spin_deg.to_radians())
        * Mat4::from_translation(offset)
        * display_matrix(ground)
}

/// Mesh one copy of a vault's display-item cluster into a world-space
/// [`ModelMesh`], for the same model-pipeline draw [`dropped_item_mesh`] feeds
/// — see that function's doc for why a vault's floating reward is an *item
/// model* on the model pipeline rather than a cuboid rig on
/// [`EntityPipeline`](crate::EntityPipeline).
#[must_use]
pub fn vault_display_item_mesh(
    quads: &[BakedQuad],
    gui_light: GuiLight,
    ground: &DisplayTransform,
    block_pos: Vec3,
    spin_deg: f32,
    offset: Vec3,
    light: u8,
) -> ModelMesh {
    let pose = vault_display_item_matrix(block_pos, spin_deg, offset, ground);
    mesh_item_quads_with_light(quads, pose, gui_light, light)
}

/// Mesh one campfire's cooking item into a world-space [`ModelMesh`], for the
/// same model-pipeline draw [`dropped_item_mesh`] feeds.
///
/// The placement is [`campfire_item_matrix`](crate::block_entity::campfire_item_matrix)
/// — a pure port of vanilla's campfire-renderer submit step's pose stack —
/// composed with the item's own `display.fixed` on the right, because that is
/// where vanilla applies the item transform: its item render-state submit
/// step applies it *after* the renderer's own pushes. Composing it on the
/// left instead would rotate the campfire's corner offset by the item's
/// display rotation, which for a food sprite (`fixed` is a `180°` Y turn on
/// most of them) mirrors all four items into the wrong corners while still
/// looking like four items on a campfire.
///
/// `fixed` is `display.get(DisplaySlot::Fixed)` — vanilla resolves a
/// campfire's stack in the item-frame display context, **not** the ground
/// one. That is the one thing this does not share with the drop path.
#[must_use]
pub fn campfire_item_mesh(
    quads: &[BakedQuad],
    gui_light: GuiLight,
    fixed: &DisplayTransform,
    pos: [i32; 3],
    facing_yaw_deg: f32,
    slot: CampfireSlot,
    light: u8,
) -> ModelMesh {
    let pose = crate::block_entity::campfire_item_matrix(pos, facing_yaw_deg, slot)
        * display_matrix(fixed);
    mesh_item_quads_with_light(quads, pose, gui_light, light)
}

/// Mesh one item on a shelf's slot into a world-space [`ModelMesh`], for the
/// same model-pipeline draw [`dropped_item_mesh`] feeds.
///
/// The placement is
/// [`shelf_slot_matrix`](crate::block_entity::shelf_slot_matrix) —
/// ported from vanilla's shelf-renderer item-submit pose stack up to its
/// final translate — composed with **two** further transforms this function
/// alone can supply, both requiring the item's own baked quads:
///
/// 1. The bounding-box correction (a vertical offset in the real jar):
///    `-box.minY`, plus `-(box.maxY - box.minY) / 2` when the shelf is
///    *not* aligned to the bottom. `box` is the item's posed-model bounding
///    box — the item's extents *after* its own on-shelf display-context
///    transform, which is exactly what [`posed_item_y_extent`] measures.
///    Applied as a translate **inside** the `0.25×` scale
///    [`shelf_slot_matrix`] already applied (vanilla translates after it
///    scales), which is why this is a right-hand factor rather than folded
///    into that function's own world-space translate.
/// 2. The item's own `display.on_shelf` transform, composed on the right for
///    the same reason [`campfire_item_mesh`] composes there: vanilla applies
///    it *inside* its item render-state submit step, after every pose the
///    renderer itself pushes.
#[must_use]
pub fn shelf_item_mesh(
    quads: &[BakedQuad],
    gui_light: GuiLight,
    on_shelf: &DisplayTransform,
    pos: [i32; 3],
    facing_yaw_deg: f32,
    slot: ShelfSlot,
    align_to_bottom: bool,
    light: u8,
) -> ModelMesh {
    let (min_y, max_y) = posed_item_y_extent(quads, on_shelf);
    let mut offset_y = -min_y;
    if !align_to_bottom {
        offset_y += -(max_y - min_y) / 2.0;
    }
    let pose = crate::block_entity::shelf_slot_matrix(pos, facing_yaw_deg, slot, align_to_bottom)
        * Mat4::from_translation(Vec3::new(0.0, offset_y, 0.0))
        * display_matrix(on_shelf);
    mesh_item_quads_with_light(quads, pose, gui_light, light)
}

/// Mesh a suspicious sand/gravel block's revealed item into a world-space
/// [`ModelMesh`], for the same model-pipeline draw [`dropped_item_mesh`] feeds.
///
/// The placement is
/// [`brushable_item_matrix`](crate::block_entity::brushable_item_matrix) —
/// ported from vanilla's brushable-block renderer submit step's pose stack —
/// composed with the item's own `display.fixed` on the right, for the
/// identical reason [`campfire_item_mesh`] composes there: vanilla's
/// brushable-block render-state extraction resolves the item in the
/// item-frame display context, not the ground one.
#[must_use]
pub fn brushable_item_mesh(
    quads: &[BakedQuad],
    gui_light: GuiLight,
    fixed: &DisplayTransform,
    pos: [i32; 3],
    hit_direction: lodestone_assets::Direction,
    dust_progress: u8,
    light: u8,
) -> ModelMesh {
    let pose = crate::block_entity::brushable_item_matrix(pos, hit_direction, dust_progress)
        * display_matrix(fixed);
    mesh_item_quads_with_light(quads, pose, gui_light, light)
}

/// [`mesh_item_quads`] followed by the world-light override both
/// [`dropped_item_mesh`] and [`held_item_mesh`] need: the baked geometry nails
/// every vertex to [`GUI_ITEM_LIGHT`](crate::GUI_ITEM_LIGHT) (an inventory slot
/// is full-bright by definition), and a world-placed item is not, so the caller's
/// own world sample overwrites it here, in one place, after meshing.
pub(crate) fn mesh_item_quads_with_light(
    quads: &[BakedQuad],
    pose: Mat4,
    gui_light: GuiLight,
    light: u8,
) -> ModelMesh {
    let mut mesh = mesh_item_quads(quads, pose, gui_light);
    for vertex in &mut mesh.vertices {
        vertex.light = light;
    }
    mesh
}

// ---------------------------------------------------------------------------
// Thrown item projectiles
// ---------------------------------------------------------------------------
//
// A snowball is not a cuboid rig and not a dropped item either: it is the item's
// *own* model, posed by `display.ground`, turned to face the camera, and drawn at
// the entity's position with no bob, no spin and no hover lift. Transcribed from
// the 26.2 client's thrown-item renderer, whose whole submit step is
//
// ```text
// scale(scale, scale, scale)
// mulPose(camera.orientation)
// submit(...)                              // resolved in the ground display context
// ```
//
// with the entity's position already on the pose stack by the dispatcher. The
// ground context is why [`ground_transform`] is shared with the drop path
// rather than duplicated: vanilla's render-state extraction for a thrown item
// resolves the item's display in that same ground context.

/// One entity type's thrown-item-renderer registration: which item's model to
/// draw, at what scale, and whether the renderer forces full-bright block light.
///
/// The `scale` and `full_bright` columns are **not** uniform, and reading them as
/// uniform is the visible bug: a `fireball` is `3.0` and a `small_fireball`
/// `0.75`, so the two would otherwise be the same size on screen even though the
/// large one is four times the small one in vanilla.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThrownItem {
    /// The item id whose baked geometry to draw, e.g. `"minecraft:snowball"`.
    ///
    /// This is vanilla's default-item fallback. It is only the *fallback*: the
    /// entity's real stack rides entity metadata (the same item-stack
    /// metadata field a dropped item uses), and a caller that has it
    /// should prefer it — a dispenser-fired arrow-of-harming analogue for
    /// potions is exactly the case where the two differ.
    pub item: &'static str,
    /// Vanilla's thrown-item-renderer scale, applied *before* the billboard
    /// rotation.
    pub scale: f32,
    /// Vanilla's full-bright flag, which overrides the block light level to
    /// `15`. A fireball glows; a snowball does not.
    pub full_bright: bool,
}

/// The [`ThrownItem`] registration for an entity type path (`"snowball"`), or
/// `None` for every entity that is not drawn as a billboarded thrown item in vanilla.
///
/// This is the **complete** 26.2 registration list, read out of
/// vanilla's entity-renderer registration table rather than guessed from the
/// name. Two entries commonly assumed to be here are not, and adding them would
/// draw the wrong thing:
///
/// * **`wind_charge` and `breeze_wind_charge` use a dedicated wind-charge renderer**, a real
///   cuboid model — not an item billboard, and there is no `wind_charge` *item*
///   sprite to draw either. Both are now in the
///   [`entity_models`](lodestone_assets::entity_models) corpus (`wind_charge`
///   model, `breeze_wind_charge` aliased onto it — see `wind_charge_model`'s
///   doc), placed by [`non_living_vehicle_matrix`] rather than by
///   [`entity_model_matrix`]. This entry stays for the same reason the
///   arrow/trident one below does: it is what stops either type being added to
///   the table below, which would draw an item billboard over the mesh.
/// * **`arrow`, `spectral_arrow` and `trident` use their own dedicated projectile renderers**,
///   a 3-D cuboid rig, not an item billboard. Those three are now in the
///   [`entity_models`](lodestone_assets::entity_models) corpus and are placed by
///   [`projectile_model_matrix`] rather than by [`entity_model_matrix`]; see
///   `docs/projectile-renderers.md`. This entry stayed here after they landed
///   because the fact it records — that they are *not* billboarded-thrown-item
///   entries — is what stops them being added to the table below, which would
///   draw an item sprite over the mesh.
///
///   The note this replaced said the orientation "needs a velocity the draw
///   record does not carry". That was the wrong conclusion from a true premise:
///   vanilla derives its own yaw/pitch from `atan2` on velocity, but it does so on
///   the *server* too (in its projectile-shoot and arrow-tick logic) and then
///   broadcasts the result as ordinary entity rotation. The draw record's
///   existing `yaw`/`pitch` **are** those velocity-derived angles, so no velocity
///   plumbing was needed.
///
/// # "Not a billboarded-thrown-item entry" and "not drawn as an item" are two claims
///
/// The note this paragraph replaced ran them together, listing
/// `dragon_fireball`, `wither_skull`, `llama_spit`, `shulker_bullet`,
/// `fishing_bobber`, `firework_rocket` and `end_crystal` as "dedicated renderers
/// too, and likewise absent". The first claim is true of all seven and this
/// table's membership is exactly that set — a parity gate in
/// `tests/thrown_and_held_item_pixels.rs` checks it against the vanilla
/// registration list, so widening the table is widening what the table *means*.
///
/// The second claim is false of one, and knowing which matters to whoever picks
/// the rest up. Vanilla's firework-entity renderer **does** draw an item model billboarded
/// on `camera.orientation`, exactly the way these entries are drawn. What keeps
/// it out is not its geometry but its inputs: the stack comes from the entity
/// rather than from a default, and a rocket fired from a crossbow is spun onto
/// its flight axis by a metadata bit the draw record does not carry. Adding it
/// here would make the table mean "types drawn as a billboarded item", which is
/// a *different* table and would take the parity gate's premise with it.
///
/// Four of the other six now have corpus rigs of their own — `wither_skull`,
/// `llama_spit`, `shulker_bullet` and `evoker_fangs` — placed by
/// [`non_living_vehicle_matrix`] or [`projectile_model_matrix`]. They stay out of
/// this table for the same reason the arrows do: an item sprite drawn over a mesh
/// is two wrong things at once.
///
/// `dragon_fireball` and `fishing_bobber` are still absent and neither is an
/// item: the first is a single camera-facing quad built vertex by vertex from a
/// texture, the second a billboard plus a line back to the caster. Both need a
/// draw path this crate does not have.
#[must_use]
pub fn thrown_item_for(type_path: &str) -> Option<ThrownItem> {
    // `(entity type, default item, scale, full_bright)`.
    const TABLE: &[(&str, &str, f32, bool)] = &[
        ("egg", "minecraft:egg", 1.0, false),
        ("ender_pearl", "minecraft:ender_pearl", 1.0, false),
        (
            "experience_bottle",
            "minecraft:experience_bottle",
            1.0,
            false,
        ),
        // Vanilla's eye-of-ender's default-item accessor resolves to the ender-eye item — the *item* id is
        // `ender_eye`, not `eye_of_ender`, which is the entity type. Using the
        // entity name here resolves no item and draws nothing.
        ("eye_of_ender", "minecraft:ender_eye", 1.0, true),
        ("fireball", "minecraft:fire_charge", 3.0, true),
        ("lingering_potion", "minecraft:lingering_potion", 1.0, false),
        ("small_fireball", "minecraft:fire_charge", 0.75, true),
        ("snowball", "minecraft:snowball", 1.0, false),
        ("splash_potion", "minecraft:splash_potion", 1.0, false),
    ];
    TABLE
        .iter()
        .find(|(name, ..)| *name == type_path)
        .map(|&(_, item, scale, full_bright)| ThrownItem {
            item,
            scale,
            full_bright,
        })
}

/// The **camera→world rotation**, which is what vanilla's `camera.orientation`
/// is: apply it to a model authored facing camera-space `+Z` and the model faces
/// the eye.
///
/// # Derived from the view matrix, not written out as `Ry(yaw)·Rx(pitch)`
///
/// Every hand-written form of this was wrong on the first try, in a different way
/// each time, because three conventions stack: vanilla's own quaternion is
/// `rotationYXZ(π - yaw, -pitch, 0)` (note the `π -`, which exists because MC's
/// camera space is rotated 180° from its world space), `glam`'s right-handed view
/// looks down **-Z**, and [`Camera::forward`](crate::Camera::forward) is
/// Minecraft's convention (`yaw 0` faces `+Z`). Taking the view matrix and
/// inverting its rotation cannot get any of those backwards: a view matrix is
/// `R · T` with `R` orthonormal, so `R⁻¹ = Rᵀ`.
///
/// Pass [`Camera::view_matrix`](crate::Camera::view_matrix). The determinant is
/// `+1`, so this does not flip winding — see [`thrown_item_matrix`].
///
/// # Why the item's front face lands the right way round either way
///
/// A flat sprite item is [`extruded_sprite_geometry`](crate::BlockModels)'s slab,
/// whose `SOUTH` face (outward normal `+Z`) carries UVs `(0, 0, 16, 16)` and whose
/// `NORTH` face carries `(16, 0, 0, 16)` — the reversed `u`. That flip is exactly
/// what makes *both* faces read unmirrored from their own side, so a 180°
/// yaw error here is invisible on the sprite items, which is every entity in
/// [`thrown_item_for`]. What is **not** invisible is getting the *pitch* term
/// wrong (an upside-down snowball) or dropping the rotation entirely (a slab seen
/// edge-on from the side, i.e. a near-invisible sliver).
#[must_use]
pub fn camera_orientation(view_matrix: Mat4) -> Mat4 {
    let mut rotation = view_matrix;
    rotation.w_axis = Vec4::W;
    rotation.transpose()
}

/// The world placement matrix for a thrown item projectile, matching
/// vanilla's thrown-item-renderer submit function's pose-stack order exactly:
///
/// ```text
/// T(position) · S(scale) · camera_orientation · display_matrix(ground)
/// ```
///
/// `orientation` is [`camera_orientation`]`(camera.view_matrix())` and `ground`
/// the item's own [`ground_transform`] — the `GROUND` display context
/// vanilla's own render-state extraction resolves the item in.
///
/// **No bob, no spin, no hover lift.** Those three are vanilla's dropped-item renderer's and
/// are the tempting thing to reuse from [`dropped_item_matrix`]; a bobbing,
/// spinning snowball in flight is the signature of having done so.
///
/// The determinant is **positive** — a translation, a positive uniform scale, a
/// rotation and `display_matrix`'s positive scale — so this composes with
/// `Camera::view_projection` to the same winding as terrain, exactly like a drop.
#[must_use]
pub fn thrown_item_matrix(
    position: Vec3,
    orientation: Mat4,
    scale: f32,
    ground: &DisplayTransform,
) -> Mat4 {
    Mat4::from_translation(position)
        * Mat4::from_scale(Vec3::splat(scale))
        * orientation
        * display_matrix(ground)
}

/// Mesh one thrown item projectile into a world-space [`ModelMesh`], for the same
/// pass and the same camera uniform [`dropped_item_mesh`] feeds.
///
/// `light` is the packed sky/block sample at the projectile, or
/// [`GUI_ITEM_LIGHT`](crate::GUI_ITEM_LIGHT) when [`ThrownItem::full_bright`] is
/// set — vanilla's own block-light-level override returns `15` for the fireballs
/// and the eye of ender, which is what makes a fireball readable against a dark
/// Nether ceiling.
#[must_use]
pub fn thrown_item_mesh(
    quads: &[BakedQuad],
    gui_light: GuiLight,
    ground: &DisplayTransform,
    position: Vec3,
    orientation: Mat4,
    scale: f32,
    light: u8,
) -> ModelMesh {
    let pose = thrown_item_matrix(position, orientation, scale, ground);
    mesh_item_quads_with_light(quads, pose, gui_light, light)
}

// ---------------------------------------------------------------------------
// `minecraft:special` items on the 3-D world surfaces
// ---------------------------------------------------------------------------
//
// A chest, shulker box or skull has no item model and no block model — every
// triangle comes from a block-entity rig, resolved once by
// [`crate::special_item_rig`]. The *poses* are the ordinary item poses; only the
// geometry source differs. So the two helpers here are the pieces that
// [`dropped_item_matrix`]/[`held_item_matrix`] need and that a rig cannot supply
// the way a quad list can:
//
// * a hover lift measured from the rig's own AABB rather than from quads;
// * an item-frame pose, which no baked-item path needed because a framed item
//   drew nothing at all before.
//
// Everything else is shared with the baked path *by calling the same function*,
// which is the point: a chest and a pickaxe must bob, spin and hang on identical
// arcs. See `docs/held-block-entity-items.md`.

/// Vanilla's own minimum-Y offset for a **rig** rather than a quad list: the lift that
/// puts the posed rig's lowest point [`ITEM_MIN_HOVER_HEIGHT`] above the drop's
/// own position.
///
/// `local_min`/`local_max` are a `BlockEntityMesh`'s rest-pose AABB, and `ground`
/// the item's own `display.ground`. All eight corners are transformed, not just
/// `local_min`: `display_matrix` can rotate, and under a rotation the lowest
/// point of the posed box is not the image of the lowest point of the original
/// one. Transforming `local_min` alone is the plausible wrong version — it agrees
/// exactly whenever the transform has no rotation, which is true of most `ground`
/// transforms and false of the ones that matter.
#[must_use]
pub fn special_item_hover_lift(local_min: Vec3, local_max: Vec3, ground: &DisplayTransform) -> f32 {
    let pose = display_matrix(ground);
    let mut min_y = f32::INFINITY;
    for i in 0..8u8 {
        let corner = Vec3::new(
            if i & 1 == 0 { local_min.x } else { local_max.x },
            if i & 2 == 0 { local_min.y } else { local_max.y },
            if i & 4 == 0 { local_min.z } else { local_max.z },
        );
        min_y = min_y.min(pose.transform_point3(corner).y);
    }
    if min_y.is_finite() {
        -min_y + ITEM_MIN_HOVER_HEIGHT
    } else {
        ITEM_MIN_HOVER_HEIGHT
    }
}

// ---------------------------------------------------------------------------
// Item frames
// ---------------------------------------------------------------------------
//
// Vanilla's item-frame renderer — the frame body, and whatever hangs in it. Both are posed
// out of one shared chain, [`item_frame_space`], for the reason the boat's hull
// and its water patch share one: two matrices that must agree can only be
// guaranteed to agree by being the same matrix.

/// `translate(0.0F, 0.0F, 0.4375F)` on vanilla's own transform stack — how far
/// in front of the frame's own plane a *visible* frame's contents sit.
pub(crate) const ITEM_FRAME_CONTENT_LIFT: f32 = 0.4375;

/// The same step for an **invisible** frame, vanilla's `translate(0, 0, 0.5F)`.
/// A frame with no body to hold it clear needs the extra 1/16 to stay out of the
/// wall behind it.
pub(crate) const ITEM_FRAME_INVISIBLE_CONTENT_LIFT: f32 = 0.5;

/// `scale(0.5F, 0.5F, 0.5F)` in vanilla's item-frame renderer submit function's item
/// branch — the one number here a reader is likely to assume is `1.0` from the
/// framed *map* path. A map is drawn a full block across by its own separate
/// branch, and copying that would draw a chest twice the size of the frame
/// around it.
pub(crate) const FRAMED_ITEM_SCALE: f32 = 0.5;

/// One eighth turn, `rotation * 360 / 8` from vanilla's per-eighth-turn axis
/// rotation in the
/// item branch.
pub(crate) const FRAMED_ITEM_ROTATION_STEP_DEG: f32 = 45.0;

/// The frame's own rotation: `Rx(pitch) · Ry(yaw)` exactly as
/// vanilla's item-frame renderer submit function pushes it, re-expressed in the `(yaw, pitch)` the
/// wire actually carries.
///
/// # Why `180 - yaw`, and why the pitch passes through unchanged
///
/// The renderer derives its two angles from the frame's facing direction, which is not
/// on the wire; vanilla's frame direction-setter derives the entity's own yaw/pitch
/// from that same direction, and those *are*. Composing the two derivations
/// eliminates it:
///
/// | direction | renderer | entity |
/// |---|---|---|
/// | horizontal | `pitch = 0`, `yaw = 180 - dir_yaw` | `pitch = 0`, `yaw = dir.2d_index() * 90` |
/// | vertical | `pitch = -90 * step`, `yaw = 180` | `pitch = -90 * step`, `yaw = 0` |
///
/// Vanilla's direction-to-yaw conversion **is** `dir.2d_index() * 90`, so the horizontal row is
/// `yaw_render = 180 - yaw_entity`; and the vertical row's `yaw_entity` is `0`,
/// so `180 - yaw_entity` is `180` there too. One expression covers both, and the
/// pitch is the entity's own in either case.
///
/// The `180 -` is the half of this that a reader will want to drop, because
/// dropping it still produces a frame flat against a wall — just the *wrong* wall,
/// with its back plate facing the room.
#[must_use]
pub fn item_frame_facing(yaw_deg: f32, pitch_deg: f32) -> Mat4 {
    Mat4::from_rotation_x(pitch_deg.to_radians())
        * Mat4::from_rotation_y((180.0 - yaw_deg).to_radians())
}

/// The unit vector along the frame's `Direction` — the way it faces out of its
/// wall — from the same two angles.
///
/// Derived from [`item_frame_facing`] rather than from a `Direction` table, and
/// the `NEG_Z` is why: the frame's model has its back plate at local `+z` (the
/// `template_item_frame` element spans `z = 15.5..16`), so after the facing
/// rotation local `-z` is *by construction* the direction the frame looks. A
/// separate table would be a second place for the same fact to be wrong, and it
/// would agree with this one at yaw `0` and `180` — the two inputs a test is most
/// likely to pick.
#[must_use]
pub fn item_frame_facing_step(yaw_deg: f32, pitch_deg: f32) -> Vec3 {
    item_frame_facing(yaw_deg, pitch_deg).transform_vector3(Vec3::NEG_Z)
}

/// The frame's own space: origin at the **centre of its attachment block**, `+z`
/// into the wall behind it, in the `(packet_anchor, yaw, pitch)` terms the shell
/// has for vanilla's own hanging-entity base class.
///
/// ```text
/// T(floor(packet_anchor) + (0.5, 0.5, 0.5)) · Rx(pitch) · Ry(180 - yaw)
/// ```
///
/// Everything vanilla's item-frame renderer submit function draws is posed relative to this: the
/// body at `T(-0.5, -0.5, -0.5)` (block models are corner-origin), the contents
/// at `T(0, 0, 0.4375)`. Vanilla's item-frame spawn-packet builder sends its own
/// position accessor — the integer attachment `BlockPos` — rather than the entity centre created by
/// its bounding-box recalculation. The dispatcher offset and the renderer's matching
/// negative offset cancel, then the renderer's `direction * .46875` cancels the
/// entity-centre displacement, leaving exactly this block centre.
#[must_use]
pub fn item_frame_space(packet_anchor: Vec3, yaw_deg: f32, pitch_deg: f32) -> Mat4 {
    Mat4::from_translation(packet_anchor.floor() + Vec3::splat(0.5))
        * item_frame_facing(yaw_deg, pitch_deg)
}

/// Vanilla's item-frame entity bounds after its base renderer's should-render
/// check inflates them by half a block, returned as `(min, max)`.
///
/// The entity is not centred on its packet attachment anchor: vanilla's item-frame entity
/// moves its bounding-box centre `0.46875` blocks away from the attachment
/// block centre, gives the wall-normal axis a thickness of `1/16`, and uses a
/// one-block square when it holds a map (`3/4` otherwise). The renderer then
/// inflates that exact box by `0.5`. A symmetric box around the packet anchor
/// misses the inflated room-facing edge and can cull visible contents at a
/// grazing camera angle.
#[must_use]
pub fn item_frame_culling_aabb(
    packet_anchor: Vec3,
    yaw_deg: f32,
    pitch_deg: f32,
    has_map: bool,
) -> (Vec3, Vec3) {
    let facing = item_frame_facing_step(yaw_deg, pitch_deg);
    let centre = packet_anchor.floor() + Vec3::splat(0.5) - facing * 0.46875;
    let face_size = if has_map { 1.0 } else { 0.75 };
    let thin = 0.0625;
    let axis = facing.abs();
    let size = if axis.x >= axis.y && axis.x >= axis.z {
        Vec3::new(thin, face_size, face_size)
    } else if axis.y >= axis.z {
        Vec3::new(face_size, thin, face_size)
    } else {
        Vec3::new(face_size, face_size, thin)
    };
    let half = size * 0.5 + Vec3::splat(0.5);
    (centre - half, centre + half)
}

/// The world placement for the frame **body** — the wooden border and back
/// plate — as a transform over block-local `0.0..=1.0` model quads.
///
/// Vanilla's item-frame renderer submit function's push-pose, translate(-0.5, -0.5, -0.5),
/// then submit-with-Z-offset. The `-0.5`s are the block model's
/// corner-origin convention, not a centring fudge — the same pair
/// `falling_block_pose` applies for the same reason.
#[must_use]
pub fn item_frame_body_matrix(packet_anchor: Vec3, yaw_deg: f32, pitch_deg: f32) -> Mat4 {
    item_frame_space(packet_anchor, yaw_deg, pitch_deg) * Mat4::from_translation(Vec3::splat(-0.5))
}

/// How far in front of the frame's plane its contents sit, `invisible` selecting
/// between vanilla's two `translate` calls.
#[must_use]
pub fn item_frame_content_lift(invisible: bool) -> f32 {
    if invisible {
        ITEM_FRAME_INVISIBLE_CONTENT_LIFT
    } else {
        ITEM_FRAME_CONTENT_LIFT
    }
}

/// The world placement for an item hanging in an item frame:
///
/// ```text
/// item_frame_space · T(0, 0, lift) · Rz(rotation · 45°) · S(0.5) · display_matrix(fixed)
/// ```
///
/// `rotation` is vanilla's item-frame rotation accessor, `0..8`; `invisible` is
/// `state.isInvisible`, which swaps the lift (see [`item_frame_content_lift`]).
///
/// # The sign trap
///
/// The lift is along the frame's own local `+z`, which after
/// [`item_frame_facing`] points **into** the wall. From the packet's attachment
/// block centre, visible contents therefore land `0.4375` toward its wall face;
/// equivalently, they are `1/16` outside that face. Getting that sign wrong (or
/// dropping `180 - yaw`) sends contents through the attachment block instead.
#[must_use]
pub fn framed_item_matrix(
    packet_anchor: Vec3,
    yaw_deg: f32,
    pitch_deg: f32,
    rotation: u8,
    invisible: bool,
    fixed: &DisplayTransform,
) -> Mat4 {
    item_frame_space(packet_anchor, yaw_deg, pitch_deg)
        * Mat4::from_translation(Vec3::new(0.0, 0.0, item_frame_content_lift(invisible)))
        * Mat4::from_rotation_z(
            (f32::from(rotation % 8) * FRAMED_ITEM_ROTATION_STEP_DEG).to_radians(),
        )
        * Mat4::from_scale(Vec3::splat(FRAMED_ITEM_SCALE))
        * display_matrix(fixed)
}

/// Mesh an ordinary (baked-quad) item hanging in an item frame into a
/// world-space [`ModelMesh`], for the same model-pipeline draw
/// [`dropped_item_mesh`] feeds.
///
/// The rig-shaped items — a chest, a shulker box, a skull — go through
/// [`framed_item_matrix`] and the block-entity pass instead; this is the other
/// 99% of the item registry, and its absence is why a sword in a frame drew
/// nothing while a chest in one drew fine.
///
/// `fixed` is the item's own `display.fixed`, composed on the right for the
/// identical reason [`campfire_item_mesh`] composes there:
/// vanilla's item-frame render-state extraction resolves the stack in
/// the fixed display context, and vanilla applies that transform inside
/// its item-stack render-state submit function, after every pose the renderer itself pushes.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn framed_item_mesh(
    quads: &[BakedQuad],
    gui_light: GuiLight,
    fixed: &DisplayTransform,
    packet_anchor: Vec3,
    yaw_deg: f32,
    pitch_deg: f32,
    rotation: u8,
    invisible: bool,
    light: u8,
) -> ModelMesh {
    let pose = framed_item_matrix(packet_anchor, yaw_deg, pitch_deg, rotation, invisible, fixed);
    mesh_item_quads_with_light(quads, pose, gui_light, light)
}

// ---------------------------------------------------------------------------
// Experience orbs
// ---------------------------------------------------------------------------
//
// Vanilla's experience-orb renderer, which is one camera-facing quad and nothing else. It
// is **not** an [`entity_models`] rig and never will be, exactly as a dropped
// item is not: `model_for_type("experience_orb")` and
// `entity_texture_candidates("experience_orb")` are both deliberately empty and
// stay that way (see `unknown_entity_type_has_no_model`). The corpus holds cuboid
// part hierarchies; this is a sprite, and it draws through the same
// billboard-with-its-own-sheet shape the mob-fire layer uses.
