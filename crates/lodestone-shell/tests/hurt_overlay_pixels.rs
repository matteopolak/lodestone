//! Pixel gate: a **remote** entity that reports a hurt animation must go red on
//! screen, and a second, otherwise-identical entity that never gets the report
//! must not change by a single byte (`docs/combat.md`).
//!
//! # Why this exists, and why the existing gate could not replace it
//!
//! `crates/lodestone-render/tests/entity_hurt_overlay_pixels.rs` already proves
//! the *renderer* can draw the overlay: it builds an `EntityInstanceRaw`, calls
//! `with_hurt_overlay(true)` by hand, and measures red pixels. It passed on the
//! day it landed, and on that same day production called `with_hurt_overlay`
//! **zero times anywhere in `lodestone-shell`** — the twelfth confirmed instance
//! of `CLAUDE.md`'s dominant defect class. A crate's own test suite is a closed
//! loop: it can be entirely green while the crate is dead code.
//!
//! So this gate refuses to touch `EntityInstanceRaw`, `with_hurt_overlay`, or
//! any tint word. It pushes a `ClientEvent` into the real `IngestQueue` and
//! reads texels out the other end. Every hop in between is the shipped one:
//!
//! ```text
//! ClientEvent::EntityHurtAnimation          (the wire event)
//!   -> ingest::handles_event                (the routing switch that has hidden
//!                                            working code three times)
//!   -> ingest::apply_entity_hurt_animation  -> HurtTime(10)
//!   -> entities::extract_entity_draws       -> EntityDraw::hurt
//!   -> gpu::RenderState::prepare_entities   -> the hurt half of the split plan
//!   -> upload_instances_tinted              -> InstanceTint::with_hurt
//!   -> ENTITY_WGSL fs_main                  -> pixels
//! ```
//!
//! # The metric: by location, never by frame average
//!
//! A frame average cannot tell a uniform-but-wrong frame from a localised blob,
//! and this effect is *specifically* a localised blob — vanilla's overlay is a
//! per-model blend, not the full-screen tint that fix's title asked for (there
//! is no such thing in the jar; see the issue's own comment thread). So every
//! assertion here is a bounding box and a count over a **mask**, and failure
//! output prints where, not what fraction.
//!
//! The mask is the zombie's own silhouette, derived at run time as the pixels
//! that differ between "one zombie" and "no entities at all" — never a
//! hardcoded rect. A hardcoded rect is how a HUD gate once measured 20 logical
//! pixels above a row that was drawing perfectly.
//!
//! The "is it redder?" predicate is derived from the one vanilla constant this
//! effect has: `OverlayTexture`'s red row is a flat `(255, 0, 0)` at alpha
//! `HURT_OVERLAY_ALPHA_BYTE`, so a reddened pixel is one whose distance to pure
//! red *decreased*. Nothing here hardcodes an expected RGB triple.
//!
//! # Four premises, each checked rather than assumed
//!
//! `CLAUDE.md`: a control's premise can be false before the feature under test
//! ever existed, and it fails in the safe-looking direction — the control
//! fires, the gate looks rigorous, and what it measures is unrelated.
//!
//! 1. **The zombie actually drew.** `entities_drawn == 1`, *and* the silhouette
//!    mask is a real run of pixels. A transparent placeholder texture would
//!    otherwise make every assertion below vacuous in exactly the shape of the
//!    bug (`prepare_entities` runs, nothing appears).
//! 2. **Nothing else in the frame is already red.** Asked because this is a
//!    red-tint gate: the entity-less frame is checked to contain zero pixels
//!    the reddening predicate would accept as red-dominant, so redness in a
//!    diff can only have come from the overlay. (The sky is `SKY_COLOR`, a
//!    blue; the first-person bare arm — the thing that broke a sky gate's
//!    "clears uniformly" premise — is in *both* compared frames identically
//!    and so cannot enter a diff or the mask at all.)
//! 3. **The only thing that changed is the flag.** `subject_rest` and
//!    `subject_hurt` are compared field by field: identical `EntityDraw`s
//!    except `hurt`. No `GameTick` runs between the two extractions, so pose,
//!    age and interpolation are bit-identical and the pixel difference has
//!    exactly one possible cause.
//! 4. **The control's pixels *can* redden.** The control zombie is re-rendered
//!    with `hurt` forced true and must fail the "unchanged" assertion it
//!    otherwise passes. Without this, a control that was silent because its
//!    rect was dead would look like a control that was silent because it was
//!    never damaged.
//!
//! # The negative controls, and what they printed
//!
//! Three, because there are three different ways this could pass while broken:
//!
//! * **No event** (entity 2): the whole production chain minus the one
//!   `ClientEvent`. Must be byte-identical across the two extractions.
//! * **Flag forced off**: `subject_hurt` with `hurt` overwritten to `false`,
//!   rendered through the same `RenderState::render`. Must be byte-identical to
//!   `subject_rest` — this is what attributes the diff to the flag rather than
//!   to anything else about the post-event world.
//! * **Flag forced on** (premise 4 above): the control with `hurt` forced true
//!   must redden, proving the detector is live over the control's own pixels.
//!
//! The run's actual numbers are printed by the test; this comment does not
//! describe what they would be.
//!
//! ```text
//! cargo test -p lodestone-shell --test hurt_overlay_pixels -- --ignored --nocapture
//! ```

use bevy_ecs::world::World;
use lodestone::entities::{EntityDraw, EntityInterpPlugin, extracted_entity_draws, fold_entities};
use lodestone::gpu::{RenderState, SKY_COLOR};
use lodestone_ecs::app::App;
use lodestone_ecs::ingest::{IngestPlugin, IngestQueue};
use lodestone_ecs::{Extract, NetIngest};
use lodestone_model::{ClientEvent, Rotation, Vec3 as ModelVec3};
use lodestone_render::{
    Camera, GpuContext, HURT_OVERLAY_ALPHA_BYTE, HeadlessTarget, RenderTarget,
};

const W: u32 = 320;
const H: u32 = 240;

/// Vanilla's overlay colour: `OverlayTexture`'s red row is a flat
/// `ARGB.color(-1291911168)` = `(a = 178, 255, 0, 0)`, sampled whenever
/// `LivingEntityRenderer.java` sets `hasRedOverlay`. The predicate below is
/// derived from *this*, not from any measured pixel value.
const VANILLA_OVERLAY_RGB: [i32; 3] = [255, 0, 0];

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
/// together, exactly as `Sim::new` installs them in its one `App` — the same
/// fixture `remote_entity_swing_pixels.rs` uses, minus the player/terrain
/// plugins neither gate has any use for.
fn world_with_two_tracked_zombies(feet: glam::Vec3) -> World {
    let mut app = App::new();
    app.add_plugins((IngestPlugin, EntityInterpPlugin));
    let mut world = std::mem::take(app.world_mut());

    for id in [1, 2] {
        world
            .resource_mut::<IngestQueue>()
            .push(ClientEvent::EntitySpawned {
                entity_id: id,
                uuid: None,
                entity_type: "minecraft:zombie".parse().expect("valid entity type key"),
                pos: ModelVec3::new(f64::from(feet.x), f64::from(feet.y), f64::from(feet.z)),
                rotation: Rotation::new(0.0, 0.0),
                velocity: None,
            });
        world.run_schedule(NetIngest);
    }

    // The entities are already fully described by the ingest
    // components `apply_entity_spawn` just wrote (`EntityKind`/`Position`/
    // `Rotation`/`HeadYaw`), so the fold reads those directly rather than a
    // hand-built `EntitySnapshot` — same as `Sim::fold_entities` does live.
    fold_entities(&mut world);
    world
}

fn draw_for(world: &World, id: i32) -> EntityDraw {
    extracted_entity_draws(world)
        .into_iter()
        .find(|d| d.id == id)
        .unwrap_or_else(|| panic!("entity {id} not among the extracted draws"))
}

/// A set of pixel indices plus its bounding box, printed rather than reduced to
/// a percentage (`CLAUDE.md`: measure by location, never by frame average).
#[derive(Debug)]
struct Region {
    px: Vec<usize>,
    min_x: u32,
    max_x: u32,
    min_y: u32,
    max_y: u32,
}

impl Region {
    fn from(mut hit: impl FnMut(usize) -> bool) -> Self {
        let mut r = Region {
            px: Vec::new(),
            min_x: u32::MAX,
            max_x: 0,
            min_y: u32::MAX,
            max_y: 0,
        };
        for y in 0..H {
            for x in 0..W {
                let i = (y * W + x) as usize;
                if hit(i) {
                    r.px.push(i);
                    r.min_x = r.min_x.min(x);
                    r.max_x = r.max_x.max(x);
                    r.min_y = r.min_y.min(y);
                    r.max_y = r.max_y.max(y);
                }
            }
        }
        r
    }

    fn len(&self) -> usize {
        self.px.len()
    }

    /// Whether pixel `i` is in this region. `px` is built in raster order, so
    /// this is a binary search rather than a set.
    fn contains(&self, i: usize) -> bool {
        self.px.binary_search(&i).is_ok()
    }

    /// The bounding box, as text — `"empty"` when nothing matched. This is what
    /// a failing assertion prints: a box says *where*, a count alone cannot
    /// distinguish a localised blob from noise scattered over the frame.
    fn bbox(&self) -> String {
        if self.px.is_empty() {
            "empty".to_owned()
        } else {
            format!(
                "x[{}..{}] y[{}..{}], {} px",
                self.min_x,
                self.max_x,
                self.min_y,
                self.max_y,
                self.px.len()
            )
        }
    }
}

fn rgb(px: &[u8], i: usize) -> [i32; 3] {
    [
        i32::from(px[i * 4]),
        i32::from(px[i * 4 + 1]),
        i32::from(px[i * 4 + 2]),
    ]
}

/// Squared distance to vanilla's overlay colour. The reddening predicate is
/// "this decreased", which needs no expected pixel value of its own.
fn dist_to_overlay_sq(c: [i32; 3]) -> i32 {
    (0..3)
        .map(|k| (c[k] - VANILLA_OVERLAY_RGB[k]).pow(2))
        .sum()
}

/// Whether `b` moved meaningfully toward vanilla's overlay red relative to `a`.
/// The margin keeps rounding noise out; it is a distance in the same units as
/// the channel bytes, not a tuned magic number.
fn reddened(a: [i32; 3], b: [i32; 3]) -> bool {
    dist_to_overlay_sq(b) + 400 < dist_to_overlay_sq(a)
}

/// A channel byte from the `Rgba8Unorm` target, back to the gamma-encoded value
/// the shader was blending.
///
/// The target is `Rgba8Unorm`, not `…Srgb`, so its bytes are the shader's
/// post-`srgb_to_linear` output. The magnitude check has to compare in the space
/// the blend happened in, which is gamma — see `CLAUDE.md`: vanilla is not
/// colour-managed and tint, shade and this overlay all multiply in gamma bytes.
/// This is the standard sRGB EOTF inverse, matching the shader's own pair.
fn linear_byte_to_gamma(byte: i32) -> f64 {
    let linear = f64::from(byte) / 255.0;
    if linear <= 0.003_130_8 {
        linear * 12.92
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    }
}

fn differs(a: &[u8], b: &[u8], i: usize) -> bool {
    let (p, q) = (rgb(a, i), rgb(b, i));
    (0..3).map(|k| (p[k] - q[k]).abs()).sum::<i32>() > 8
}

#[test]
#[ignore = "requires a GPU adapter"]
fn a_hurt_remote_entity_reddens_and_an_undamaged_one_does_not() {
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

    // Same fixture shape as the sibling entity gates: camera at the origin, mob
    // a few blocks south.
    let feet = glam::Vec3::new(0.0, 0.0, 4.0);
    let mut world = world_with_two_tracked_zombies(feet);
    world.run_schedule(Extract);

    let subject_rest = draw_for(&world, 1);
    let control_rest = draw_for(&world, 2);
    assert!(
        !subject_rest.hurt && !control_rest.hurt,
        "before any damage event, EntityDraw::hurt must be false for both entities \
         (subject={}, control={})",
        subject_rest.hurt,
        control_rest.hurt
    );

    // The one real event: only entity 1 ever reports a hurt animation. No
    // `GameTick` runs afterwards, deliberately — `HurtTime` is inserted during
    // `NetIngest`, so `Extract` can already see it, and skipping the tick keeps
    // every pose input bit-identical between the two extractions.
    world
        .resource_mut::<IngestQueue>()
        .push(ClientEvent::EntityHurtAnimation {
            entity_id: 1,
            yaw: 0.0,
        });
    world.run_schedule(NetIngest);
    world.run_schedule(Extract);

    let subject_hurt = draw_for(&world, 1);
    let control_hurt = draw_for(&world, 2);

    eprintln!("=== HURT OVERLAY PRODUCTION-PATH PIXEL GATE (#98) ===");
    eprintln!("overlay alpha byte: {HURT_OVERLAY_ALPHA_BYTE} (vanilla OverlayTexture red row)");
    eprintln!(
        "EntityDraw::hurt  subject: {} -> {} | control: {} -> {}",
        subject_rest.hurt, subject_hurt.hurt, control_rest.hurt, control_hurt.hurt
    );

    assert!(
        subject_hurt.hurt,
        "EntityHurtAnimation reached ingest but EntityDraw::hurt is still false — either \
         ingest::handles_event dropped the event or extract_entity_draws is not reading \
         HurtTime. This is issue #98's island exactly."
    );
    assert!(
        !control_hurt.hurt,
        "an entity that never received a damage event must never gain the overlay — \
         if this fails, the ingest system is not filtering by entity id"
    );

    // Premise 3: the flag is the *only* difference in the production data, so
    // the pixel diff below has exactly one possible cause.
    assert_eq!(
        EntityDraw {
            hurt: subject_rest.hurt,
            block_state: None,
            ..subject_hurt.clone()
        },
        subject_rest,
        "subject_rest and subject_hurt differ in more than `hurt` — something else about \
         the world moved between the two extractions, so no pixel difference can be \
         attributed to the overlay"
    );
    assert_eq!(
        control_hurt, control_rest,
        "the control's own extracted draw changed across the event"
    );

    let mut shoot = |draws: &[EntityDraw]| -> (Vec<u8>, usize) {
        let frame = target.acquire().expect("headless acquire");
        let stats = state.render(device, queue, frame.view(), &cam, None, draws);
        (target.read_texels(device, queue), stats.entities_drawn)
    };

    let (empty_px, empty_drawn) = shoot(&[]);
    let (subject_rest_px, n1) = shoot(std::slice::from_ref(&subject_rest));
    let (subject_hurt_px, n2) = shoot(std::slice::from_ref(&subject_hurt));
    let (control_rest_px, n3) = shoot(std::slice::from_ref(&control_rest));
    let (control_hurt_px, n4) = shoot(std::slice::from_ref(&control_hurt));

    // Negative control 2: the post-event draw with the flag forced off.
    let forced_off = EntityDraw {
        hurt: false,
        block_state: None,
        ..subject_hurt.clone()
    };
    let (forced_off_px, n5) = shoot(std::slice::from_ref(&forced_off));
    // Premise 4: the control's own pixels with the flag forced on.
    let forced_on = EntityDraw {
        hurt: true,
        block_state: None,
        ..control_hurt.clone()
    };
    let (forced_on_px, n6) = shoot(std::slice::from_ref(&forced_on));

    assert_eq!(
        empty_drawn, 0,
        "the entity-less frame drew {empty_drawn} entities — it is meant to be the \
         silhouette baseline"
    );
    for (label, n) in [
        ("subject rest", n1),
        ("subject hurt", n2),
        ("control rest", n3),
        ("control hurt", n4),
        ("forced off", n5),
        ("forced on", n6),
    ] {
        assert_eq!(
            n, 1,
            "{label}: entities_drawn={n} — the zombie failed to reach the entity pipeline, \
             which would make this gate measure the absence of an entity rather than the \
             absence of an overlay"
        );
    }

    // Premise 1: the zombie is a real run of pixels, not a transparent
    // placeholder that would make everything below vacuous.
    let mask = Region::from(|i| differs(&subject_rest_px, &empty_px, i));
    eprintln!("zombie silhouette (rest vs no entities): {}", mask.bbox());
    assert!(
        mask.len() > 500,
        "the zombie covers only {} px ({}) — too little to be a drawn mob, so this gate \
         would be measuring nothing",
        mask.len(),
        mask.bbox()
    );

    // Premise 2: nothing in the frame is already red-dominant before the
    // overlay exists, so redness in a diff can only come from the overlay.
    let sky = SKY_COLOR.map(|c| (c * 255.0).round() as i32);
    let already_red = Region::from(|i| {
        let c = rgb(&empty_px, i);
        c[0] > 128 && c[0] > c[1] * 2 && c[0] > c[2] * 2
    });
    eprintln!(
        "sky bytes: {sky:?}; already-red pixels in the entity-less frame: {}",
        already_red.bbox()
    );
    assert_eq!(
        already_red.len(),
        0,
        "the entity-less frame already contains red-dominant pixels at {} — a red-tint \
         gate cannot attribute redness to the overlay if something else paints red here",
        already_red.bbox()
    );

    // ---- the subject ----
    let reddened_px =
        Region::from(|i| reddened(rgb(&subject_rest_px, i), rgb(&subject_hurt_px, i)));
    eprintln!("reddened by the overlay: {}", reddened_px.bbox());
    assert!(
        reddened_px.len() * 2 > mask.len(),
        "only {} of the zombie's {} silhouette pixels moved toward vanilla's overlay red \
         ({}) — the overlay is not reaching the mob's own model",
        reddened_px.len(),
        mask.len(),
        reddened_px.bbox()
    );

    // ---- magnitude, not just direction ----
    //
    // Everything above this point measures *whether* the overlay applied. None of
    // it can see *how much*, and that is how the swapped `mix` shipped: the gate
    // reported 3440/3440 reddened while the shader rendered ~70% red where vanilla
    // renders ~30%. `reddened()` is satisfied by both.
    //
    // Green is the channel that makes this cheap and unambiguous. Vanilla's
    // overlay green is **0**, so in gamma space the blend is a pure scaling with
    // no additive term:
    //
    //   correct  (`mix(red, shaded, a)`):  G_out = a       * G_in  = 0.698 * G_in
    //   swapped  (`mix(shaded, red, a)`):  G_out = (1 - a) * G_in  = 0.302 * G_in
    //
    // Both hypotheses are computed and the measurement must land on vanilla's.
    // Expressed as a ratio rather than an absolute, so it needs no knowledge of
    // the mob's own texel colours — the expected value comes from vanilla's
    // formula and the 178 constant, never from a pixel we rendered.
    //
    // Measured in **gamma** space because that is where the shader blends; the
    // target is `Rgba8Unorm`, so its bytes are linear and have to be converted
    // back. Only pixels with enough signal to give a stable ratio are used.
    let alpha = f64::from(HURT_OVERLAY_ALPHA_BYTE) / 255.0;
    let mut ratios: Vec<f64> = Vec::new();
    for i in 0..(W as usize * H as usize) {
        if !mask.contains(i) {
            continue;
        }
        let g_rest = linear_byte_to_gamma(rgb(&subject_rest_px, i)[1]);
        let g_hurt = linear_byte_to_gamma(rgb(&subject_hurt_px, i)[1]);
        if g_rest > 0.15 {
            ratios.push(g_hurt / g_rest);
        }
    }
    assert!(
        ratios.len() > 100,
        "only {} silhouette pixels had enough green signal to measure a ratio — this \
         magnitude check would be reading noise",
        ratios.len()
    );
    ratios.sort_by(|a, b| a.partial_cmp(b).expect("no NaN ratios"));
    let measured = ratios[ratios.len() / 2];
    let correct = alpha;
    let swapped = 1.0 - alpha;
    eprintln!(
        "green retention over {} px: measured {measured:.4} | vanilla {correct:.4} | \
         swapped-args {swapped:.4}",
        ratios.len()
    );
    assert!(
        (measured - correct).abs() < (measured - swapped).abs(),
        "green retention is {measured:.4}, closer to the swapped-argument prediction \
         {swapped:.4} than to vanilla's {correct:.4} — `mix`'s first two arguments are \
         the wrong way round in the entity shader. Vanilla's entity.fsh is \
         `mix(overlayColor.rgb, color.rgb, overlayColor.a)`, so the alpha weights the \
         entity's own colour, not the red. This is issue #371."
    );
    assert!(
        (measured - correct).abs() < 0.06,
        "green retention is {measured:.4} but vanilla's blend predicts {correct:.4} — the \
         overlay is applying at the wrong strength even though the arguments are in the \
         right order (fog contamination, a linear-space blend, or a changed alpha)"
    );

    // A *model* overlay, not the full-screen tint that fix's title asked for:
    // nothing outside the mob's own silhouette may move at all.
    let leaked = Region::from(|i| {
        differs(&subject_rest_px, &subject_hurt_px, i) && !mask.contains(i)
    });
    assert_eq!(
        leaked.len(),
        0,
        "the overlay changed {} pixels outside the zombie's silhouette at {} — vanilla's \
         hurt overlay is a per-model blend, and a leak here would mean it had become a \
         de-facto screen-space tint",
        leaked.len(),
        leaked.bbox()
    );

    // Direction, per channel and per location: a blend toward `(255, 0, 0)` can
    // only raise red and lower green and blue. A global gamma or exposure slip
    // would move all three the same way and is caught here.
    let wrong_direction = Region::from(|i| {
        if !mask.contains(i) {
            return false;
        }
        let (a, b) = (rgb(&subject_rest_px, i), rgb(&subject_hurt_px, i));
        b[0] + 2 < a[0] || b[1] > a[1] + 2 || b[2] > a[2] + 2
    });
    assert_eq!(
        wrong_direction.len(),
        0,
        "{} silhouette pixels moved the wrong way for a blend toward (255,0,0) at {} — \
         red fell or green/blue rose, which a mix toward pure red cannot do",
        wrong_direction.len(),
        wrong_direction.bbox()
    );

    // ---- negative control 1: no event at all ----
    let control_moved = Region::from(|i| differs(&control_rest_px, &control_hurt_px, i));
    let control_reddened =
        Region::from(|i| reddened(rgb(&control_rest_px, i), rgb(&control_hurt_px, i)));
    eprintln!(
        "control (never damaged): moved {}, reddened {}",
        control_moved.bbox(),
        control_reddened.bbox()
    );
    assert_eq!(
        control_reddened.len(),
        0,
        "the undamaged control reddened at {} — the overlay is not keyed on the entity \
         that was actually hit",
        control_reddened.bbox()
    );
    assert_eq!(
        control_moved.len(),
        0,
        "the undamaged control's frames differ at {} across an event it never received",
        control_moved.bbox()
    );

    // ---- negative control 2: the same draw with the flag forced off ----
    let forced_off_moved = Region::from(|i| differs(&subject_rest_px, &forced_off_px, i));
    eprintln!(
        "flag forced off vs rest: {} (must be empty)",
        forced_off_moved.bbox()
    );
    assert_eq!(
        forced_off_moved.len(),
        0,
        "the post-event draw with `hurt` forced false differs from the pre-event frame at \
         {} — then the {} reddened pixels above are not attributable to the flag alone",
        forced_off_moved.bbox(),
        reddened_px.len()
    );

    // ---- premise 4: the control's rect is not dead ----
    let forced_on_reddened =
        Region::from(|i| reddened(rgb(&control_rest_px, i), rgb(&forced_on_px, i)));
    eprintln!(
        "control with the flag forced on: reddened {} (must be non-empty)",
        forced_on_reddened.bbox()
    );
    assert!(
        forced_on_reddened.len() * 2 > mask.len(),
        "forcing `hurt` on for the control reddened only {} px ({}) — so the control's \
         silence above is evidence about its rect being dead, not about it never having \
         been damaged",
        forced_on_reddened.len(),
        forced_on_reddened.bbox()
    );
}
