//! Pixel gate: an entity with `entityShadows` on must draw **more** non-sky
//! pixels than the identical entity with the option off — the shadow decal's
//! own ring, extending past the mob's silhouette onto ground the body pass
//! never touches. Same shape as `armour_pixels.rs`'s
//! `a_fully_armoured_zombie_draws_more_silhouette_than_a_bare_one`, and for
//! the same reason: `RenderState::prepare_shadows` is proven only by
//! construction otherwise, and a closed unit-test loop cannot tell "reaches
//! pixels" from "never called".
//!
//! # No terrain in this harness — that is what makes the ring visible
//!
//! `RenderState::render` here is driven with no world sections at all (the
//! same headless-entity fixture `armour_pixels.rs` uses), so the only thing
//! that can paint a pixel near the ground plane is the shadow decal itself —
//! there is no brown dirt block for it to darken. That makes the shadow's
//! contribution *more* visible here than in a real scene, not less: every
//! shadow-covered texel the camera can see past the mob's own silhouette
//! reads as a fresh non-sky pixel against the plain sky clear.
//!
//! **Read that as scope, not as reassurance.** No terrain also means no depth
//! competition, so this gate is structurally blind to whether the decal
//! *survives* the depth test against the ground it is coplanar with —
//! measured, not argued: with `EntityPipeline::SHADOW_DEPTH_BIAS` inverted,
//! which makes every ground shadow in a real world invisible, this file still
//! reports its usual `7703 / 6699 / delta 1004` and passes. That question
//! belongs to `entity_shadow_z_fight_pixels.rs`, which stands the same mob on
//! real meshed terrain; this one answers "does the pass reach pixels at all",
//! and only that.
//!
//! # The ground and the camera
//!
//! `prepare_shadows` needs a `ShadowGroundSource`, which nothing in this
//! headless fixture would otherwise install (there is no `Sim`/`NetClient`
//! here) — this test installs a synthetic one directly:
//! `minecraft:stone` for every block at `y < 0`, `None` (air) everywhere
//! else, so the entity standing at `feet.y == 0` finds real ground exactly
//! one cell below it, the same shape a live world would answer with.
//!
//! The camera looks down at roughly 45°, elevated and pulled back from the
//! entity, so both the mob and a margin of the (otherwise invisible) ground
//! plane around its feet are in frame — a level, `pitch: 0` camera like
//! `armour_pixels.rs`'s would see the shadow quad edge-on.
//!
//! # The control
//!
//! Not "the ground source is never installed" — that gate would not
//! distinguish "shadows draw" from "shadows were never attempted". The
//! control here is the **same** entity, same pose, same camera, same
//! installed ground, with only `RenderState::set_entity_shadows_enabled(false)`
//! differing — exactly vanilla's own `entityShadows` toggle, so any pixel
//! delta is attributable to the shadow *pass* itself, not to whether the
//! ground/light plumbing exists.
//!
//! [`RenderStats::shadow_pieces_drawn`] is asserted too, as an exact,
//! non-approximate corroboration: nonzero with the option on, exactly zero
//! with it off.
//!
//! Fail-closed, like its siblings: no GPU adapter or no `client.jar` (no
//! `shadow.png`) is a failure, never a skip.
//!
//! ```text
//! cargo test -p lodestone-shell --test entity_shadow_pixels -- --ignored --nocapture
//! ```

use lodestone::entities::EntityDraw;
use lodestone::gpu::{RenderState, SKY_COLOR};
use lodestone_render::{AnimInput, Camera, GpuContext, HeadlessTarget, RenderTarget};

const W: u32 = 320;
const H: u32 = 240;

/// The bytes the sky clear actually lands on in this readback — see
/// `armour_pixels.rs::sky_bytes`'s own doc for why this is derived rather
/// than a second hand-typed copy.
fn sky_bytes() -> [u8; 3] {
    SKY_COLOR.map(|c| (c * 255.0).round() as u8)
}

/// Non-sky pixels in `pixels` — `armour_pixels.rs::non_sky_count`, unchanged.
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

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn entity_shadows_draw_a_ring_the_bare_silhouette_does_not() {
    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);

    let mut state = RenderState::new(device, queue, format, W, H, None);

    // A flat `minecraft:stone` floor at y < 0, air everywhere at y >= 0 — the
    // one real "ground" cell an entity standing at feet.y == 0 needs.
    let stone = lodestone_data::block_states::state_id("minecraft:stone")
        .expect("minecraft:stone must resolve to a real state id");
    state.set_shadow_ground_source(move |[_, y, _]| if y < 0 { Some(stone) } else { None });

    // Elevated and pulled back, pitched down ~45°, so the camera sees both
    // the mob and a margin of ground around its feet — a level camera like
    // `armour_pixels.rs`'s would see the flat shadow quad edge-on.
    let feet = glam::Vec3::new(0.0, 0.0, 4.0);
    let camera = Camera {
        position: glam::Vec3::new(0.0, 3.5, 1.0),
        yaw: 0.0,
        pitch: 45.0,
        fov_y_degrees: 70.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(8, 0),
    };

    let subject = EntityDraw {
        hurt: false,
        block_state: None,
        item_frame_rotation: 0,
        id: 1,
        type_path: std::sync::Arc::from("zombie"),
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
        name_tag: None,
        item_use: None,
        creeper_swelling: 0.0,
        swim_amount: 0.0,
        death_time: 0.0,
        on_fire: false,
        invisible: false,
        armor_stand: None,
        player_skin: None,
        variant_sheet: None,
        experience_orb_value: None,
        cape_sway: (0.0, 0.0, 0.0),
    };

    let mut shoot = |state: &RenderState| -> (Vec<u8>, lodestone::gpu::RenderStats) {
        let frame = target.acquire().expect("headless acquire");
        let stats = state.render(
            device,
            queue,
            frame.view(),
            &camera,
            None,
            std::slice::from_ref(&subject),
        );
        (target.read_texels(device, queue), stats)
    };

    // Subject: `entityShadows` on (vanilla's own default, and this
    // `RenderState`'s untouched default — see `set_entity_shadows_enabled`'s
    // doc).
    let (subject_px, subject_stats) = shoot(&state);
    // Control: identical in every respect except the option.
    state.set_entity_shadows_enabled(false);
    let (control_px, control_stats) = shoot(&state);

    let sky = sky_bytes();
    let subject_count = non_sky_count(&subject_px, sky);
    let control_count = non_sky_count(&control_px, sky);
    let delta = subject_count as isize - control_count as isize;

    eprintln!("=== entity shadow pixel gate ===");
    eprintln!("subject (shadows ON)  non-sky px = {subject_count}");
    eprintln!("control (shadows OFF) non-sky px = {control_count}");
    eprintln!("delta                             = {delta}");
    eprintln!("subject shadow_pieces_drawn       = {}", subject_stats.shadow_pieces_drawn);
    eprintln!("control shadow_pieces_drawn       = {}", control_stats.shadow_pieces_drawn);

    // The exact, non-approximate corroboration: the pass actually ran on the
    // subject and produced real geometry, and produced none at all once the
    // option — the only thing that differs between the two calls — is off.
    assert!(
        subject_stats.shadow_pieces_drawn > 0,
        "shadow_pieces_drawn=0 on the subject means the ground scan found no ground under \
         the entity — either the installed ShadowGroundSource or the collision-shape gate is \
         broken, not (yet) a pixel question"
    );
    assert_eq!(
        control_stats.shadow_pieces_drawn, 0,
        "the control has entityShadows off; shadow_pieces_drawn must be exactly 0, got {}",
        control_stats.shadow_pieces_drawn
    );

    // The load-bearing pixel assertion. A shadow quad, even under fairly
    // conservative alpha, is easily 10s of pixels at this resolution and
    // camera distance — the floor is deliberately loose (a handful of
    // pixels), because the point is presence, not a tight photometric match.
    assert!(
        delta > 5,
        "entity shadows should draw a visibly larger non-sky footprint than the same entity \
         with the option off (the shadow decal's own ring beyond the mob's silhouette, over \
         plain sky since this harness has no terrain); got delta={delta}. Near-zero or negative \
         means the shadow pass is not reaching pixels."
    );
    // A broad sanity ceiling: the shadow ring cannot plausibly rival or
    // exceed the mob's own silhouette at this camera distance.
    assert!(
        (delta as usize) < control_count.max(1) * 2,
        "the shadow delta ({delta}) is implausibly large next to the mob's own silhouette \
         ({control_count}) — likely a broken control (e.g. the ground plane leaking into the \
         mob's own draw) rather than a real shadow effect"
    );

    assert!(
        control_count > 50,
        "the bare mob itself should reach a real run of pixels ({control_count}); if this is \
         near zero the whole entity path is broken, not just shadows"
    );
}
