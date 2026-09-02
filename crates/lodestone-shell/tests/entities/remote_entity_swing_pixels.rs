//! Pixel gate: a **remote** entity's arm must move on screen once it reports
//! a `SwingMainHand` animation, and a second, otherwise-identical entity that
//! never gets the report must not move at all (
//! `docs/arm-swing-animation.md`).
//!
//! # Why this cannot be a unit test
//!
//! `ClientboundAnimatePacket` decoded cleanly into `ClientEvent::EntityAnimation`
//! long before this fix, with a green protocol-layer test
//! (`v770/tests/entity_events.rs`) and a component (`Skeleton::pose`'s
//! `attack_anim`) that was itself unit-tested and correct. Neither of those
//! tests could ever fail on the missing wiring: `lodestone_ecs::ingest::
//! handles_event`'s routing switch had no arm for `EntityAnimation`, so
//! `SharedState::apply` never ran `NetIngest` for it and no system saw the
//! event in production, even though a hermetic `feed()`-style call in
//! `lodestone-ecs` would have bypassed that switch entirely and shown green.
//! Per `CLAUDE.md`'s dominant defect class, only a gate that drives the real
//! shell render path can see that: decoded, tested, and reaching zero pixels.
//!
//! # What this drives
//!
//! A `bevy_ecs::World` built from the *production* plugin pair —
//! `lodestone_ecs::ingest::IngestPlugin` (the net thread's fold) and
//! `lodestone_shell::entities::EntityInterpPlugin` (the render-side fold) —
//! the same two `Sim::new` installs in the same `App`. Two `minecraft:zombie`
//! tracks are spawned at the identical pose (ids 1 and 2); only id 1 is ever
//! given a `ClientEvent::EntityAnimation { action: SwingMainHand }`. Both are
//! then extracted through the real `extract_entity_draws` system — not
//! hand-built `EntityDraw`s — and rendered through `RenderState::render`,
//! exactly `app.rs`'s frame loop's call.
//!
//! # The metric
//!
//! Per-location pixel diff, not a frame average (`CLAUDE.md`: "measure by
//! location, never by frame average" — a percentage cannot tell a
//! uniform-but-wrong frame from a localised blob). Each entity is rendered
//! twice: once before the animate event existed (`_rest`) and once three
//! `GameTick`s after it (`_mid`). The subject's rest-vs-mid diff must cover a
//! real run of pixels with a printed bounding box; the control's must not.
//!
//! # The negative control, and what it actually printed
//!
//! Entity id 2 goes through the identical two extractions and renders, the
//! identical elapsed ticks, and the identical camera — the only thing it never
//! receives is the `EntityAnimation` event. Its `attack_anim` is asserted
//! `0.0` at both captures (the direct, mechanical control), and its
//! rest-vs-mid pixel diff must fail the same "real run of pixels" assertion
//! the subject passes. See the test body for the run's actual printed numbers
//! — this file does not describe what the control would print, it prints what
//! it did.
//!
//! No vanilla `client.jar` is needed: entity rigs come from
//! `EntityModelSet::load()`'s baked-in corpus (`RenderState::new(.., None)`,
//! the same as `armour_pixels.rs`/`sheep_wool_pixels.rs`), so the only
//! `#[ignore]` reason is the GPU adapter.
//!
//! ```text
//! cargo test -p lodestone-shell --test remote_entity_swing_pixels -- --ignored --nocapture
//! ```

use bevy_ecs::world::World;
use lodestone::entities::{EntityDraw, EntityInterpPlugin, extracted_entity_draws, fold_entities};
use lodestone::gpu::{RenderState, SKY_COLOR};
use lodestone_ecs::app::App;
use lodestone_ecs::ingest::{IngestPlugin, IngestQueue};
use lodestone_ecs::{Extract, GameTick, NetIngest};
use lodestone_model::{AnimationAction, ClientEvent, Rotation, Vec3 as ModelVec3};
use lodestone_render::{AnimInput, Camera, GpuContext, HeadlessTarget, RenderTarget};

const W: u32 = 320;
const H: u32 = 240;

fn sky_bytes() -> [u8; 3] {
    SKY_COLOR.map(|c| (c * 255.0).round() as u8)
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

/// A `World` with the real ingest + render-side entity plugins installed
/// together, exactly as `Sim::new` installs them in its one `App` (see
/// `sim.rs`'s `app.add_plugins((.., IngestPlugin, .., EntityInterpPlugin, ..))`) —
/// minus the player/terrain/interaction plugins this gate has no use for.
fn world_with_two_tracked_zombies(feet: glam::Vec3) -> World {
    let mut app = App::new();
    app.add_plugins((IngestPlugin, EntityInterpPlugin));
    let mut world = std::mem::take(app.world_mut());

    for id in [1, 2] {
        world.resource_mut::<IngestQueue>().push(ClientEvent::EntitySpawned {
            entity_id: id,
            uuid: None,
            entity_type: "minecraft:zombie".parse().expect("valid entity type key"),
            pos: ModelVec3::new(f64::from(feet.x), f64::from(feet.y), f64::from(feet.z)),
            rotation: Rotation::new(0.0, 0.0),
            velocity: None,
        });
        world.run_schedule(NetIngest);
    }

    // Read the ingest components `apply_entity_spawn` already wrote
    // directly, rather than a hand-built `EntitySnapshot` — same as
    // `Sim::fold_entities` does live.
    fold_entities(&mut world);
    world
}

fn draw_for(world: &World, id: i32) -> EntityDraw {
    extracted_entity_draws(world)
        .into_iter()
        .find(|d| d.id == id)
        .unwrap_or_else(|| panic!("entity {id} not among the extracted draws"))
}

/// The bounding box and count of pixels that differ by more than rounding
/// noise between two same-sized frames — `None` when they are identical.
/// Printed rather than folded into a single percentage per `CLAUDE.md`'s
/// "measure by location" rule: a bounding box can show a diff is localised to
/// the arm, where a bare percentage could not distinguish that from noise
/// scattered across the whole frame.
fn diff_bbox(a: &[u8], b: &[u8], w: u32, h: u32) -> Option<(u32, u32, u32, u32, usize)> {
    let (mut min_x, mut max_x, mut min_y, mut max_y, mut n) = (u32::MAX, 0u32, u32::MAX, 0u32, 0usize);
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let d = (i32::from(a[i]) - i32::from(b[i])).abs()
                + (i32::from(a[i + 1]) - i32::from(b[i + 1])).abs()
                + (i32::from(a[i + 2]) - i32::from(b[i + 2])).abs();
            if d > 20 {
                n += 1;
                min_x = min_x.min(x);
                max_x = max_x.max(x);
                min_y = min_y.min(y);
                max_y = max_y.max(y);
            }
        }
    }
    if n == 0 { None } else { Some((min_x, max_x, min_y, max_y, n)) }
}

#[test]
#[ignore = "requires a GPU adapter"]
fn a_remote_swing_moves_the_arm_and_a_silent_entity_does_not() {
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

    // Same fixture shape as `armour_pixels.rs`/`sheep_wool_pixels.rs`: camera
    // at the origin, mob a few blocks south.
    let feet = glam::Vec3::new(0.0, 0.0, 4.0);
    let mut world = world_with_two_tracked_zombies(feet);
    world.run_schedule(Extract);

    let subject_rest = draw_for(&world, 1);
    let control_rest = draw_for(&world, 2);
    assert_eq!(
        subject_rest.anim.attack_anim, 0.0,
        "before any EntityAnimation event, attack_anim must be exactly REST's 0.0 \
         (AnimInput::REST.attack_anim == {})",
        AnimInput::REST.attack_anim
    );
    assert_eq!(control_rest.anim.attack_anim, 0.0);

    // The one real event: only entity 1 ever reports a swing.
    world.resource_mut::<IngestQueue>().push(ClientEvent::EntityAnimation {
        entity_id: 1,
        action: AnimationAction::SwingMainHand,
    });
    world.run_schedule(NetIngest);
    for _ in 0..3 {
        world.run_schedule(GameTick);
    }
    world.run_schedule(Extract);

    let subject_mid = draw_for(&world, 1);
    let control_mid = draw_for(&world, 2);

    eprintln!("=== remote entity swing pixel gate ===");
    eprintln!(
        "subject attack_anim: rest={:.4} mid={:.4}",
        subject_rest.anim.attack_anim, subject_mid.anim.attack_anim
    );
    eprintln!(
        "control attack_anim: rest={:.4} mid={:.4}",
        control_rest.anim.attack_anim, control_mid.anim.attack_anim
    );

    assert!(
        subject_mid.anim.attack_anim > 0.05,
        "three ticks into a 6-tick swing, attack_anim should be well off zero; got {}",
        subject_mid.anim.attack_anim
    );
    assert_eq!(
        control_mid.anim.attack_anim, 0.0,
        "an entity that never received EntityAnimation must never gain swing progress — \
         if this fails, the ingest system is not filtering by entity id"
    );

    let mut shoot = |draw: &EntityDraw| -> (Vec<u8>, lodestone::gpu::RenderStats) {
        let frame = target.acquire().expect("headless acquire");
        let stats = state.render(device, queue, frame.view(), &cam, None, std::slice::from_ref(draw));
        (target.read_texels(device, queue), stats)
    };

    let (subject_rest_px, subject_rest_stats) = shoot(&subject_rest);
    let (subject_mid_px, subject_mid_stats) = shoot(&subject_mid);
    let (control_rest_px, control_rest_stats) = shoot(&control_rest);
    let (control_mid_px, control_mid_stats) = shoot(&control_mid);

    for (label, stats) in [
        ("subject rest", subject_rest_stats),
        ("subject mid", subject_mid_stats),
        ("control rest", control_rest_stats),
        ("control mid", control_mid_stats),
    ] {
        assert_eq!(
            stats.entities_drawn, 1,
            "{label}: entities_drawn={} — the zombie itself failed to reach the entity \
             pipeline, which would make this gate measure the absence of an entity rather \
             than the absence of a swing",
            stats.entities_drawn
        );
    }

    let sky = sky_bytes();
    let _ = sky; // kept for parity with sibling gates' `sky_bytes()`; diff_bbox compares frames directly.

    let subject_diff = diff_bbox(&subject_rest_px, &subject_mid_px, W, H);
    let control_diff = diff_bbox(&control_rest_px, &control_mid_px, W, H);

    eprintln!("subject rest-vs-mid diff bbox (x0,x1,y0,y1,count) = {subject_diff:?}");
    eprintln!("control rest-vs-mid diff bbox (x0,x1,y0,y1,count) = {control_diff:?}");

    let (sx0, sx1, sy0, sy1, scount) = subject_diff.unwrap_or_else(|| {
        panic!(
            "the swinging zombie's rest and mid-swing frames are byte-identical — the \
             arm never moved a single pixel, which is issue #10 exactly"
        )
    });
    assert!(
        scount > 30,
        "the subject's rest-vs-mid diff is only {scount} px at ({sx0},{sy0})-({sx1},{sy1}) — \
         too small to be a real arm movement rather than rounding noise"
    );

    // The negative control, run through the identical pipeline and the
    // identical elapsed ticks, differing only in whether `EntityAnimation`
    // ever reached it: it must fail the very "a real run of pixels changed"
    // assertion the subject just passed.
    match control_diff {
        None => {
            eprintln!("control: byte-identical rest vs mid, as expected — no event, no movement");
        }
        Some((cx0, cx1, cy0, cy1, ccount)) => {
            eprintln!(
                "control: {ccount} px differed at ({cx0},{cy0})-({cx1},{cy1}) \
                 (subject moved {scount} px)"
            );
            assert!(
                ccount < scount / 4,
                "the silent control moved {ccount} px — nearly as much as the real swing \
                 ({scount} px) — so the diff is not attributable to the swing"
            );
        }
    }
}
