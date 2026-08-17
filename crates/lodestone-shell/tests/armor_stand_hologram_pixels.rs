//! Pixel gate for issue #643's remaining half: an invisible entity's own
//! body/rig contributes **zero** pixels while its nametag still contributes
//! real ones — the "server hologram" trick (an invisible, custom-named
//! armour stand). Driven through the real [`RenderState::render`] path, the
//! same call `app.rs`'s frame loop makes — not a reimplementation of
//! `RenderState::prepare_entities`'s own `if e.invisible { continue; }` gate.
//!
//! **Probe method: full-frame rasterized pixel readback, not vertex
//! sampling.** Every assertion below counts real output texels from a real
//! render pass, so a quad larger than some point-sampled probe rect (see
//! `CLAUDE.md`'s note on vertex-sampled probes) cannot slip past it — the
//! whole frame is the probe.
//!
//! Four draws, three real renders:
//!
//! 1. `baseline` — no entities at all, the sky-only frame.
//! 2. `body_only` — a **visible** armour stand, no name tag. Diffed against
//!    `baseline` this gives two things at once: the **positive control**
//!    that this entity type really does draw a body silhouette when visible
//!    (an absence assertion is worthless without proof the detector can
//!    fire at all), and the **measured** body bounding box to check the
//!    invisible case's absence against — derived from a real render, not a
//!    guessed rectangle.
//! 3. `invisible_named` (the subject) — the same armour stand,
//!    `EntityDraw::invisible: true`, with a name tag. Diffed against
//!    `baseline`, this must contribute **zero** pixels inside `body_only`'s
//!    measured body bbox (the absence claim) and at least one pixel *outside*
//!    it, above the body (the presence claim: the nametag still draws).
//!
//! Fail-closed: no GPU adapter is a failure, never a silent skip.
//!
//! ```text
//! cargo test -p lodestone-shell --test armor_stand_hologram_pixels -- --ignored --nocapture
//! ```

use lodestone::entities::{EntityDraw, NameTag};
use lodestone::gpu::{RenderState, SKY_COLOR};
use lodestone_render::{AnimInput, Camera, GpuContext, HeadlessTarget, RenderTarget};

const W: u32 = 320;
const H: u32 = 240;

fn sky_bytes() -> [u8; 3] {
    SKY_COLOR.map(|c| (c * 255.0).round() as u8)
}

/// The bounding box (in pixels) and count of every texel that differs
/// between `a` and `b` by more than a per-channel-sum threshold of `8` — the
/// same threshold `nametag_pixels.rs`/`live_dropped_item.rs` use for the
/// same purpose.
fn diff_bbox(a: &[u8], b: &[u8], w: u32) -> Option<(u32, u32, u32, u32, usize)> {
    let mut bbox: Option<(u32, u32, u32, u32, usize)> = None;
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
            None => (x, x, y, y, 1),
            Some((lo_x, hi_x, lo_y, hi_y, n)) => {
                (lo_x.min(x), hi_x.max(x), lo_y.min(y), hi_y.max(y), n + 1)
            }
        });
    }
    bbox
}

/// The count of differing texels whose location falls **inside** `rect`
/// (`lo_x..=hi_x, lo_y..=hi_y`) — the same threshold `diff_bbox` uses. This
/// is what turns "some pixels changed somewhere" into "no pixels changed
/// *in the region the body silhouette measured*", the localisation
/// `CLAUDE.md` asks for rather than a bare frame-wide count.
fn diff_count_in_rect(a: &[u8], b: &[u8], w: u32, rect: (u32, u32, u32, u32)) -> usize {
    let (lo_x, hi_x, lo_y, hi_y) = rect;
    a.chunks_exact(4)
        .zip(b.chunks_exact(4))
        .enumerate()
        .filter(|(i, (pa, pb))| {
            let x = *i as u32 % w;
            let y = *i as u32 / w;
            if x < lo_x || x > hi_x || y < lo_y || y > hi_y {
                return false;
            }
            let d = (i32::from(pa[0]) - i32::from(pb[0])).abs()
                + (i32::from(pa[1]) - i32::from(pb[1])).abs()
                + (i32::from(pa[2]) - i32::from(pb[2])).abs();
            d > 8
        })
        .count()
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

fn armor_stand_draw(feet: glam::Vec3, invisible: bool, name_tag: Option<NameTag>) -> EntityDraw {
    EntityDraw {
        hurt: false,
        block_state: None,
        id: 1,
        type_path: std::sync::Arc::from("armor_stand"),
        item: None,
        main_arm_left: false,
        equipment: Vec::new(),
        equipment_dye: Vec::new(),
        equipment_trim: Vec::new(),
        feet,
        yaw: 0.0,
        head_yaw: 0.0,
        pitch: 0.0,
        scale: 1.0,
        anim: AnimInput::REST,
        wool: None,
        count: 1,
        foil: false,
        item_dyed_color: None,
        item_potion_color: None,
        name_tag,
        item_use: None,
        creeper_swelling: 0.0,
        swim_amount: 0.0,
        death_time: 0.0,
        on_fire: false,
        invisible,
        // No `ArmorStandFlags` reported: this gate is about the shared-flags
        // invisible bit alone, not the base-plate/arms cosmetics (covered by
        // `gpu::entity_passes::tests::hide_armor_stand_parts_collapses_only_the_named_parts`),
        // so leaving this `None` keeps the body at its full, unhidden
        // silhouette for the maximum-signal positive control.
        armor_stand: None,
        player_skin: None,
        variant_sheet: None,
        experience_orb_value: None,
        cape_sway: (0.0, 0.0, 0.0),
    }
}

#[test]
#[ignore = "requires a GPU adapter"]
fn an_invisible_named_armor_stand_draws_no_body_but_still_draws_its_tag() {
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
    let tag = || NameTag {
        text: "Hologram".to_owned(),
        see_through: true,
    };

    let body_only = armor_stand_draw(feet, false, None);
    let invisible_named = armor_stand_draw(feet, true, Some(tag()));

    let mut shoot = |draws: &[EntityDraw]| -> Vec<u8> {
        let frame = target.acquire().expect("headless acquire");
        state.render(device, queue, frame.view(), &cam, None, draws);
        target.read_texels(device, queue)
    };
    let empty_px = shoot(&[]);
    let body_only_px = shoot(std::slice::from_ref(&body_only));
    let invisible_named_px = shoot(std::slice::from_ref(&invisible_named));

    let sky = sky_bytes();
    eprintln!("=== armour-stand hologram pixel gate (#643) ===");
    eprintln!("sky bytes = {sky:?}");

    // Positive control: the body really does draw a real silhouette when
    // visible — without this, an "invisible draws nothing" assertion below
    // would pass just as well with the whole entity pipeline broken.
    let body_bbox = diff_bbox(&empty_px, &body_only_px, W);
    let (lo_x, hi_x, lo_y, hi_y, body_count) = body_bbox.expect(
        "control failed: a visible, unnamed armour stand must draw a real body silhouette \
         against the empty baseline — got byte-identical frames, meaning the entity pipeline \
         itself is not reaching the screen (not an invisible-flag problem)",
    );
    eprintln!(
        "body_only vs empty: bbox=({lo_x},{hi_x})x({lo_y},{hi_y}), {body_count} px"
    );
    assert!(
        body_count > 100,
        "control: the visible armour stand's body silhouette is only {body_count} px — too \
         small to trust as a real body, not noise"
    );

    // The absence claim, localised to the measured body rect: the invisible
    // subject must contribute nothing inside the same rectangle the visible
    // control just proved the body occupies.
    let body_rect = (lo_x, hi_x, lo_y, hi_y);
    let invisible_body_px = diff_count_in_rect(&empty_px, &invisible_named_px, W, body_rect);
    eprintln!("invisible_named vs empty, inside body_rect = {invisible_body_px} px");
    assert_eq!(
        invisible_body_px, 0,
        "an invisible armour stand must contribute zero pixels inside the body rect \
         {body_rect:?} that the visible control measured — got {invisible_body_px}, meaning \
         the body/rig still drew despite EntityDraw::invisible being set"
    );

    // The presence claim: the nametag still draws, and specifically outside
    // (above) the body rect — an entity that simply drew nothing at all
    // would also pass a bare "zero pixels in body_rect" assertion, which is
    // exactly the absence-needs-a-control trap; this proves the detector
    // fires for the tag too, not just for the body.
    let full_bbox = diff_bbox(&empty_px, &invisible_named_px, W);
    let (t_lo_x, t_hi_x, t_lo_y, t_hi_y, tag_count) = full_bbox.expect(
        "an invisible, named armour stand must still draw its nametag — got byte-identical \
         frames vs the empty baseline, meaning the tag drew nothing either (this is the \
         hologram case issue #643 reports: it must show the tag with no body, not neither)",
    );
    eprintln!(
        "invisible_named vs empty (whole frame): bbox=({t_lo_x},{t_hi_x})x({t_lo_y},{t_hi_y}), \
         {tag_count} px"
    );
    assert!(
        tag_count > 0,
        "the nametag pass produced no pixels at all for the invisible subject"
    );
    assert!(
        t_hi_y <= lo_y,
        "the invisible subject's only diff (the nametag) must sit entirely above the measured \
         body rect (tag max_y={t_hi_y} must be <= body min_y={lo_y}) — if it overlaps, some of \
         the diff pixels are not accounted for by the tag alone, so the body-rect-only \
         assertion above may have gotten lucky rather than actually proving the body is gone"
    );
}
