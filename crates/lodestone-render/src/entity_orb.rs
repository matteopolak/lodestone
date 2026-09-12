use super::*;

/// Texture path for the standalone experience-orb sprite sheet.
pub const EXPERIENCE_ORB_TEXTURE: &str =
    "assets/minecraft/textures/entity/experience/experience_orb.png";

/// Where the entity ground-shadow sprite lives in the vanilla jar — a
/// standalone radial-gradient decal, not a slice of any atlas, matching
/// [`EXPERIENCE_ORB_TEXTURE`]'s own shape. `textures/misc/shadow.png`, from
/// vanilla's shadow-feature renderer's own hard-coded default-namespace texture
/// location.
pub const SHADOW_TEXTURE: &str = "assets/minecraft/textures/misc/shadow.png";

/// How many distinct sprite cells [`experience_orb_icon`] can return, i.e. the
/// number of baked orb quads a renderer needs.
pub const EXPERIENCE_ORB_ICON_COUNT: u32 = 11;

/// The 16-pixel sprite cell edge inside the 64-pixel sheet, so four cells per row.
pub(crate) const ORB_CELL: f32 = 16.0;
/// The sheet edge both cell axes are normalised against.
pub(crate) const ORB_SHEET: f32 = 64.0;
/// Cells per row — `icon % 4` picks the column, `icon / 4` the row.
pub(crate) const ORB_CELLS_PER_ROW: u32 = 4;

/// Vanilla's experience-orb icon accessor — which of the eleven sprite cells an orb
/// worth `value` draws.
///
/// # It is a bucketed ladder, not a linear map, and the buckets are the *orb
/// denominations*
///
/// The thresholds are `2477, 1237, 617, 307, 149, 73, 37, 17, 7, 3` — the same
/// irregular, roughly-doubling ladder vanilla's experience-award splitting function splits a payout over,
/// read top-down with `>=`. So the cell is constant *across* a bucket: an orb worth
/// 7, one worth 8 and one worth 16 all draw cell 2, and 17 is the first value that
/// draws cell 3. Any gate that observes only one value, or two values inside one
/// bucket, cannot tell this function from a linear `value / 250` — the pairs that
/// discriminate are the ones straddling a threshold.
///
/// Values below 3 (including `0`, which is what an orb whose `DATA_VALUE` never
/// reached us reads as) draw cell 0. A negative value cannot occur on the wire but
/// falls into the same arm rather than panicking or wrapping.
#[must_use]
pub fn experience_orb_icon(value: i32) -> u32 {
    // Written as the same descending `>=` ladder vanilla uses rather than as a
    // `match` on ranges: a range table has to restate every threshold twice
    // (as one arm's end and the next arm's start) and an off-by-one there is
    // invisible except at exactly the boundary value.
    if value >= 2477 {
        10
    } else if value >= 1237 {
        9
    } else if value >= 617 {
        8
    } else if value >= 307 {
        7
    } else if value >= 149 {
        6
    } else if value >= 73 {
        5
    } else if value >= 37 {
        4
    } else if value >= 17 {
        3
    } else if value >= 7 {
        2
    } else if value >= 3 {
        1
    } else {
        0
    }
}

/// The four `(u, v)` corners of one orb sprite cell, in the bottom-left,
/// bottom-right, top-right, top-left order [`experience_orb_mesh`] winds.
///
/// Vanilla's experience-orb renderer submit function's own arithmetic: `u0 = (icon % 4 * 16) / 64`,
/// `v0 = (icon / 4 * 16) / 64`, each `+16` for the far edge — and note vanilla
/// pairs the quad's **bottom** vertices with `v1` (the cell's larger v) and its top
/// with `v0`, so the sprite is not flipped. Getting that pair the other way round
/// draws an upside-down orb, which is invisible on a radially symmetric cell and
/// visible on the higher-value ones.
pub(crate) fn experience_orb_cell_uvs(icon: u32) -> [[f32; 2]; 4] {
    let column = (icon % ORB_CELLS_PER_ROW) as f32;
    let row = (icon / ORB_CELLS_PER_ROW) as f32;
    let u0 = column * ORB_CELL / ORB_SHEET;
    let u1 = (column * ORB_CELL + ORB_CELL) / ORB_SHEET;
    let v0 = row * ORB_CELL / ORB_SHEET;
    let v1 = (row * ORB_CELL + ORB_CELL) / ORB_SHEET;
    [[u0, v1], [u1, v1], [u1, v0], [u0, v0]]
}

/// One orb's quad in *local* space, for the sprite cell `icon`, ready to be posed
/// by [`experience_orb_matrix`].
///
/// The corners are vanilla's literally: `x ∈ [-0.5, 0.5]`, `y ∈ [-0.25, 0.75]`,
/// `z = 0`. The y range is **not** centred on zero — vanilla's four `vertex` calls
/// are `(-0.5, -0.25)`, `(0.5, -0.25)`, `(0.5, 0.75)`, `(-0.5, 0.75)` — so the
/// quad sits three-quarters above its own origin and, after the `0.3` scale and the
/// `+0.1` lift in [`experience_orb_matrix`], spans `y ∈ [0.025, 0.325]` above the
/// orb's feet. Centring it would sink half the sprite into the floor.
///
/// The vertex `light`/`tint`/`anim` lanes are inert defaults exactly as
/// [`crate::entity_pipeline::flame_mesh`]'s are: the orb pass carries its light and
/// its colour **per instance**, because both change per orb and per tick.
#[must_use]
pub fn experience_orb_mesh(icon: u32) -> (Vec<ModelVertex>, Vec<u32>) {
    const CORNERS: [[f32; 2]; 4] = [[-0.5, -0.25], [0.5, -0.25], [0.5, 0.75], [-0.5, 0.75]];
    let uvs = experience_orb_cell_uvs(icon);
    let vertices = CORNERS
        .iter()
        .zip(uvs)
        .map(|([x, y], uv)| ModelVertex {
            position: [*x, *y, 0.0],
            uv,
            ao: 1.0,
            light: 0,
            tint: 255,
            anim: 0,
            cutout_bypass: 0,
            tint_rgb_override: [0, 0, 0, 0],
        })
        .collect();
    // The same two-triangle winding every other baked quad in this crate uses.
    (vertices, vec![0, 1, 2, 0, 2, 3])
}

/// The world placement for one orb, matching vanilla's experience-orb renderer
/// submit function's
/// pose-stack order:
///
/// ```text
/// T(feet) · T(0, 0.1, 0) · camera_orientation · S(0.3)
/// ```
///
/// `orientation` is [`camera_orientation`]`(camera.view_matrix())` — the same one
/// matrix every orb this frame shares, since a billboard's rotation depends only
/// on the camera. The `0.1` lift is applied in **world** space, before the
/// orientation, so it is straight up whatever way the camera is looking; folding it
/// into the local quad instead would tilt it with the view.
///
/// Determinant is positive (a translation, a rotation and a positive uniform
/// scale), so this composes to terrain's winding — and `EntityPipeline` is
/// `cull_mode: None` regardless, so a sign error here would show as wrong depth
/// order rather than as a vanished quad.
#[must_use]
pub fn experience_orb_matrix(feet: Vec3, orientation: Mat4) -> Mat4 {
    /// `scale(0.3F, 0.3F, 0.3F)`.
    const ORB_SCALE: f32 = 0.3;
    /// `translate(0.0F, 0.1F, 0.0F)`.
    const ORB_LIFT: f32 = 0.1;
    Mat4::from_translation(feet + Vec3::new(0.0, ORB_LIFT, 0.0))
        * orientation
        * Mat4::from_scale(Vec3::splat(ORB_SCALE))
}

/// Vanilla's pulsing orb colour, as the gamma-space `[r, g, b]` bytes an
/// `InstanceTint` carries.
///
/// Vanilla's experience-orb renderer submit function, verbatim, with `rr = ageInTicks / 2`:
///
/// ```text
/// r = (sin(rr) + 1) * 0.5 * 255
/// g = 255
/// b = (sin(rr + 4π/3) + 1) * 0.1 * 255
/// ```
///
/// The two amplitudes differ — `0.5` for red, `0.1` for blue — and the phase
/// offset is `4π/3`, not `2π/3`, so the orb cycles green→yellow→green rather than
/// through a full hue wheel. Green is pinned at full and never modulates.
///
/// These are **gamma-space** bytes multiplied into a gamma-encoded texel, which is
/// where `entity.wgsl` applies an `InstanceTint`; vanilla is not colour-managed and
/// converting them to linear first would pull the whole cycle toward white.
#[must_use]
pub fn experience_orb_tint(age_ticks: f32) -> [u8; 3] {
    let phase = age_ticks / 2.0;
    let channel = |value: f32| -> u8 {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped into 0..=255 first, and vanilla truncates too"
        )]
        {
            (value.clamp(0.0, 255.0)) as u8
        }
    };
    let red = (phase.sin() + 1.0) * 0.5 * 255.0;
    let blue = ((phase + std::f32::consts::PI * 4.0 / 3.0).sin() + 1.0) * 0.1 * 255.0;
    [channel(red), 255, channel(blue)]
}

/// An orb's packed light, from the sample at its own position.
///
/// Vanilla's experience-orb light-level accessor is
/// `clamp(super.getBlockLightLevel(..) + 7, 0, 15)` — a **+7 boost to the block
/// nibble only**, which is what keeps an orb readable on a cave floor. The sky
/// nibble is passed through untouched; boosting both would make an orb in a lit
/// room brighter than the room.
#[must_use]
pub fn experience_orb_light(packed: u8) -> u8 {
    let sky = packed & 0xF0;
    let block = ((packed & 0x0F) + 7).min(15);
    sky | block
}

// ---------------------------------------------------------------------------
// Held items, and the first-person arm
// ---------------------------------------------------------------------------
//
// Both are *item/part geometry hung off an arm*, and both are transcribed from
// the 26.2 client rather than tuned by eye. The two chains are deliberately kept
// separate (`held_item_matrix` vs `first_person_arm_pose`) because vanilla's are:
// one hangs off the third-person part hierarchy, the other replaces it entirely.
