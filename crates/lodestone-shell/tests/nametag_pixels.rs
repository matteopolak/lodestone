//! Pixel gate: a named entity draws real glyph pixels above its head — driven
//! through the real [`RenderState::render`] path, the same call `app.rs`'s
//! frame loop makes (issue #100). Per `CLAUDE.md`'s dominant defect class — a
//! subsystem built, tested, and reaching zero pixels because nothing calls it
//! — this exercises `gpu/nametag.rs` exactly as `render_inner` does, not a
//! reimplementation.
//!
//! Two gates:
//!
//! 1. [`a_named_entity_draws_text_pixels_above_it`] — the base case: subject
//!    (a name tag set) against a control that is byte-identical except
//!    `EntityDraw::name_tag: None` (the render-side shape of "not visible" —
//!    both "no custom name" and "`CUSTOM_NAME_VISIBLE` false" collapse to
//!    this by the time a snapshot reaches [`EntityDraw`], so this control
//!    covers both). The delta's *location* is checked against an
//!    analytically-projected anchor point, derived from the real
//!    `lodestone_data::entity_dimensions` census — not a hardcoded
//!    constant — per `CLAUDE.md`'s "derive the rect from the same
//!    expression the draw uses".
//! 2. [`occlusion`] — the depth-pass split: a giant, close occluder entity
//!    between the camera and a distant tagged entity's tag, real depth
//!    tested and written by the ordinary entity pass exactly as terrain
//!    would be. A standing (non-sneaking) tag must still contribute *some*
//!    pixels behind it (the see-through pass, which uses no depth
//!    attachment at all); a sneaking tag — `NameTag::see_through: false` —
//!    must contribute none, which is also the **control that proves the
//!    occluder genuinely occludes** (CLAUDE.md: "assertions of an absence
//!    need a control proving the detector works" — here, that the occluder
//!    actually blocks the depth-tested normal pass).
//!
//! Fail-closed: no GPU adapter or no `client.jar` is a failure, never a
//! silent skip.
//!
//! ```text
//! cargo test -p lodestone-shell --test nametag_pixels -- --ignored --nocapture
//! ```

use lodestone::entities::{EntityDraw, NameTag};
use lodestone::gpu::{RenderState, SKY_COLOR};
use lodestone_render::{AnimInput, Camera, GpuContext, HeadlessTarget, RenderTarget};

const W: u32 = 320;
const H: u32 = 240;

/// See `armour_pixels.rs`'s identically-named helper for why this conversion
/// is worth its own doc rather than a bare literal.
fn sky_bytes() -> [u8; 3] {
    SKY_COLOR.map(|c| (c * 255.0).round() as u8)
}

fn non_sky_count(pixels: &[u8], sky: [u8; 3]) -> usize {
    pixels
        .chunks_exact(4)
        .filter(|px| {
            let d = (i32::from(px[0]) - i32::from(sky[0])).abs()
                + (i32::from(px[1]) - i32::from(sky[1])).abs()
                + (i32::from(px[2]) - i32::from(sky[2])).abs();
            d > 60
        })
        .count()
}

/// The bounding box (in pixels) of every texel that differs between `a` and
/// `b` by more than a per-channel-sum threshold of `8` — the same threshold
/// `live_dropped_item.rs` uses for the same purpose. Printed on failure so a
/// wrong location reads as "where", not just "how many" (`CLAUDE.md`).
fn diff_bbox(a: &[u8], b: &[u8], w: u32) -> Option<(u32, u32, u32, u32)> {
    let mut bbox: Option<(u32, u32, u32, u32)> = None;
    for (i, (pa, pb)) in a.chunks_exact(4).zip(b.chunks_exact(4)).enumerate() {
        let d = (i32::from(pa[0]) - i32::from(pb[0])).abs()
            + (i32::from(pa[1]) - i32::from(pb[1])).abs()
            + (i32::from(pa[2]) - i32::from(pb[2])).abs();
        if d <= 8 {
            continue;
        }
        let x = i as u32 % w;
        let y = i as u32 / w;
        bbox = Some(match bbox {
            None => (x, x, y, y),
            Some((lo_x, hi_x, lo_y, hi_y)) => (lo_x.min(x), hi_x.max(x), lo_y.min(y), hi_y.max(y)),
        });
    }
    bbox
}

fn project(view_proj: glam::Mat4, world: glam::Vec3, w: u32, h: u32) -> (f32, f32) {
    let clip = view_proj * glam::Vec4::new(world.x, world.y, world.z, 1.0);
    let ndc_x = clip.x / clip.w;
    let ndc_y = clip.y / clip.w;
    (
        (ndc_x * 0.5 + 0.5) * w as f32,
        (1.0 - (ndc_y * 0.5 + 0.5)) * h as f32,
    )
}

/// The world-space nametag anchor `gpu/nametag.rs::push_entity_quads` uses:
/// `feet.y + base_height * scale + 0.5` — reads the **same**
/// `lodestone_data::entity_dimensions` census the render code does, rather
/// than a remembered height constant, so a real drift in either place shows
/// up as a location mismatch instead of being silently absorbed by two
/// copies of one guess agreeing with each other.
fn expected_anchor(type_path: &str, feet: glam::Vec3, scale: f32) -> glam::Vec3 {
    let height = lodestone_data::entity_types::entity_type_id_parts("minecraft", type_path)
        .and_then(lodestone_data::entity_dimensions::base_dimensions)
        .map(|dims| dims.height)
        .filter(|h| *h > 0.0)
        .unwrap_or(1.8);
    feet + glam::Vec3::new(0.0, height * scale + 0.5, 0.0)
}

fn camera() -> Camera {
    Camera {
        position: glam::Vec3::new(0.0, 1.0, 0.0),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 60.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(8, 0),
    }
}

fn base_draw(id: i32, type_path: &str, feet: glam::Vec3, scale: f32) -> EntityDraw {
    EntityDraw {
        hurt: false,
        block_state: None,
        id,
        type_path: type_path.to_owned(),
        item: None,
        equipment: Vec::new(),
        equipment_dye: Vec::new(),
        equipment_trim: Vec::new(),
        feet,
        yaw: 0.0,
        head_yaw: 0.0,
        pitch: 0.0,
        scale,
        anim: AnimInput::REST,
        wool: None,
        count: 1,
        foil: false,
        name_tag: None,
        item_use: None,
        creeper_swelling: 0.0,
        on_fire: false,
        // Not a player, so no skin can apply.
        player_skin: None,
    }
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_named_entity_draws_text_pixels_above_it() {
    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let state = RenderState::new(device, queue, format, W, H, None);
    let cam = camera();

    let feet = glam::Vec3::new(0.0, 0.0, 6.0);
    let subject = EntityDraw {
        name_tag: Some(NameTag {
            text: "Babe".to_owned(),
            see_through: true,
        }),
        ..base_draw(1, "pig", feet, 1.0)
    };
    // The control: byte-identical except for `name_tag`. Both "no custom
    // name" and "custom name reported but not visible" collapse to `None`
    // by the time a snapshot becomes an `EntityDraw` (`net::entity_snapshot`
    // resolves the rule before this point), so this one control covers both.
    let control = EntityDraw {
        name_tag: None,
        ..subject.clone()
    };

    let mut shoot = |draw: &EntityDraw| -> Vec<u8> {
        let frame = target.acquire().expect("headless acquire");
        state.render(device, queue, frame.view(), &cam, None, std::slice::from_ref(draw));
        target.read_texels(device, queue)
    };

    let subject_px = shoot(&subject);
    let control_px = shoot(&control);

    let sky = sky_bytes();
    let subject_count = non_sky_count(&subject_px, sky);
    let control_count = non_sky_count(&control_px, sky);
    let delta = subject_count as isize - control_count as isize;

    let anchor = expected_anchor("pig", feet, 1.0);
    let (ax, ay) = project(cam.view_projection(), anchor, W, H);
    let bbox = diff_bbox(&subject_px, &control_px, W);

    eprintln!("=== nametag pixel gate ===");
    eprintln!("subject non-sky px = {subject_count}");
    eprintln!("control non-sky px = {control_count}");
    eprintln!("delta               = {delta}");
    eprintln!("analytic anchor screen pos = ({ax:.1}, {ay:.1})");
    eprintln!("diff bbox = {bbox:?}");

    assert!(
        control_count > 50,
        "control: the pig body itself must reach a substantial run of pixels \
         ({control_count}) — if this is near zero the whole entity path is broken, not just \
         the nametag"
    );
    assert!(
        delta > 20,
        "a named entity must draw visibly more than the same entity with no tag; got \
         delta={delta} (subject={subject_count}, control={control_count})"
    );

    let (lo_x, hi_x, lo_y, hi_y) = bbox.expect(
        "the subject/control frames must differ somewhere — got byte-identical frames, which \
         means the nametag pass drew nothing at all",
    );
    // Generous tolerance: text width/height plus the shadow offset, given
    // `PX_SCALE` at this distance projects to a handful of screen pixels per
    // logical text pixel — not a tight pixel-perfect box, just "near the
    // analytic anchor, not somewhere unrelated on screen".
    let tol = 60.0;
    assert!(
        (ax - (lo_x as f32 + hi_x as f32) / 2.0).abs() < tol
            && ay > (ay - tol).max(0.0)
            && (ay - lo_y as f32).abs() < tol.max((hi_y - lo_y) as f32),
        "the pixel diff bbox ({lo_x},{hi_x})x({lo_y},{hi_y}) must sit near the analytically \
         projected anchor ({ax:.1},{ay:.1}) — far off means the tag drew somewhere the real \
         render code's own anchor math does not predict"
    );
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn occlusion() {
    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let state = RenderState::new(device, queue, format, W, H, None);
    let cam = camera();

    // A giant, close entity — real depth-tested-and-written geometry via the
    // ordinary entity pass (the same one terrain uses), standing in for a
    // wall with no terrain harness required. `feet.y` is dropped low enough,
    // and `scale` big enough, that its silhouette should cover essentially
    // the whole frame at this distance.
    let occluder = base_draw(9, "pig", glam::Vec3::new(0.0, -6.0, 7.0), 14.0);
    // The distant, tagged entity, well behind the occluder.
    let far_feet = glam::Vec3::new(0.0, 0.0, 30.0);
    let standing = EntityDraw {
        name_tag: Some(NameTag {
            text: "Behind The Wall".to_owned(),
            see_through: true,
        }),
        ..base_draw(1, "pig", far_feet, 1.0)
    };
    let sneaking = EntityDraw {
        name_tag: Some(NameTag {
            text: "Behind The Wall".to_owned(),
            see_through: false,
        }),
        ..standing.clone()
    };

    let mut shoot = |draws: &[EntityDraw]| -> Vec<u8> {
        let frame = target.acquire().expect("headless acquire");
        state.render(device, queue, frame.view(), &cam, None, draws);
        target.read_texels(device, queue)
    };

    let baseline_px = shoot(&[occluder.clone()]);
    let standing_px = shoot(&[occluder.clone(), standing]);
    let sneaking_px = shoot(&[occluder.clone(), sneaking]);

    let sky = sky_bytes();
    let baseline_count = non_sky_count(&baseline_px, sky);

    eprintln!("=== nametag occlusion gate ===");
    eprintln!("baseline (occluder only) non-sky px = {baseline_count} / {}", W * H);

    // The premise a control must prove before it is trusted (CLAUDE.md: "a
    // control's premise can be false before the feature under test ever
    // existed"): the occluder really does cover essentially the whole frame,
    // not just some corner that happens to miss the tag's screen position.
    assert!(
        baseline_count as f32 > (W * H) as f32 * 0.9,
        "the occluder must cover the vast majority of the frame ({baseline_count} / {}), or \
         this gate cannot claim the far tag's screen position is actually behind it",
        W * H
    );

    let sneaking_bbox = diff_bbox(&baseline_px, &sneaking_px, W);
    let standing_bbox = diff_bbox(&baseline_px, &standing_px, W);
    eprintln!("sneaking (occluded, no see-through) diff bbox = {sneaking_bbox:?}");
    eprintln!("standing (occluded, see-through) diff bbox    = {standing_bbox:?}");

    // The control that proves the occluder genuinely blocks the depth-tested
    // normal pass: a sneaking entity's tag has *no* see-through pass to fall
    // back on (`NameTag::see_through: false`), so fully occluded it must be
    // completely invisible — pixel-identical to the no-tag baseline.
    assert!(
        sneaking_bbox.is_none(),
        "control failed: a sneaking (non-see-through) tag fully behind the occluder must be \
         pixel-identical to the no-tag baseline (proving the occluder really occludes the \
         depth-tested normal pass), but got a diff bbox of {sneaking_bbox:?}"
    );

    // The actual claim: a standing (see-through-eligible) tag in the exact
    // same occluded position must still contribute real pixels — the
    // depth-testless see-through pass, drawn "dimmed" rather than hidden.
    assert!(
        standing_bbox.is_some(),
        "a standing entity's tag, fully behind the same occluder the sneaking control proved \
         blocks the normal pass, must still contribute see-through pixels — got no diff at all, \
         meaning the see-through pass is not reaching the screen"
    );
}
