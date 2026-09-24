use super::*;

/// A baked wearable mesh whose parts attach to a wearer's skeleton.
#[derive(Debug, Clone)]
pub struct ArmourMesh {
    /// Four vertices per quad, part-local (the part's own pose is *not* folded
    /// in — the wearer's matrix supplies it).
    pub vertices: Vec<ModelVertex>,
    /// Six indices per quad, wound so front faces point outward.
    pub indices: Vec<u32>,
    /// `(wearer part name, index range)` for every part that actually carries
    /// geometry, in bake order. Parts pruned by the slot's retention rule are
    /// absent rather than present-and-empty, so a caller cannot accidentally
    /// issue a zero-index draw.
    pub parts: Vec<(&'static str, PartRange)>,
}

impl ArmourMesh {
    /// Bake the mesh for one slot.
    #[must_use]
    pub fn for_slot(slot: ArmourSlot) -> Self {
        let def = humanoid_armour_model(slot);
        let baked = bake_entity_parts(&def);
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        let mut parts = Vec::new();
        for part in &baked {
            if part.quads.is_empty() {
                continue;
            }
            // Resolve the baked name back to the `&'static str` the slot
            // declares, so the pairing in `attach` is a pointer-cheap compare
            // and a name this mesh carries but the slot does not is a bake bug
            // that shows up here rather than as a missing draw.
            let Some(name) = slot
                .part_names()
                .iter()
                .find(|n| **n == part.name.as_str())
                .copied()
            else {
                continue;
            };
            let index_start = indices.len() as u32;
            let vertex_start = vertices.len() as u32;
            push_part_quads(&part.quads, &mut vertices, &mut indices);
            parts.push((
                name,
                PartRange {
                    index_start,
                    index_count: indices.len() as u32 - index_start,
                    vertex_start,
                    vertex_count: vertices.len() as u32 - vertex_start,
                },
            ));
        }
        ArmourMesh {
            vertices,
            indices,
            parts,
        }
    }

    /// Number of quads in the mesh.
    #[must_use]
    pub fn quad_count(&self) -> usize {
        self.indices.len() / 6
    }

    /// Pair each of this mesh's parts with the index of the wearer's part of the
    /// same name, dropping any part the wearer's rig does not have.
    ///
    /// The caller then reads `instance.part_transforms[wearer_index]` — the
    /// wearer's own, already-animated, already-world-space matrix — and draws
    /// `range` instanced over it. Nothing is written back: see this section's
    /// header for why an armour layer must not touch `part_transforms`.
    ///
    /// A non-humanoid rig yields nothing — see [`wearer_carries_armour`], which
    /// this enforces so a caller cannot forget it.
    pub fn attach<'a>(
        &'a self,
        wearer: &'a Skeleton,
    ) -> impl Iterator<Item = (PartRange, usize)> + 'a {
        let humanoid = wearer_carries_armour(wearer);
        self.parts
            .iter()
            .filter(move |_| humanoid)
            .filter_map(|(name, range)| wearer.index_of(name).map(|i| (*range, i)))
    }
}

/// Whether a rig wears humanoid armour at all.
///
/// Vanilla's real gate is which *renderer* owns a humanoid armour layer
/// (its base humanoid mob renderer, its player renderer, its armour-stand
/// renderer, the piglin
/// and zombie families), and the structural equivalent here is the animation
/// family: [`AnimFamily::Humanoid`] is exactly "has both arms and both legs",
/// which is what vanilla's base humanoid model means.
///
/// **Part names alone are not sufficient and that is the trap.** A pig has both
/// `head` and `body`, so a chestplate keyed on part names would attach its
/// `body` cube to a pig's torso and draw a floating breastplate on a farm
/// animal — geometry that resolves perfectly and is completely wrong. Vanilla
/// draws nothing there.
#[must_use]
pub fn wearer_carries_armour(wearer: &Skeleton) -> bool {
    wearer.family() == crate::entity_anim::AnimFamily::Humanoid
}

/// The four baked humanoid armour meshes, one per [`ArmourSlot`].
///
/// Built once (CPU only, like [`EntityModelSet`]) and uploaded once; a mob's
/// armour costs one instance matrix per drawn part, exactly as its own body
/// does.
#[derive(Debug, Clone)]
pub struct ArmourModelSet {
    meshes: Vec<(ArmourSlot, ArmourMesh)>,
}

impl Default for ArmourModelSet {
    fn default() -> Self {
        Self::load()
    }
}

impl ArmourModelSet {
    /// Bake all four slot meshes, in [`ArmourSlot::ALL`] order — which is
    /// vanilla's own armour-layer submit order, so a caller that walks
    /// [`iter`](Self::iter) draws in vanilla's sequence.
    #[must_use]
    pub fn load() -> Self {
        Self {
            meshes: ArmourSlot::ALL
                .into_iter()
                .map(|slot| (slot, ArmourMesh::for_slot(slot)))
                .collect(),
        }
    }

    /// The baked mesh for a slot.
    #[must_use]
    pub fn get(&self, slot: ArmourSlot) -> Option<&ArmourMesh> {
        self.meshes
            .iter()
            .find(|(s, _)| *s == slot)
            .map(|(_, m)| m)
    }

    /// Every `(slot, mesh)` pair, in submit order (for uploading each once).
    pub fn iter(&self) -> impl Iterator<Item = (ArmourSlot, &ArmourMesh)> {
        self.meshes.iter().map(|(s, m)| (*s, m))
    }
}

// ---------------------------------------------------------------------------
// Sheep wool
// ---------------------------------------------------------------------------
//
// The wool layer follows exactly the humanoid-armour discipline above — a
// second, independently-baked mesh posed off the *wearer's* already-animated
// `part_transforms`, never a second skeleton — with one structural
// simplification and one structural trap that armour does not have:
//
// * **One mesh, not one per slot.** Armour needs [`ArmourModelSet`] because
//   the four slots bake different geometry; wool is a single overlay over the
//   whole sheep body, so [`WoolMesh`] has no per-slot table.
// * **The gate cannot live inside the mesh geometry the way `ArmourMesh`'s
//   does.** [`wearer_carries_armour`] reads the wearer's *animation family*,
//   which is a structural property `sheep`, `pig`, `cow` and `wolf` all share
//   — a farm animal has no `head`/`body` parts that would make a chestplate
//   attach fail. Wool cannot reuse that gate: it must be keyed on the
//   wearer's **resolved model name being exactly `"sheep"`**
//   (`docs/entity-rendering.md`'s "pig/cow trap, worse"), so [`WoolMesh::attach`]
//   takes the resolved model name as a second argument rather than reading it
//   off the [`Skeleton`] the way armour's `wearer.family()` check does.

/// [`sheep_wool_model`](lodestone_assets::entity_models::sheep_wool_model)'s
/// six named parts, in the order [`WoolMesh::load`] bakes them — the same
/// pre-order `sheep_model`'s body shares (pinned by
/// `sheep_wool_model_shares_sheep_body_part_names_and_pivots` in
/// `lodestone-assets/tests/entity_models.rs`).
pub(crate) const SHEEP_WOOL_PART_NAMES: [&str; 6] = [
    "head",
    "body",
    "right_hind_leg",
    "left_hind_leg",
    "right_front_leg",
    "left_front_leg",
];

/// The sheep wool overlay's baked mesh, in the shared part-local
/// [`ModelVertex`] format, with its parts keyed by the **sheep body's** part
/// names — the same shape as [`ArmourMesh`], minus the per-slot table, since
/// wool has only one variant.
#[derive(Debug, Clone)]
pub struct WoolMesh {
    /// Four vertices per quad, part-local (the part's own pose is *not*
    /// folded in — the wearer's matrix supplies it).
    pub vertices: Vec<ModelVertex>,
    /// Six indices per quad, wound so front faces point outward.
    pub indices: Vec<u32>,
    /// `(sheep body part name, index range)` for every part that carries
    /// geometry, in bake order.
    pub parts: Vec<(&'static str, PartRange)>,
}

impl WoolMesh {
    /// Bake the wool overlay mesh.
    #[must_use]
    pub fn load() -> Self {
        let def = lodestone_assets::entity_models::sheep_wool_model();
        let baked = bake_entity_parts(&def);
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        let mut parts = Vec::new();
        for part in &baked {
            if part.quads.is_empty() {
                continue;
            }
            // Same discipline as `ArmourMesh::for_slot`: resolve the baked
            // name back to the `&'static str` this module owns, so a name
            // this mesh carries but the constant list does not is a bake bug
            // caught here rather than as a missing draw.
            let Some(name) = SHEEP_WOOL_PART_NAMES
                .iter()
                .find(|n| **n == part.name.as_str())
                .copied()
            else {
                continue;
            };
            let index_start = indices.len() as u32;
            let vertex_start = vertices.len() as u32;
            push_part_quads(&part.quads, &mut vertices, &mut indices);
            parts.push((
                name,
                PartRange {
                    index_start,
                    index_count: indices.len() as u32 - index_start,
                    vertex_start,
                    vertex_count: vertices.len() as u32 - vertex_start,
                },
            ));
        }
        WoolMesh {
            vertices,
            indices,
            parts,
        }
    }

    /// Number of quads in the mesh.
    #[must_use]
    pub fn quad_count(&self) -> usize {
        self.indices.len() / 6
    }

    /// Pair each of this mesh's parts with the index of the wearer's part of
    /// the same name, dropping every part when `wearer_model` is not
    /// `"sheep"`.
    ///
    /// The caller then reads `instance.part_transforms[wearer_index]` and
    /// draws `range` instanced over it, exactly [`ArmourMesh::attach`]'s
    /// contract. `wearer_model` is the resolved
    /// [`EntityModelSet::resolve`] model name (`instance.model` /
    /// [`EntityBatch::model`]) — **never** [`Skeleton::family`], because
    /// `AnimFamily::Quadruped` is shared by `pig`, `cow` and `wolf`: gating on
    /// family alone would grow wool on a pig exactly as an ungated armour
    /// attach once drew a breastplate on one. See this section's header.
    pub fn attach<'a>(
        &'a self,
        wearer: &'a Skeleton,
        wearer_model: &str,
    ) -> impl Iterator<Item = (PartRange, usize)> + 'a {
        let is_sheep = wearer_model == "sheep";
        self.parts
            .iter()
            .filter(move |_| is_sheep)
            .filter_map(|(name, range)| wearer.index_of(name).map(|i| (*range, i)))
    }
}

/// The sheep wool overlay's CPU model, loaded once. There is only one
/// [`WoolMesh`] (wool has no per-material variant the way armour does), so
/// unlike [`ArmourModelSet`] this holds a single mesh rather than a table —
/// the wrapper exists for symmetry with the armour loading path and so a
/// future second wool variant (e.g. a baby rig) has somewhere to live.
#[derive(Debug, Clone)]
pub struct SheepWoolModelSet {
    mesh: WoolMesh,
}

impl Default for SheepWoolModelSet {
    fn default() -> Self {
        Self::load()
    }
}

impl SheepWoolModelSet {
    /// Bake the wool mesh.
    #[must_use]
    pub fn load() -> Self {
        Self {
            mesh: WoolMesh::load(),
        }
    }

    /// The baked wool mesh.
    #[must_use]
    pub fn mesh(&self) -> &WoolMesh {
        &self.mesh
    }
}

// ---------------------------------------------------------------------------
// The player's cape
// ---------------------------------------------------------------------------
//
// Structurally the same "second, independently-baked mesh posed off the
// wearer's already-animated `part_transforms`" discipline as armour and wool
// above, with one difference from both: the cape needs an **extra** local
// transform on top of the wearer's body matrix (the per-frame lean/flap
// rotation), where armour and wool reuse the wearer's part matrix verbatim.
// `attach` therefore hands back the same `(PartRange, wearer_index)` pairing
// as the other two — the caller is what composes the extra matrix in, once
// per instance, via [`cape_local_rotation`].

/// The player cape's baked mesh: one part, `"cape"`, in the wearer's
/// **body-pivot-local** space (see [`lodestone_assets::entity::player_cape_model`]
/// for why no rotation is baked in).
#[derive(Debug, Clone)]
pub struct CapeMesh {
    /// Four vertices per quad, part-local.
    pub vertices: Vec<ModelVertex>,
    /// Six indices per quad.
    pub indices: Vec<u32>,
    /// Always exactly one entry, `("cape", range)` — kept as a list rather
    /// than a bare range for the same reason [`ArmourMesh::parts`] is: a bake
    /// that produced no quads (a malformed model) yields an empty list rather
    /// than a range into nothing.
    pub parts: Vec<(&'static str, PartRange)>,
}

impl CapeMesh {
    /// Bake the cape mesh.
    #[must_use]
    pub fn load() -> Self {
        let def = lodestone_assets::entity::player_cape_model();
        let baked = bake_entity_parts(&def);
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        let mut parts = Vec::new();
        for part in &baked {
            if part.name.as_str() != "cape" || part.quads.is_empty() {
                continue;
            }
            let index_start = indices.len() as u32;
            let vertex_start = vertices.len() as u32;
            push_part_quads(&part.quads, &mut vertices, &mut indices);
            parts.push((
                "cape",
                PartRange {
                    index_start,
                    index_count: indices.len() as u32 - index_start,
                    vertex_start,
                    vertex_count: vertices.len() as u32 - vertex_start,
                },
            ));
        }
        CapeMesh {
            vertices,
            indices,
            parts,
        }
    }

    /// Pair the cape part with the wearer's `"body"` part, dropping it
    /// entirely for a non-humanoid rig — same gate [`wearer_carries_armour`]
    /// uses, and for the same reason (a farm animal has no `body` pivot a
    /// cape should hang from).
    pub fn attach<'a>(
        &'a self,
        wearer: &'a Skeleton,
    ) -> impl Iterator<Item = (PartRange, usize)> + 'a {
        let humanoid = wearer_carries_armour(wearer);
        self.parts
            .iter()
            .filter(move |_| humanoid)
            .filter_map(|(name, range)| wearer.index_of(name).map(|i| (*range, i)))
    }
}

/// The per-frame cape placement, relative to the wearer's **body** part
/// transform: translate to the pivot vanilla's cape-layer builder gives it
/// (`(0, 0, 2)` model texels), then rotate.
///
/// `lean`/`lean2`/`flap` are vanilla's per-frame cape-lean, cape-lean2 and
/// cape-flap values, in **degrees** — see
/// `lodestone_shell::entities::cape_sway` for how those three are derived
/// from the lagged "cloak" position each frame.
///
/// # The rotation, derived from a rotate-onto-existing-pose composition
///
/// Vanilla's per-frame animation step does not set a rotation on the cape, it
/// **composes** one onto the cape's existing pose:
///
/// ```text
/// old_rotation = rotationZYX(zRot, yRot, xRot)
/// new_rotation = old_rotation.rotate(rotation)
/// ```
///
/// i.e. `new = old * rotation` (the underlying rotation composition always
/// post-multiplies). The cape's `old` rotation is the static pose the model
/// builder gives it, `Ry(pi)` (it hangs facing backward), and the `rotation`
/// argument is itself built by chained rotate-Y/X/Z calls — each one *also* a
/// post-multiply — so:
///
/// ```text
/// rotation = Ry(-pi) * Rx(theta_x) * Rz(theta_z) * Ry(theta_y2)
/// new      = Ry(pi) * rotation
///          = [Ry(pi) * Ry(-pi)] * Rx(theta_x) * Rz(theta_z) * Ry(theta_y2)
///          = Rx(theta_x) * Rz(theta_z) * Ry(theta_y2)
/// ```
///
/// The static `Ry(pi)` and the quaternion's leading `Ry(-pi)` are exact
/// inverses on the same axis and cancel — which is exactly why
/// [`lodestone_assets::entity::player_cape_model`] bakes no rotation at all:
/// baking `Ry(pi)` here would double it instead of cancelling it.
///
/// `theta_x = 6 + lean/2 + flap`, `theta_z = lean2/2`,
/// `theta_y2 = 180 - lean2/2`, all degrees, straight out of vanilla's
/// per-frame cape animation step.
#[must_use]
pub fn cape_local_rotation(lean: f32, lean2: f32, flap: f32) -> Mat4 {
    let theta_x = (6.0 + lean / 2.0 + flap).to_radians();
    let theta_z = (lean2 / 2.0).to_radians();
    let theta_y2 = (180.0 - lean2 / 2.0).to_radians();
    let translate = Mat4::from_translation(Vec3::new(0.0, 0.0, 2.0 / 16.0));
    let rotate = Mat4::from_rotation_x(theta_x) * Mat4::from_rotation_z(theta_z) * Mat4::from_rotation_y(theta_y2);
    translate * rotate
}

// ---------------------------------------------------------------------------
// The elytra
// ---------------------------------------------------------------------------
//
// The same "second mesh posed off the wearer's already-animated
// `part_transforms`" discipline as armour, wool and the cape above. It sits
// closest to the cape: both hang off the wearer's `"body"` part and both need
// an **extra** local transform the caller composes in per frame.
//
// It differs from the cape in three ways that each cost something to get
// wrong, so they are named here rather than left to be rediscovered:
//
//  * **Two parts, not one**, and their transforms are not equal — the right
//    wing negates the left's Y and Z rotation. A single shared matrix draws
//    both wings folded the same way, which reads as "one wing is inside out".
//  * **The draw gate is the chest equipment slot**, not a texture URL. An
//    elytra is worn where a chestplate goes, and vanilla's real gate is that
//    the piece's `equipment/<asset>.json` declares a `wings` layer at all —
//    which is why a diamond chestplate, whose asset declares `humanoid` and
//    `humanoid_leggings` and no `wings`, draws nothing here.
//  * **The elytra layer's submit step translates the whole layer `+0.125` on
//    Z** before anything else, to clear the wearer's own body. That is 0.125
//    *blocks* (the pose stack is in blocks at layer level; the underlying
//    part-render step is what divides texels by 16), i.e. 2 texels —
//    numerically the same as the cape's `z = 2` pivot, and a different
//    quantity with a different origin.

/// The elytra's baked mesh: two parts, `"left_wing"` and `"right_wing"`, in
/// the wearer's **body-pivot-local** space.
///
/// See [`lodestone_assets::entity::elytra_model`] for why neither the static
/// pose rotation nor the crouch `y` is baked in, and
/// [`elytra_wing_transform`] for what the caller must compose per wing.
#[derive(Debug, Clone)]
pub struct ElytraMesh {
    /// Four vertices per quad, part-local.
    pub vertices: Vec<ModelVertex>,
    /// Six indices per quad.
    pub indices: Vec<u32>,
    /// One entry per wing, in `("left_wing", _), ("right_wing", _)` order —
    /// a list rather than a fixed pair for the same reason
    /// [`CapeMesh::parts`] is one: a bake that produced no quads yields an
    /// empty list rather than a range into nothing.
    pub parts: Vec<(ElytraWing, PartRange)>,
}

/// Which wing a [`ElytraMesh::parts`] range belongs to.
///
/// A named side rather than a `&'static str`, because the side is not just a
/// label here — it *selects* the sign of two of the three rotation terms in
/// [`elytra_wing_transform`], and a stringly-typed version invites the
/// silently-symmetric bug where both wings get the left one's matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElytraWing {
    /// `left_wing` — pivot `x = +5`, rotations used as given.
    Left,
    /// `right_wing` — pivot `x = -5`, Y and Z rotations negated.
    Right,
}

impl ElytraWing {
    /// The part name in [`lodestone_assets::entity::elytra_model`].
    #[must_use]
    pub const fn part_name(self) -> &'static str {
        match self {
            ElytraWing::Left => "left_wing",
            ElytraWing::Right => "right_wing",
        }
    }

    /// The wing pivot's X in **model texels** (vanilla's elytra model builder
    /// offsets each wing part by `±5, 0, 0`).
    #[must_use]
    pub const fn pivot_x(self) -> f32 {
        match self {
            ElytraWing::Left => 5.0,
            ElytraWing::Right => -5.0,
        }
    }
}

impl ElytraMesh {
    /// Bake the elytra mesh.
    #[must_use]
    pub fn load() -> Self {
        let def = lodestone_assets::entity::elytra_model();
        let baked = lodestone_assets::entity::bake_entity_parts(&def);
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        let mut parts = Vec::new();
        for wing in [ElytraWing::Left, ElytraWing::Right] {
            let Some(part) = baked
                .iter()
                .find(|p| p.name.as_str() == wing.part_name() && !p.quads.is_empty())
            else {
                continue;
            };
            let index_start = indices.len() as u32;
            let vertex_start = vertices.len() as u32;
            push_part_quads(&part.quads, &mut vertices, &mut indices);
            parts.push((
                wing,
                PartRange {
                    index_start,
                    index_count: indices.len() as u32 - index_start,
                    vertex_start,
                    vertex_count: vertices.len() as u32 - vertex_start,
                },
            ));
        }
        ElytraMesh {
            vertices,
            indices,
            parts,
        }
    }

    /// Pair both wings with the wearer's `"body"` part, dropping them
    /// entirely for a non-humanoid rig — the same gate [`CapeMesh::attach`]
    /// and [`wearer_carries_armour`] use, and for the same reason: a rig with
    /// no `body` pivot has nothing for the wings to hang off, and attaching
    /// by part name alone would strap an elytra to a pig.
    pub fn attach<'a>(
        &'a self,
        wearer: &'a Skeleton,
    ) -> impl Iterator<Item = (ElytraWing, PartRange, usize)> + 'a {
        let humanoid = wearer_carries_armour(wearer);
        let body = wearer.index_of("body");
        self.parts
            .iter()
            .filter(move |_| humanoid)
            .filter_map(move |(wing, range)| body.map(|i| (*wing, *range, i)))
    }
}

/// Vanilla's elytra animation state's resting rotation triple
/// `(x_rot, y_rot, z_rot)` in radians — the target it lerps toward when the
/// wearer is neither fall-flying nor crouching, `(PI/12, 0, -PI/12)`.
///
/// This is also the elytra model's authored rest pose, which is why it is
/// what a standing player's wings look like. A caller that keeps no animation
/// state at all can pass this straight to [`elytra_wing_transform`] and get
/// the correct wings for every wearer who is standing, walking or running —
/// everything except a glide and a crouch.
#[must_use]
pub fn elytra_rest_rotations() -> (f32, f32, f32) {
    (
        std::f32::consts::PI / 12.0,
        0.0,
        -std::f32::consts::PI / 12.0,
    )
}

/// The per-tick *target* `(x_rot, y_rot, z_rot)` an elytra's animation state
/// lerps toward — vanilla's per-tick elytra-state update's three-way branch,
/// in radians.
///
/// `motion` is the wearer's delta movement in blocks per tick. Only its
/// **normalised Y** is read, and only when it is negative: a steeper dive
/// folds the wings back further, which is the whole visual point of the
/// gliding pose.
///
/// This is the pure half of vanilla's elytra animation state. The impure half
/// is two lerped triples (`rot*` and `rot*Old`) advanced once per game tick
/// by `current += (target - current) * ` [`ELYTRA_ROTATION_LERP`] and read
/// back interpolated by partial ticks — that state belongs wherever entity
/// ticks live, not here, exactly as `cape_sway`'s lagged cloak position does.
///
/// # Precedence
///
/// Fall-flying wins over crouching, not the other way round: vanilla checks
/// fall-flying first, and a player can be both.
#[must_use]
pub fn elytra_target_rotations(fall_flying: bool, crouching: bool, motion: Vec3) -> (f32, f32, f32) {
    use std::f32::consts::PI;
    if fall_flying {
        // `ratio = 1 - (-normalize(motion).y)^1.5` while descending, else 1.
        // Computed in f64 because vanilla's own vector type is
        // double-precision and its pow call operates in double too, and the
        // exponent is fractional, so the f32 round trip is not free.
        let ratio = if motion.y < 0.0 {
            let len = f64::from(motion.x).hypot(f64::from(motion.y)).hypot(f64::from(motion.z));
            // Vanilla's vector normalize returns ZERO for a zero-length
            // vector, whose `y` is 0 and so leaves `ratio` at 1 — matching
            // the guard rather than dividing by zero.
            let ny = if len < 1.0e-4 { 0.0 } else { f64::from(motion.y) / len };
            1.0 - (-ny).max(0.0).powf(1.5)
        } else {
            1.0
        };
        let ratio = ratio as f32;
        // Vanilla's lerp helper: `start + delta * (end - start)`.
        let lerp = |start: f32, end: f32| start + ratio * (end - start);
        (
            lerp(PI / 12.0, PI / 9.0),
            0.0,
            lerp(-PI / 12.0, -PI / 2.0),
        )
    } else if crouching {
        // Transcribed from the branch, not derived: the Y term is vanilla's
        // own float literal `0.08726646F` (5 degrees), and it is the only one
        // of the nine constants in this function that is not a fraction of PI.
        (PI * 2.0 / 9.0, 0.08726646, -PI / 4.0)
    } else {
        elytra_rest_rotations()
    }
}

/// The per-tick approach rate in vanilla's per-tick elytra-state update
/// (`rot += (target - rot) * 0.3`).
pub const ELYTRA_ROTATION_LERP: f32 = 0.3;

/// The wearer's crouching wing `y` offset in **model texels** — vanilla's
/// per-frame elytra animation step's `isCrouching ? 3.0F : 0.0F`, which it
/// assigns to *both* wings.
#[must_use]
pub const fn elytra_wing_y(crouching: bool) -> f32 {
    if crouching { 3.0 } else { 0.0 }
}

/// The per-frame placement of one wing, relative to the wearer's **body**
/// part transform.
///
/// `x_rot`/`y_rot`/`z_rot` are the *left* wing's angles in radians — the
/// triple [`elytra_target_rotations`] produces, after the caller's own
/// lerping. The right wing's negations are applied here rather than by the
/// caller so there is exactly one place that knows the sign convention.
///
/// # The composition
///
/// ```text
/// T(0, 0, 0.125) * T(pivot_x/16, y/16, 0) * Rz(z) * Ry(y) * Rx(x)
/// ```
///
/// * The leading translate is the elytra layer's own submit-step translate
///   of `(0, 0, 0.125)`, applied to the layer as a whole and therefore
///   **outside** the wing's own pivot. In blocks.
/// * `T(pivot_x/16, y/16, 0)` is the wing's pivot: `x` is authored and
///   constant (`±5` texels), `y` is assigned per frame by the per-frame
///   animation step and is `3` texels only while crouching. `z` is `0`.
/// * The rotation order is `Rz * Ry * Rx`, matching vanilla's own
///   translate-and-rotate part composition — not the `Rx * Rz * Ry` the cape
///   ends up with, which is a *composed* quaternion chain rather than a part
///   pose.
///
/// # The right wing
///
/// Vanilla's per-frame animation step gives it `rot_y = -left.rot_y` and
/// `rot_z = -left.rot_z`, and
/// leaves `rot_x` and `y` shared. Two of three negated, and it is the two that
/// are *not* negated that make a "just mirror everything" version wrong: a
/// mirrored `rot_x` pitches one wing up and the other down.
#[must_use]
pub fn elytra_wing_transform(
    wing: ElytraWing,
    x_rot: f32,
    y_rot: f32,
    z_rot: f32,
    crouching: bool,
) -> Mat4 {
    let (y_rot, z_rot) = match wing {
        ElytraWing::Left => (y_rot, z_rot),
        ElytraWing::Right => (-y_rot, -z_rot),
    };
    let layer = Mat4::from_translation(Vec3::new(0.0, 0.0, 0.125));
    let pivot = Mat4::from_translation(Vec3::new(
        wing.pivot_x() / 16.0,
        elytra_wing_y(crouching) / 16.0,
        0.0,
    ));
    let rotate =
        Mat4::from_rotation_z(z_rot) * Mat4::from_rotation_y(y_rot) * Mat4::from_rotation_x(x_rot);
    layer * pivot * rotate
}

/// The texture layers to draw for an item sitting in `slot`, in draw order —
/// empty when this item is not humanoid armour, or is armour for a *different*
/// slot, or its material declares no layers for this slot's layer type.
///
/// The slot equality check is vanilla's own armour-layer render gate: a
/// helmet's declared slot must equal the slot it is worn in, so a plugin can
/// put a helmet in the boots slot, and vanilla draws nothing rather than
/// drawing a helmet around the ankles.
#[must_use]
pub fn armour_layers(slot: ArmourSlot, item_path: &str) -> &'static [ArmourLayer] {
    match armour_item(item_path) {
        Some((item_slot, asset)) if item_slot == slot => asset.layers(slot.layer_type()),
        _ => &[],
    }
}

/// The gamma-space RGB a layer multiplies its texel by: vanilla's
/// dyeable-layer "colour when undyed" for a dyeable layer, white for any
/// other.
///
/// This is [`armour_layer_tint_with_dye`] with the stack's own
/// `minecraft:dyed_color` **absent** — kept as a zero-argument convenience
/// because every call site today (`gpu.rs::prepare_armour`) has no dye value
/// to hand it: the wire component is dropped at the shell's
/// `entity_snapshot` boundary. See `docs/armour-rendering.md` for the wiring
/// that would change that.
#[must_use]
pub fn armour_layer_tint(layer: &ArmourLayer) -> [u8; 3] {
    armour_layer_tint_with_dye(layer, None)
}

/// The gamma-space RGB a layer multiplies its texel by, given the wearer
/// stack's own `minecraft:dyed_color` component if it decoded one.
///
/// This is vanilla's own per-layer colour resolution, transcribed exactly:
///
/// ```text
/// fn color_for_layer(layer, dye_color) -> color {
///    if let Some(dyeable) = layer.dyeable() {
///       let color_when_undyed = dyeable.color_when_undyed().map(opaque).unwrap_or(0);
///       if dye_color != 0 { dye_color } else { color_when_undyed }
///    } else {
///       WHITE // no tint
///    }
/// }
/// ```
///
/// where `dye_color` is the stack's dyed-color component, opacity-forced, if
/// present, or `0` (not `color_when_undyed` — that fallback lives here, one
/// call up) if it does not carry one.
///
/// A non-dyeable layer (`layer.dye` is [`None`]) ignores `dyed_color`
/// entirely and returns white (opaque white, i.e. "no tint") — matching the
/// non-dyeable branch above, which never reads `dye_color`.
///
/// **A leather piece dyed pure black (`0x000000`) is indistinguishable from
/// an undyed one**, and this is vanilla's own behaviour, not a port bug:
/// forcing opacity only touches the alpha byte, so a `0x000000` dye still
/// reads as `dye_color == 0` and the `dye_color != 0 ? dye_color :
/// color_when_undyed` fallback falls through to
/// [`UNDYED_LEATHER_RGB`](lodestone_assets::equipment::UNDYED_LEATHER_RGB)
/// exactly as if the component were absent. `dyed_color_zero_reads_as_undyed`
/// pins this so a future "fix" that special-cases black does not quietly
/// diverge from the game it is porting.
#[must_use]
pub fn armour_layer_tint_with_dye(layer: &ArmourLayer, dyed_color: Option<u32>) -> [u8; 3] {
    let Some(undyed) = layer.dye else {
        // The layer is not dyeable: vanilla returns opaque white
        // unconditionally, never consulting the dye colour.
        return [255, 255, 255];
    };
    // Vanilla's dyed-color read forces only the alpha byte, so the low 24
    // bits of `dyed_color` are already the RGB vanilla reads. `0` (component
    // absent, or present-but-black) falls through to the undyed colour.
    match dyed_color.filter(|&rgb| rgb & 0x00FF_FFFF != 0) {
        Some(rgb) => [(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8],
        None => undyed,
    }
}

// ---------------------------------------------------------------------------
// Dropped items
// ---------------------------------------------------------------------------
//
// A dropped item is an entity that is **not** a cuboid part rig, so none of the
// machinery above applies to it: it has no skeleton, no per-mob sheet, and no
// `entity_models` corpus entry. What it has is an *item model* — the same baked
// geometry [`BlockModels::item_forms`](crate::BlockModels::item_forms) already
// supplies for a hotbar icon — drawn in the world through the ordinary
// [`ModelPipeline`](crate::ModelPipeline) rather than the entity pipeline.
//
// This section owns the *placement*: where in the world that geometry goes, and
// how it bobs and spins. Transcribed from the 26.2 client's dropped-item
// submit step:
//
// ```text
//   box          = the item's ground-posed model bounding box
//   minOffsetY   = -box.minY + 0.0625
//   bob          = sin(ageInTicks / 10 + bobOffs) * 0.1 + 0.1
//   translate(0, bob + minOffsetY, 0)
//   rotateY(getSpin(ageInTicks, bobOffs))   // radians
//   // then the item is drawn under its display.ground transform
// ```
//
// and `getSpin(age, bobOffs) = age / 20 + bobOffs`.
//
// # The winding invariant, stated for a *world* pose
//
// The GUI item path composes `gui_ortho * gui_item_pose`, and each of those two
// matrices has a negative determinant so that the **product**'s determinant sign
// matches [`Camera::view_projection`](crate::Camera::view_projection)'s — which
// is itself negative, because `glam`'s DirectX right-handed perspective is.
// That is a statement about the *composed* matrix, and it does not transfer to
// this path.
//
// Here the pose is a **world-space model matrix** left-multiplied by the very
// same `Camera::view_projection`, exactly like a terrain section's. So the pose
// must not flip anything: its determinant has to be **positive**, and the
// composed `view_projection * pose` then inherits the camera's negative sign.
// Reading the GUI rule as "the pose determinant must be negative" and coding to
// it would ship an item you are looking at the *inside* of — which spins
// convincingly in a screenshot. `dropped_item_pose_preserves_winding` derives
// the reference sign from the camera rather than hardcoding either answer.
