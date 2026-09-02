//! Pixel gate: a **remote** entity reporting bit `0x01` of the shared-flags
//! byte must show the mob-fire billboard on screen, and clearing that bit (or
//! never reporting it) must draw byte-identically to no flame pass at all
//! (player report: "mobs dont show flames yet").
//!
//! # Why this needs the render path, not just a unit test
//!
//! `EntityDraw::on_fire`'s extraction (`entities.rs`) is covered by a
//! hermetic unit test already
//! (`an_entity_flags_bit_reaches_the_extracted_draw_as_on_fire`) — but that
//! test cannot see whether anything downstream of the flag actually paints a
//! pixel. Per `CLAUDE.md`'s dominant defect class, a flag that decodes
//! correctly and reaches zero pixels is exactly the failure mode a unit test
//! of the decode step cannot catch; only a gate that drives
//! `RenderState::render` can.
//!
//! # What this drives
//!
//! The same production-plugin `World` shape as
//! `remote_entity_swing_pixels.rs`: `lodestone_ecs::ingest::IngestPlugin` (the
//! net thread's fold, which is what actually inserts `EntityFlags` from an
//! `EntityMetadataUpdated` event) and `lodestone_shell::entities::
//! EntityInterpPlugin` (the render-side fold), then a real
//! `extract_entity_draws` call — not a hand-built `EntityDraw` — and a real
//! `RenderState::render`.
//!
//! Three renders of the same `minecraft:zombie` type at the same pose:
//! * `on` — bit `0x01` set (`EntityMetadataUpdate { flags: Some(0x01), .. }`).
//! * `off` — bit `0x01` explicitly reported **clear**
//!   (`flags: Some(0x00)`) on the *same* tracked entity, after `on`.
//! * `never` — a second, otherwise-identical entity that never receives any
//!   metadata at all.
//!
//! `off` and `never` must be **pixel-for-pixel identical**
//! (`assert_eq!` on the raw framebuffer bytes, not a tolerance) — that is
//! the "bit-identical to no flame pass" property `CLAUDE.md`'s evidence
//! standards ask for, checked at the one level a unit test cannot reach.
//! `on` must differ from `off` over a real, printed bounding box of pixels,
//! not a handful of rounding-noise texels.
//!
//! No vanilla `client.jar` is strictly required for the *entity* itself (the
//! zombie falls back to a synthetic placeholder sheet, same as every other
//! entity pixel gate here) — but the flame texture has no synthetic fallback
//! by design (see `EntityRenderer::flame_texture`'s doc), so this gate only
//! exercises real flame pixels when a `.cache/mc/<version>` pack is
//! discoverable from the test binary's working directory (true for `cargo
//! test` run from this repo, per `crate::gpu::entities::load_flame_textures`'s
//! doc). Without one, `on` and `off` are expected to be identical too, and
//! this gate says so rather than silently passing — see the first assertion
//! block below.
//!
//! ```text
//! cargo test -p lodestone-shell --test mob_fire_pixels -- --ignored --nocapture
//! ```

use bevy_ecs::world::World;
use lodestone::entities::{EntityDraw, EntityInterpPlugin, extracted_entity_draws, fold_entities};
use lodestone::gpu::RenderState;
use lodestone_ecs::app::App;
use lodestone_ecs::ingest::{IngestPlugin, IngestQueue};
use lodestone_ecs::{Extract, NetIngest};
use lodestone_model::{ClientEvent, EntityMetadataUpdate, Rotation, Vec3 as ModelVec3};
use lodestone_render::{Camera, GpuContext, HeadlessTarget, RenderTarget};

const W: u32 = 320;
const H: u32 = 240;

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

/// Two tracked zombies at `feet`, through the real ingest + render-side
/// plugin pair — see `remote_entity_swing_pixels.rs::world_with_two_tracked_zombies`
/// for the identical shape, adapted here for `EntityMetadataUpdated` instead
/// of `EntityAnimation`.
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
/// Identical shape to `remote_entity_swing_pixels.rs::diff_bbox` — see that
/// file's doc for why this is a bounding box and not a bare percentage.
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
fn on_fire_draws_a_flame_and_the_off_case_is_pixel_identical_to_never_reported() {
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

    // Same fixture shape as the sibling entity pixel gates: camera at the
    // origin, mob a few blocks south, entirely inside frame (the flame stack
    // reaches a few blocks above a zombie's head, per
    // `entity_pipeline::tests::zombie_flame_geometry_matches_the_hand_derived_prediction`'s
    // ~3.07-block predicted top).
    let feet = glam::Vec3::new(0.0, 0.0, 4.0);
    let mut world = world_with_two_tracked_zombies(feet);
    world.run_schedule(Extract);

    let never_reported = draw_for(&world, 2);
    assert_eq!(
        never_reported.on_fire, false,
        "entity 2 must never report on_fire — it receives no metadata at all"
    );

    // Bit set: entity 1 is on fire.
    world.resource_mut::<IngestQueue>().push(ClientEvent::EntityMetadataUpdated {
        entity_id: 1,
        metadata: EntityMetadataUpdate {
            flags: Some(0x01),
            ..Default::default()
        },
    });
    world.run_schedule(NetIngest);
    world.run_schedule(Extract);
    let subject_on = draw_for(&world, 1);
    assert_eq!(
        subject_on.on_fire, true,
        "a live EntityFlags(0x01) must reach EntityDraw::on_fire — this is issue #434's \
         extraction half, exercised here through the real ingest pipeline rather than a \
         hand-built component"
    );

    // Bit cleared: the same entity, told explicitly that the bit is now 0 —
    // not "never reported again", a real report of "off".
    world.resource_mut::<IngestQueue>().push(ClientEvent::EntityMetadataUpdated {
        entity_id: 1,
        metadata: EntityMetadataUpdate {
            flags: Some(0x00),
            ..Default::default()
        },
    });
    world.run_schedule(NetIngest);
    world.run_schedule(Extract);
    let subject_off = draw_for(&world, 1);
    assert_eq!(
        subject_off.on_fire, false,
        "clearing bit 0x01 must clear on_fire, not leave it latched from the earlier report"
    );

    let mut shoot = |draw: &EntityDraw| -> (Vec<u8>, lodestone::gpu::RenderStats) {
        let frame = target.acquire().expect("headless acquire");
        let stats = state.render(device, queue, frame.view(), &cam, None, std::slice::from_ref(draw));
        (target.read_texels(device, queue), stats)
    };

    let (on_px, on_stats) = shoot(&subject_on);
    let (off_px, off_stats) = shoot(&subject_off);
    let (never_px, never_stats) = shoot(&never_reported);

    for (label, stats) in [("on", &on_stats), ("off", &off_stats), ("never", &never_stats)] {
        assert_eq!(
            stats.entities_drawn, 1,
            "{label}: entities_drawn={} — the zombie itself failed to reach the entity \
             pipeline, which would make this gate measure the absence of an entity rather \
             than the absence of a flame",
            stats.entities_drawn
        );
    }
    eprintln!(
        "flame_billboards_drawn: on={} off={} never={}",
        on_stats.flame_billboards_drawn, off_stats.flame_billboards_drawn, never_stats.flame_billboards_drawn
    );
    assert_eq!(
        on_stats.flame_billboards_drawn, 1,
        "on_fire=true must produce exactly one flame billboard"
    );
    assert_eq!(
        off_stats.flame_billboards_drawn, 0,
        "on_fire=false (explicitly reported off) must produce zero flame billboards"
    );
    assert_eq!(
        never_stats.flame_billboards_drawn, 0,
        "an entity that never reported flags must produce zero flame billboards"
    );

    // The bit-identical negative control: "off" and "never" must be the exact
    // same frame, byte for byte — not merely "visually similar". A tolerance
    // check here would let a low-alpha residual flame slip through unnoticed.
    assert_eq!(
        off_px, never_px,
        "on_fire=false must render byte-identical to an entity that never reported the \
         flags byte at all — any difference here is a flame pass that still draws \
         something when the flag is clear"
    );

    let diff = diff_bbox(&off_px, &on_px, W, H);
    eprintln!("on-vs-off diff bbox (x0,x1,y0,y1,count) = {diff:?}");
    let Some((x0, x1, y0, y1, count)) = diff else {
        panic!(
            "on_fire=true rendered byte-identical to on_fire=false — the flame pass never \
             painted a single pixel, which is issue #434 exactly. This can also mean no \
             `.cache/mc/<version>` pack is discoverable from this test binary's working \
             directory (see this file's module doc) — check for that before assuming a \
             code regression."
        );
    };
    assert!(
        count > 50,
        "on-vs-off diff is only {count} px at ({x0},{y0})-({x1},{y1}) — too small to be a \
         real flame billboard rather than antialiasing noise at the mob's silhouette edge"
    );
}
