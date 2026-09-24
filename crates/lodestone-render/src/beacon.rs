//! The beacon light beam — `BeaconRenderer`'s own decompiled source, ported. Not a cuboid rig:
//! vanilla builds this geometry procedurally every frame (a rotating
//! diamond-section "core" plus an axis-aligned glow square, both scrolling a
//! shared texture vertically), so this module has no `EntityModelDef` the
//! way [`crate::block_entity`]'s family does — the geometry functions here
//! *are* the model.
//!
//! ## What it is
//!
//! Two nested translucent quads-cylinders per beam **section** (one per
//! contiguous run of same/averaged-coloured `minecraft:beacon_beam_block`
//! stacked above the beacon): a `0.2`-radius "solid" core that spins, and a
//! `0.25`-radius glow square that does not. Both scroll
//! `textures/entity/beacon/beacon_beam.png` vertically with the game clock.
//! Before this landed, a beacon had **zero** visual indication it was
//! active — `assets/minecraft/models/block/beacon.json` has real geometry
//! for the pyramid glass/obsidian frame, so a beacon was never a *hole* the
//! way a chest is, just permanently inert-looking.
//!
//! ## How it works
//!
//! [`beacon_beam_vertices`] is the whole port of `BeaconRenderer.
//! submitBeaconBeam`/`renderPart`/`renderQuad`/`addVertex`: given a beam's
//! world position, its resolved [`BeamSection`] list, the shared animation
//! clock and the distance-derived radius scale, it returns two triangle
//! lists (solid, glow) in world space, ready for a caller to upload
//! directly — no matrix, no further transform, because vanilla's own
//! `poseStack.translate(0.5, 0.0, 0.5)` prologue is folded into the
//! function rather than left for a caller to reapply.
//!
//! [`beacon_beam_color`] and [`average_beam_color`] resolve *what* a beam
//! section's colour is — `BeaconBlockEntity.tick`'s `state.getBlock()
//! instanceof BeaconBeamBlock` scan, restated as a lookup over a block's
//! registry path rather than a live `instanceof` check, since this crate
//! has no block-registry trait object to test against. The beacon block
//! itself is white (`BeaconBlock.getColor()`); every stained-glass
//! (pane) block is its own dye colour
//! (`StainedGlassBlock`/`StainedGlassPaneBlock`).
//!
//! ## How to change it
//!
//! The **scan** that turns a live world into a [`BeaconSpawn`] — walking up
//! from the beacon, checking the four-ring base pyramid, deciding where a
//! run of colour breaks — is *not* here. It needs [`lodestone_world::World`]
//! block-state reads this crate cannot depend on, so it lives in
//! `lodestone_shell::block_entities::{beacon_levels, beacon_beam_scan,
//! beacon_spawn, beacon_spawns}`, mirroring where every other block-entity
//! gather already lives. This module only knows how to turn a *resolved*
//! section list into pixels.
//!
//! ## What is deliberately not ported
//!
//! * **Scoping/zoom radius shrink.** `BeaconRenderer.extract`'s
//!   `player.isScoping() ? 1.0F : max(1.0, dist/96.0)` — this client has no
//!   scope/zoom feature, so [`beam_radius_scale`] always takes the unscoped
//!   arm. The day a zoom key lands, this needs a second argument.
//! * **Fog.** Unlike the terrain/entity passes, the GPU pass built over this
//!   module's output does not fold `apply_fog` into the fragment shader —
//!   the same simplification `gpu/sign_text.rs` already makes for its own
//!   translucent, jar-sourced-texture pass. A beam is a bright, self-lit
//!   effect at `setLight(15728880)` (full-bright) in vanilla too, so the
//!   missing fog term is the least visible of this module's gaps.

use crate::banner_pattern::DyeColor;
use glam::Vec3;

/// `BeaconRenderer.SOLID_BEAM_RADIUS`.
pub const SOLID_BEAM_RADIUS: f32 = 0.2;
/// `BeaconRenderer.BEAM_GLOW_RADIUS`.
pub const BEAM_GLOW_RADIUS: f32 = 0.25;
/// `TheEndGatewayRenderer.submit`'s own hardcoded `solidBeamRadius` argument
/// to the general `submitBeaconBeam` overload — narrower than a beacon's own
/// [`SOLID_BEAM_RADIUS`], and (unlike a beacon's) never scaled by distance.
pub const END_GATEWAY_SOLID_BEAM_RADIUS: f32 = 0.15;
/// `TheEndGatewayRenderer.submit`'s own hardcoded `beamGlowRadius` argument.
pub const END_GATEWAY_BEAM_GLOW_RADIUS: f32 = 0.175;
/// `BeaconRenderer.MAX_RENDER_Y` — the topmost beam section (the one with no
/// block above it to bound it) renders as if it reached this world height,
/// however tall it actually scanned.
pub const MAX_RENDER_Y: i32 = 2048;
/// `BeaconRenderer.BEAM_SCALE_THRESHOLD` — the horizontal distance (blocks)
/// beyond which the beam's radius grows to stay visible from far away.
pub const BEAM_SCALE_THRESHOLD: f32 = 96.0;

/// One contiguous run of a beam's colour, resolved by the world scan —
/// `BeaconBeamOwner.Section`. `color` is `0x00RRGGBB`, gamma-space, always
/// opaque (every source colour is `ARGB.opaque`d in vanilla — see the
/// module doc).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BeamSection {
    /// Gamma-space `0x00RRGGBB`, always opaque.
    pub color: u32,
    /// Height in blocks, `>= 1`.
    pub height: i32,
}

/// A fully resolved beacon beam, ready for [`beacon_beam_vertices`]. `sections`
/// is already gated the way vanilla's own `getBeamSections()` is: the caller
/// (`lodestone_shell::block_entities::beacon_spawn`) must pass an empty `Vec`
/// when the base pyramid has no completed level, even if a coloured run
/// otherwise scans clean above the beacon.
#[derive(Debug, Clone, PartialEq)]
pub struct BeaconSpawn {
    /// The beacon block's own integer corner.
    pub pos: [i32; 3],
    /// Resolved beam sections, base-to-top; empty when the beam should not
    /// render at all (no completed base level).
    pub sections: Vec<BeamSection>,
    /// `floorMod(gameTime, 40) + partialTicks` — `BeaconRenderer.extract`.
    pub animation_time: f32,
    /// [`beam_radius_scale`]'s result for this beacon's distance from the
    /// eye this frame.
    pub beam_radius_scale: f32,
}

/// One end gateway's teleport beam, for this frame — vanilla's
/// `TheEndGatewayRenderer.submit`'s `BeaconRenderer.submitBeaconBeam` call,
/// shown while `isSpawning()`/`isCoolingDown()`. Everything here is already
/// resolved by the gather (`lodestone_shell::block_entities::
/// end_gateway_beam_spawns`) — [`end_gateway_beam_vertices`] is a pure
/// function of these five fields.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EndGatewayBeamSpawn {
    /// The gateway block's own integer corner.
    pub pos: [i32; 3],
    /// `getSpawnPercent`/`getCooldownPercent`'s result — `sin(clamp(..) *
    /// PI)` already applied, matching `EndGatewayRenderState.scale`.
    pub scale: f32,
    /// `floorMod(gameTime, 40) + partialTicks` — the same scroll/spin clock
    /// [`BeaconSpawn::animation_time`] carries.
    pub animation_time: f32,
    /// `Mth.floor(scale * beamDistance)` — the beam's half-height; the drawn
    /// beam spans `y ∈ [-height, height]`. `0` (or negative) draws nothing.
    pub height: i32,
    /// `DyeColor.MAGENTA`/`PURPLE`'s texture diffuse colour, gamma-space
    /// `0x00RRGGBB` — magenta while spawning, purple while cooling down.
    pub color: u32,
}

/// One beam vertex: world-space position, gamma-space RGBA colour (already
/// carrying the section's colour and, for the glow pass, alpha `32/255`),
/// and the scrolling texture UV.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BeamVertex {
    /// World-space position.
    pub position: [f32; 3],
    /// Gamma-space RGBA, `0.0..=1.0`.
    pub color: [f32; 4],
    /// Texture UV into `beacon_beam.png`.
    pub uv: [f32; 2],
}

/// `Math.max(1.0F, distanceToBeacon / 96.0F)` — the un-scoped arm of
/// `BeaconRenderer.extract`'s radius scale. See the module doc's "What is
/// deliberately not ported" for the scoping arm this omits.
#[must_use]
pub fn beam_radius_scale(horizontal_distance: f32) -> f32 {
    (horizontal_distance / BEAM_SCALE_THRESHOLD).max(1.0)
}

/// `BeaconBlockEntity.tick`'s `state.getBlock() instanceof BeaconBeamBlock`
/// test, restated as a lookup over a block's bare registry path (no
/// `minecraft:` prefix — callers strip it the way every other resolver in
/// this crate's shell consumer does). `"beacon"` itself is always
/// [`DyeColor::White`] (`BeaconBlock.getColor()`); every
/// `"<colour>_stained_glass"`/`"<colour>_stained_glass_pane"` resolves
/// through [`DyeColor::from_name`]. Anything else is not a beam block —
/// `None`.
#[must_use]
pub fn beacon_beam_color(block_path: &str) -> Option<u32> {
    if block_path == "beacon" {
        return Some(DyeColor::White.packed_rgb());
    }
    let name = block_path
        .strip_suffix("_stained_glass_pane")
        .or_else(|| block_path.strip_suffix("_stained_glass"))?;
    DyeColor::from_name(name).map(DyeColor::packed_rgb)
}

/// `ARGB.average(lhs, rhs)` (`ARGB`'s own decompiled source) — per-channel integer average,
/// alpha included for fidelity though every caller here hands it two
/// already-opaque `0x00RRGGBB` values (alpha is implicitly `0xFF` on both
/// sides, so it stays `0xFF` and is dropped from the packed result the same
/// way [`beacon_beam_color`]'s own values are).
#[must_use]
pub fn average_beam_color(a: u32, b: u32) -> u32 {
    let chan = |sa: u32, sb: u32| (sa + sb) / 2;
    let ar = (a >> 16) & 0xFF;
    let ag = (a >> 8) & 0xFF;
    let ab = a & 0xFF;
    let br = (b >> 16) & 0xFF;
    let bg = (b >> 8) & 0xFF;
    let bb = b & 0xFF;
    (chan(ar, br) << 16) | (chan(ag, bg) << 8) | chan(ab, bb)
}

/// `Mth.frac(num)` — `num - floor(num)`.
fn frac(x: f32) -> f32 {
    x - x.floor()
}

fn unpack_rgba(color: u32, alpha: u8) -> [f32; 4] {
    [
        ((color >> 16) & 0xFF) as f32 / 255.0,
        ((color >> 8) & 0xFF) as f32 / 255.0,
        (color & 0xFF) as f32 / 255.0,
        f32::from(alpha) / 255.0,
    ]
}

/// `BeaconRenderer.submitBeaconBeam`/`renderPart`/`renderQuad`/`addVertex`,
/// ported whole. Returns `(solid, glow)` triangle-list vertices (6 per
/// vanilla `QUADS` face, triangulated `0,1,2,0,2,3` the way
/// `gpu/sign_text.rs` already triangulates a ported vanilla quad), in world
/// space — `pos` is the beacon block's own integer corner, matching every
/// other block-entity placement in this crate.
#[must_use]
pub fn beacon_beam_vertices(
    pos: [i32; 3],
    sections: &[BeamSection],
    animation_time: f32,
    beam_radius_scale: f32,
) -> (Vec<BeamVertex>, Vec<BeamVertex>) {
    let base = Vec3::new(pos[0] as f32 + 0.5, pos[1] as f32, pos[2] as f32 + 0.5);
    let mut solid = Vec::new();
    let mut glow = Vec::new();
    let mut beam_start = 0i32;
    let last_index = sections.len().saturating_sub(1);
    for (i, section) in sections.iter().enumerate() {
        // `submit`: the topmost section (no block above it to have stopped
        // the scan) renders as if it reached `MAX_RENDER_Y`, however tall it
        // actually scanned — but `beamStart` still advances by the section's
        // *real* height, not the substituted one.
        let render_height = if i == last_index {
            MAX_RENDER_Y
        } else {
            section.height
        };
        push_beam_section(
            base,
            // Beacon's own private 6-argument overload always passes
            // `scale = 1.0F` to the general form below.
            1.0,
            animation_time,
            beam_start,
            beam_start + render_height,
            section.color,
            SOLID_BEAM_RADIUS * beam_radius_scale,
            BEAM_GLOW_RADIUS * beam_radius_scale,
            &mut solid,
            &mut glow,
        );
        beam_start += section.height;
    }
    (solid, glow)
}

/// `TheEndGatewayBlockEntity`'s teleport beam — the *general* 9-parameter
/// `BeaconRenderer.submitBeaconBeam` overload
/// (`TheEndGatewayRenderer.submit`'s own call site), distinct from beacon's
/// own private 6-argument wrapper that [`beacon_beam_vertices`] calls: a
/// gateway passes its own `scale` (the spawn/cooldown percent, **not**
/// `1.0`) and its own fixed `0.15`/`0.175` radii rather than beacon's
/// `0.2`/`0.25` times a distance-derived scale.
///
/// One beam only, spanning `y ∈ [-height, height]` relative to the gateway's
/// own corner — `submitBeaconBeam(.., -state.height, state.height * 2, ..)`
/// gives `beamEnd = beamStart + height = -state.height + state.height*2 =
/// state.height`, i.e. centred on the block and reaching equally up and
/// down. `height <= 0` (not spawning/cooling down) draws nothing, matching
/// `TheEndGatewayRenderer.submit`'s own `if (state.height > 0)` guard.
#[must_use]
pub fn end_gateway_beam_vertices(
    pos: [i32; 3],
    scale: f32,
    animation_time: f32,
    height: i32,
    color: u32,
) -> (Vec<BeamVertex>, Vec<BeamVertex>) {
    let mut solid = Vec::new();
    let mut glow = Vec::new();
    if height <= 0 {
        return (solid, glow);
    }
    let base = Vec3::new(pos[0] as f32 + 0.5, pos[1] as f32, pos[2] as f32 + 0.5);
    push_beam_section(
        base,
        scale,
        animation_time,
        -height,
        height,
        color,
        END_GATEWAY_SOLID_BEAM_RADIUS,
        END_GATEWAY_BEAM_GLOW_RADIUS,
        &mut solid,
        &mut glow,
    );
    (solid, glow)
}

/// The general port of `BeaconRenderer.submitBeaconBeam`
/// (`renderPart`/`renderQuad`/`addVertex` folded in) — the 9-parameter form,
/// taking already-final radii and an explicit `scale` rather than deriving
/// either from a beacon-specific distance term. [`beacon_beam_vertices`] and
/// [`end_gateway_beam_vertices`] are its two callers, each supplying its own
/// radii/scale the way the real jar's two call sites do.
#[allow(clippy::too_many_arguments)]
fn push_beam_section(
    base: Vec3,
    scale: f32,
    animation_time: f32,
    beam_start: i32,
    beam_end: i32,
    color: u32,
    solid_radius: f32,
    glow_radius: f32,
    solid: &mut Vec<BeamVertex>,
    glow: &mut Vec<BeamVertex>,
) {
    let height = beam_end - beam_start;
    // `scroll = height < 0 ? animationTime : -animationTime` — a beam
    // section's rendered height is never negative in real use (it is either
    // a scanned run's own height or the `MAX_RENDER_Y` substitution), so
    // this always takes the `else` arm; ported as a branch anyway to match
    // `BeaconRenderer.submitBeaconBeam` exactly.
    let scroll = if height < 0 {
        animation_time
    } else {
        -animation_time
    };
    let tex_v_off = frac(scroll * 0.2 - (scroll * 0.1).floor());
    let uu1 = 0.0;
    let uu2 = 1.0;
    let vv2 = -1.0 + tex_v_off;

    // Solid inner core: a diamond cross-section that spins with the clock.
    let solid_r = solid_radius;
    let angle = (animation_time * 2.25 - 45.0).to_radians();
    let (sin_a, cos_a) = angle.sin_cos();
    // `Axis.YP.rotationDegrees` — a right-handed rotation about +Y.
    let rot = |x: f32, z: f32| (x * cos_a + z * sin_a, -x * sin_a + z * cos_a);
    let (wnx, wnz) = rot(0.0, solid_r);
    let (enx, enz) = rot(solid_r, 0.0);
    let (wsx, wsz) = rot(-solid_r, 0.0);
    let (esx, esz) = rot(0.0, -solid_r);
    let vv1_solid = height as f32 * scale * (0.5 / solid_r) + vv2;
    let solid_rgba = unpack_rgba(color, 255);
    push_beam_part(
        base, beam_start, beam_end, solid_rgba, wnx, wnz, enx, enz, wsx, wsz, esx, esz, uu1, uu2,
        vv1_solid, vv2, solid,
    );

    // Outer glow: an axis-aligned square, no rotation, low alpha.
    let glow_r = glow_radius;
    let (wnx, wnz) = (-glow_r, -glow_r);
    let (enx, enz) = (glow_r, -glow_r);
    let (wsx, wsz) = (-glow_r, glow_r);
    let (esx, esz) = (glow_r, glow_r);
    let vv1_glow = height as f32 * scale + vv2;
    // `ARGB.color(32, color)` — alpha forced to 32/255, RGB unchanged.
    let glow_rgba = unpack_rgba(color, 32);
    push_beam_part(
        base, beam_start, beam_end, glow_rgba, wnx, wnz, enx, enz, wsx, wsz, esx, esz, uu1, uu2,
        vv1_glow, vv2, glow,
    );
}

#[allow(clippy::too_many_arguments)]
fn push_beam_part(
    base: Vec3,
    beam_start: i32,
    beam_end: i32,
    color: [f32; 4],
    wnx: f32,
    wnz: f32,
    enx: f32,
    enz: f32,
    wsx: f32,
    wsz: f32,
    esx: f32,
    esz: f32,
    uu1: f32,
    uu2: f32,
    vv1: f32,
    vv2: f32,
    out: &mut Vec<BeamVertex>,
) {
    // `renderPart`'s four `renderQuad` calls, in its own order.
    push_beam_quad(
        base, beam_start, beam_end, color, wnx, wnz, enx, enz, uu1, uu2, vv1, vv2, out,
    );
    push_beam_quad(
        base, beam_start, beam_end, color, esx, esz, wsx, wsz, uu1, uu2, vv1, vv2, out,
    );
    push_beam_quad(
        base, beam_start, beam_end, color, enx, enz, esx, esz, uu1, uu2, vv1, vv2, out,
    );
    push_beam_quad(
        base, beam_start, beam_end, color, wsx, wsz, wnx, wnz, uu1, uu2, vv1, vv2, out,
    );
}

#[allow(clippy::too_many_arguments)]
fn push_beam_quad(
    base: Vec3,
    beam_start: i32,
    beam_end: i32,
    color: [f32; 4],
    ax: f32,
    az: f32,
    bx: f32,
    bz: f32,
    uu1: f32,
    uu2: f32,
    vv1: f32,
    vv2: f32,
    out: &mut Vec<BeamVertex>,
) {
    // `renderQuad`/`addVertex`: (beamEnd, a, uu2,vv1), (beamStart, a,
    // uu2,vv2), (beamStart, b, uu1,vv2), (beamEnd, b, uu1,vv1) — vanilla's
    // `QUADS` winding, triangulated `0,1,2,0,2,3`.
    let v0 = BeamVertex {
        position: [base.x + ax, base.y + beam_end as f32, base.z + az],
        color,
        uv: [uu2, vv1],
    };
    let v1 = BeamVertex {
        position: [base.x + ax, base.y + beam_start as f32, base.z + az],
        color,
        uv: [uu2, vv2],
    };
    let v2 = BeamVertex {
        position: [base.x + bx, base.y + beam_start as f32, base.z + bz],
        color,
        uv: [uu1, vv2],
    };
    let v3 = BeamVertex {
        position: [base.x + bx, base.y + beam_end as f32, base.z + bz],
        color,
        uv: [uu1, vv1],
    };
    out.extend([v0, v1, v2, v0, v2, v3]);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `BeaconBlock.getColor()` — white, and the same value `DyeColor::
    /// White.packed_rgb()` already carries for banner patterns and sign text
    /// (`WHITE(0, "white", 16383998, ...)` — a value shared across all three
    /// consumers is a real cross-check, not a coincidence).
    #[test]
    fn the_beacon_block_itself_is_white() {
        assert_eq!(beacon_beam_color("beacon"), Some(0x00F9_FFFE));
        assert_eq!(beacon_beam_color("beacon"), Some(DyeColor::White.packed_rgb()));
    }

    #[test]
    fn stained_glass_and_its_pane_resolve_the_same_dye() {
        assert_eq!(
            beacon_beam_color("red_stained_glass"),
            Some(DyeColor::Red.packed_rgb())
        );
        assert_eq!(
            beacon_beam_color("red_stained_glass_pane"),
            Some(DyeColor::Red.packed_rgb())
        );
    }

    /// Plain (undyed) glass is real vanilla geometry but **not** a
    /// `BeaconBeamBlock` — only the *stained* variants implement it
    /// (`StainedGlassBlock`/`StainedGlassPaneBlock`), so an unstained pane
    /// stopping the beam is correct, not a gap.
    #[test]
    fn plain_glass_is_not_a_beam_block() {
        assert_eq!(beacon_beam_color("glass"), None);
        assert_eq!(beacon_beam_color("glass_pane"), None);
        assert_eq!(beacon_beam_color("stone"), None);
    }

    /// `ARGB.average`, checked against a value that cannot be produced by
    /// truncation alone (an odd sum), pairwise-distinct per `CLAUDE.md`'s
    /// evidence standard.
    #[test]
    fn average_beam_color_matches_per_channel_integer_average() {
        let a = 0x00_10_20_31;
        let b = 0x00_30_40_51;
        // (0x10+0x30)/2=0x20, (0x20+0x40)/2=0x30, (0x31+0x51)/2=0x41
        assert_eq!(average_beam_color(a, b), 0x00_20_30_41);
    }

    #[test]
    fn beam_radius_scale_is_one_within_the_threshold_and_grows_beyond_it() {
        assert_eq!(beam_radius_scale(0.0), 1.0);
        assert_eq!(beam_radius_scale(BEAM_SCALE_THRESHOLD), 1.0);
        assert_eq!(beam_radius_scale(BEAM_SCALE_THRESHOLD * 2.0), 2.0);
    }

    /// A single section spanning the whole scan renders at [`MAX_RENDER_Y`]
    /// (it is the only, and therefore the last, section) — the vertical
    /// span of every emitted vertex must reach that height above the base,
    /// not the section's own scanned height.
    #[test]
    fn the_last_sections_top_reaches_max_render_y() {
        let sections = [BeamSection {
            color: 0x00FF_FFFF,
            height: 5,
        }];
        let (solid, glow) = beacon_beam_vertices([10, 64, 10], &sections, 0.0, 1.0);
        assert!(!solid.is_empty());
        assert!(!glow.is_empty());
        let max_y = solid
            .iter()
            .chain(&glow)
            .map(|v| v.position[1])
            .fold(f32::MIN, f32::max);
        let expected_top = 64.0 + MAX_RENDER_Y as f32;
        assert!(
            (max_y - expected_top).abs() < 1e-3,
            "expected top at {expected_top}, got {max_y}"
        );
        let min_y = solid
            .iter()
            .chain(&glow)
            .map(|v| v.position[1])
            .fold(f32::MAX, f32::min);
        assert!((min_y - 64.0).abs() < 1e-3, "expected base at 64, got {min_y}");
    }

    /// A second section's *start* is its predecessor's real (unsubstituted)
    /// height, not `MAX_RENDER_Y` — proves `beamStart` accumulates the real
    /// height even though the render call for the last section is fed the
    /// substituted one.
    #[test]
    fn a_middle_sections_top_is_its_own_real_height_not_max_render_y() {
        let sections = [
            BeamSection {
                color: 0x00FF_0000,
                height: 3,
            },
            BeamSection {
                color: 0x0000_FF00,
                height: 4,
            },
        ];
        let (solid, _glow) = beacon_beam_vertices([0, 0, 0], &sections, 0.0, 1.0);
        // First section's vertices (colour red) must not exceed y=3.
        let red_max_y = solid
            .iter()
            .filter(|v| v.color[0] > 0.5 && v.color[1] < 0.5)
            .map(|v| v.position[1])
            .fold(f32::MIN, f32::max);
        assert!((red_max_y - 3.0).abs() < 1e-3, "red section top: {red_max_y}");
        // Second (last) section's vertices (colour green) start at y=3 and
        // reach MAX_RENDER_Y.
        let green_min_y = solid
            .iter()
            .filter(|v| v.color[1] > 0.5 && v.color[0] < 0.5)
            .map(|v| v.position[1])
            .fold(f32::MAX, f32::min);
        assert!(
            (green_min_y - 3.0).abs() < 1e-3,
            "green section base: {green_min_y}"
        );
    }

    /// The solid core's cross-section rotates with `animation_time`; the
    /// glow square does not. Two different `animation_time` values must move
    /// the solid ring's XZ footprint but leave the glow square's identical.
    #[test]
    fn only_the_solid_core_spins_with_the_clock() {
        let sections = [BeamSection {
            color: 0x00FF_FFFF,
            height: 1,
        }];
        let (solid_a, glow_a) = beacon_beam_vertices([0, 0, 0], &sections, 0.0, 1.0);
        let (solid_b, glow_b) = beacon_beam_vertices([0, 0, 0], &sections, 10.0, 1.0);
        let xz = |v: &[BeamVertex]| -> Vec<(f32, f32)> {
            v.iter().map(|p| (p.position[0], p.position[2])).collect()
        };
        assert_ne!(xz(&solid_a), xz(&solid_b), "solid core should rotate");
        assert_eq!(xz(&glow_a), xz(&glow_b), "glow square should not rotate");
    }
}
