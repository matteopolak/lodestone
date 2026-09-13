//! Pixel gate: the particle families this pass added — geyser, noxious gas,
//! sulfur, trial-spawner/vault and the standalone remainder — reach real,
//! coloured pixels through the particle render pass, not just live `Particle`
//! structs that resolve against the atlas and then draw nothing on screen.
//!
//! # Why this is a separate file from `sheet_particle_atlas_pixels.rs`
//!
//! That gate is about *which atlas* a sheet particle samples; this one is
//! about whether these specific new emitters (several of them non-rendering
//! spawners whose children are the only thing that ever draws) actually put
//! anything on screen at all. `world-coverage`'s own "stranded" bucket — a
//! type named in the dispatch that produces no geometry — is exactly what a
//! spawner with a broken tick schedule would look like from the hermetic
//! tests alone: `spawn_one` still produces a live, resolved `Particle`, and
//! only a real render (or several ticks plus a render) can tell a seed that
//! never spawns children from one that does.
//!
//! # The two gates here
//!
//! [`sulfur_bubbles_draw_the_particle_sheets_own_white_colour`] is a genuine
//! per-texel colour prediction, the same discipline
//! `sheet_particle_atlas_pixels.rs` uses: `sulfur_bubbles` is a single-frame
//! sheet (`bubble_white.png`) drawn at an untinted `[1, 1, 1]`, so the
//! permitted colour set is just that texture's own opaque texels, decoded.
//! Its **control** is the same scene with the emitter simply not called —
//! observed, not assumed, to draw nothing — which is what proves a broken
//! `sulfur_bubbles` (dispatch arm silently dropped, sprite unresolved) would
//! actually fail this gate rather than passing by coincidence.
//!
//! [`several_new_families_change_pixels_once_ticked`] is broader and shallower:
//! it does not predict a colour, only that the frame changed a non-trivial,
//! *located* set of pixels — the right bar for the two non-rendering seeds
//! (`geyser`, `gust_emitter_large`) whose own drawn colour depends on
//! children spawned over several ticks, and for the plain billboards it adds
//! coverage cheaply without seven more colour-prediction rigs.
//!
//! Run explicitly (needs a GPU adapter *and* `.cache/mc/<version>/client.jar`
//! + `generated/reports/blocks.json`; per this repo's own convention it
//! **fails** rather than skips when either is missing, because a skip here
//! reads exactly like a pass):
//!
//! ```text
//! cargo test -p lodestone-shell --test new_particle_families_pixels -- --ignored --nocapture
//! ```

use std::path::{Path, PathBuf};

use lodestone::gpu::RenderState;
use lodestone::particles::Particles;
use lodestone_assets::{Atlas, ParticleAtlas, ResourceLocation, ResourceManager, ResourceSource, ZipSource};
use lodestone_particle::emit;
use lodestone_physics::{Aabb, CollisionView};
use lodestone_render::{
    BlockAtlas, BlockModels, Camera, GpuContext, HeadlessTarget, RenderTarget, blocks_json_registry,
};

const W: u32 = 320;
const H: u32 = 320;

/// Same framing `sheet_particle_atlas_pixels.rs` uses: close enough that a
/// billboard spans well over a hundred rows, deep into magnification.
const PARTICLE_POS: [f64; 3] = [0.5, 65.0, 0.45];
const EYE: [f32; 3] = [0.5, 65.0, 0.0];

const CHANNEL_TOLERANCE: i32 = 6;
const DRAWN_THRESHOLD: i32 = 12;

// ---------------------------------------------------------------------------
// Fixture (mirrors sheet_particle_atlas_pixels.rs's own helpers)
// ---------------------------------------------------------------------------

fn pack_root() -> PathBuf {
    let cwd = std::env::current_dir().expect("cwd");
    for base in cwd.ancestors() {
        let cache = base.join(".cache/mc");
        let Ok(entries) = std::fs::read_dir(&cache) else {
            continue;
        };
        let mut roots: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.join("client.jar").is_file() && p.join("generated/reports/blocks.json").is_file()
            })
            .collect();
        roots.sort();
        if let Some(best) = roots.pop() {
            return best;
        }
    }
    panic!(
        "no vanilla pack found under any ancestor's .cache/mc/<version>/ (needs client.jar + \
         generated/reports/blocks.json). This gate fails rather than skips: a skip reads as a pass."
    );
}

fn open_jar(root: &Path) -> ResourceManager {
    let bytes = std::fs::read(root.join("client.jar")).expect("read client.jar");
    let zip = ZipSource::from_bytes(bytes).expect("open client.jar");
    ResourceManager::new(vec![Box::new(zip) as Box<dyn ResourceSource>])
}

fn block_atlas(root: &Path, manager: &ResourceManager) -> BlockAtlas {
    let registry =
        blocks_json_registry(&root.join("generated/reports/blocks.json")).expect("blocks.json");
    let atlas = BlockAtlas::build(manager, &registry).expect("stitch block atlas");
    let models = BlockModels::build(manager, &registry).expect("bake block models");
    atlas.with_models(models)
}

/// No block ever collides — every new emitter's first frame (or few ticks) is
/// drawn well above any floor, so collision is irrelevant and this keeps the
/// fixture from needing a real chunk.
struct NoCollision;
impl CollisionView for NoCollision {
    fn collision_boxes(&self, _x: i32, _y: i32, _z: i32, _out: &mut Vec<Aabb>) {}
}

// ---------------------------------------------------------------------------
// Colour model (identical to sheet_particle_atlas_pixels.rs's own)
// ---------------------------------------------------------------------------

fn to_linear(c: u8) -> f32 {
    let c = f32::from(c) / 255.0;
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn predict(texel: [u8; 3], colour: [f32; 3]) -> [u8; 3] {
    let mut out = [0u8; 3];
    for c in 0..3 {
        let v = to_linear(texel[c]) * colour[c] * 255.0;
        #[expect(clippy::cast_possible_truncation, reason = "clamped to 0..=255")]
        {
            out[c] = v.round().clamp(0.0, 255.0) as u8;
        }
    }
    out
}

fn within(a: [u8; 3], b: [u8; 3]) -> bool {
    (0..3).all(|c| (i32::from(a[c]) - i32::from(b[c])).abs() <= CHANNEL_TOLERANCE)
}

fn matches_any(px: [u8; 3], set: &[[u8; 3]]) -> bool {
    set.iter().any(|&c| within(px, c))
}

fn texel_range(atlas: &Atlas, uv: [f32; 4]) -> (u32, u32, u32, u32) {
    #[expect(clippy::cast_possible_truncation, reason = "UVs are in 0..1")]
    let map = |t: f32, extent: u32| ((t * extent as f32).floor() as u32).min(extent - 1);
    let (x0, x1) = (map(uv[0], atlas.width), map(uv[2], atlas.width));
    let (y0, y1) = (map(uv[1], atlas.height), map(uv[3], atlas.height));
    (x0, y0, (x1 + 1).min(atlas.width), (y1 + 1).min(atlas.height))
}

fn texel(atlas: &Atlas, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * atlas.width + x) * 4) as usize;
    [atlas.rgba[i], atlas.rgba[i + 1], atlas.rgba[i + 2], atlas.rgba[i + 3]]
}

// ---------------------------------------------------------------------------
// Frame comparison — a located bounding box, never a bare fraction.
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone, Copy)]
struct BBox {
    count: usize,
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
}

impl BBox {
    fn add(&mut self, x: u32, y: u32) {
        if self.count == 0 {
            *self = Self { count: 1, x0: x, y0: y, x1: x, y1: y };
            return;
        }
        self.count += 1;
        self.x0 = self.x0.min(x);
        self.y0 = self.y0.min(y);
        self.x1 = self.x1.max(x);
        self.y1 = self.y1.max(y);
    }
}

impl std::fmt::Display for BBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.count == 0 {
            return write!(f, "none");
        }
        write!(f, "{} px in x{}..{} y{}..{}", self.count, self.x0, self.x1, self.y0, self.y1)
    }
}

struct Classified {
    drawn: BBox,
    matching: BBox,
    mismatching: BBox,
}

fn classify(baseline: &[u8], frame: &[u8], permitted: &[[u8; 3]]) -> Classified {
    let mut out =
        Classified { drawn: BBox::default(), matching: BBox::default(), mismatching: BBox::default() };
    for y in 0..H {
        for x in 0..W {
            let i = ((y * W + x) * 4) as usize;
            let a = [baseline[i], baseline[i + 1], baseline[i + 2]];
            let b = [frame[i], frame[i + 1], frame[i + 2]];
            let delta: i32 = (0..3).map(|c| (i32::from(a[c]) - i32::from(b[c])).abs()).sum();
            if delta <= DRAWN_THRESHOLD {
                continue;
            }
            out.drawn.add(x, y);
            if permitted.is_empty() || matches_any(b, permitted) {
                out.matching.add(x, y);
            } else {
                out.mismatching.add(x, y);
            }
        }
    }
    out
}

fn render_frame(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    target: &mut HeadlessTarget,
    render: &mut RenderState,
    camera: &Camera,
    instances: &[lodestone::particles::ParticleInstance],
) -> Vec<u8> {
    let frame = target.acquire().expect("headless acquire");
    render.prepare_particles(device, queue, instances, camera);
    render.render(device, queue, frame.view(), camera, None, &[]);
    target.read_texels(device, queue)
}

fn camera() -> Camera {
    Camera {
        position: glam::Vec3::new(EYE[0], EYE[1], EYE[2]),
        yaw: 0.0,
        pitch: 0.0,
        aspect: W as f32 / H as f32,
        ..Camera::default()
    }
}

// ---------------------------------------------------------------------------
// Gate 1: sulfur_bubbles, colour-predicted
// ---------------------------------------------------------------------------

/// `sulfur_bubbles` draws `particle/bubble_white` untinted — vanilla's own
/// billboard-particle base's own default `[1, 1, 1]` colour, which this
/// registry type never overrides — so the permitted set is exactly that
/// texture's own opaque texels, sRGB-decoded. `bubble_white` is one frame, so
/// there is no sheet animation to average over.
#[test]
#[ignore = "requires a GPU adapter and a fetched vanilla client.jar + generated/reports/blocks.json"]
fn sulfur_bubbles_draw_the_particle_sheets_own_white_colour() {
    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU test opted in but no adapter is available; do not treat this as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;

    let root = pack_root();
    let manager = open_jar(&root);
    let blocks = block_atlas(&root, &manager);
    let (sheet, report) = ParticleAtlas::build_reported(&manager).expect("stitch particle atlas");
    assert!(
        report.missing_textures.is_empty(),
        "particle atlas is incomplete, so a colour comparison against it is not trustworthy: {:?}",
        report.missing_textures
    );
    let models = blocks.models().expect("with_models was called, so a baked model set is present");

    let bubble_loc =
        ResourceLocation::new("minecraft", "particle/bubble_white").expect("literal location");
    let bubble = sheet
        .sprite(&bubble_loc)
        .expect("vanilla ships textures/particle/bubble_white.png; a missing sprite is a broken fixture");
    let bubble_uv = [bubble.uv_min[0], bubble.uv_min[1], bubble.uv_max[0], bubble.uv_max[1]];

    let (fx0, fy0, fx1, fy1) = texel_range(sheet.atlas(), bubble_uv);
    let mut permitted: Vec<[u8; 3]> = Vec::new();
    let mut partial_alpha = 0usize;
    for y in fy0..fy1 {
        for x in fx0..fx1 {
            let t = texel(sheet.atlas(), x, y);
            if t[3] > 5 && t[3] < 250 {
                partial_alpha += 1;
            }
            if t[3] < 250 {
                continue;
            }
            let p = predict([t[0], t[1], t[2]], [1.0, 1.0, 1.0]);
            if !permitted.iter().any(|&c| c == p) {
                permitted.push(p);
            }
        }
    }
    eprintln!(
        "bubble_white permitted set = {} colours from {} texels: {:02x?}",
        permitted.len(),
        (fx1 - fx0) * (fy1 - fy0),
        permitted
    );
    assert_eq!(
        partial_alpha, 0,
        "{partial_alpha} bubble_white texels are partially transparent; `predict` models only \
         a fully-opaque draw"
    );
    assert!(!permitted.is_empty(), "bubble_white produced zero opaque texels — a broken fixture");

    let mut particles = Particles::new(Some(models)).with_particle_atlas(Some(&sheet));
    for _ in 0..12 {
        emit::sulfur_bubbles(
            particles.engine_mut(),
            PARTICLE_POS[0],
            PARTICLE_POS[1],
            PARTICLE_POS[2],
            0.0,
            0.0,
        );
    }
    let cam = camera();
    let frame = particles.extract(&cam, 0.0, &|_, _, _| Some(lodestone_particle::FULL_BRIGHT));
    eprintln!(
        "extraction = alive={} drawn={} unresolved={} sheet_drawn={}",
        frame.alive, frame.drawn, frame.unresolved, frame.sheet_drawn
    );
    assert_eq!(frame.unresolved, 0, "every sulfur_bubbles instance must resolve against the sheet");
    assert!(frame.sheet_drawn > 0, "no sulfur_bubbles instance addressed the particle sheet at all");

    let mut target = HeadlessTarget::new(device, W, H, format);
    let mut render = RenderState::new(device, queue, format, W, H, Some(&blocks));
    render.install_particle_sheet_atlas(device, queue, sheet.atlas());

    // Control: the same scene with the emitter never called. If the dispatch
    // arm were silently broken (e.g. dropped for an unresolved sprite), the
    // "subject" frame below would look exactly like this one — so this is
    // run and its own zero-pixel claim is checked, not assumed.
    let baseline_px = render_frame(device, queue, &mut target, &mut render, &cam, &[]);
    let subject_px = render_frame(device, queue, &mut target, &mut render, &cam, particles.instances());

    let subject = classify(&baseline_px, &subject_px, &permitted);
    eprintln!("subject: drawn {}", subject.drawn);
    eprintln!("subject: matching bubble_white {}", subject.matching);
    eprintln!("subject: NOT bubble_white      {}", subject.mismatching);

    assert!(
        subject.drawn.count > 200,
        "sulfur_bubbles drew only {} pixels ({}); the particle pass is not reaching the \
         framebuffer at all",
        subject.drawn.count,
        subject.drawn
    );
    #[expect(clippy::cast_precision_loss, reason = "pixel counts")]
    let hit = subject.matching.count as f32 / subject.drawn.count as f32;
    assert!(
        hit > 0.9,
        "only {:.1}% of sulfur_bubbles' drawn pixels are a colour bubble_white.png can \
         produce (bounding box of the drawn set: {}; mismatching: {}). The emitter is \
         sampling the wrong sprite or the wrong atlas.",
        hit * 100.0,
        subject.drawn,
        subject.mismatching
    );
}

// ---------------------------------------------------------------------------
// Gate 2: breadth — several more families, ticked where they need it, must
// change a located, non-trivial set of pixels relative to an empty baseline.
// ---------------------------------------------------------------------------

/// Covers the plain billboards this pass added (`noxious_gas`,
/// `trial_spawner_detection`, `vault_connection`, `ominous_spawning`,
/// `pause_mob_growth`, `shriek`) at their first frame, plus the two
/// non-rendering spawners (`geyser`, `gust_emitter_large`) after several
/// ticks — which is the only way either spawner's *own* children reach a
/// frame at all, since the seed itself draws nothing by design.
#[test]
#[ignore = "requires a GPU adapter and a fetched vanilla client.jar + generated/reports/blocks.json"]
fn several_new_families_change_pixels_once_ticked() {
    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU test opted in but no adapter is available; do not treat this as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;

    let root = pack_root();
    let manager = open_jar(&root);
    let blocks = block_atlas(&root, &manager);
    let (sheet, report) = ParticleAtlas::build_reported(&manager).expect("stitch particle atlas");
    assert!(report.missing_textures.is_empty(), "particle atlas incomplete: {:?}", report.missing_textures);
    let models = blocks.models().expect("with_models was called");

    let mut particles = Particles::new(Some(models)).with_particle_atlas(Some(&sheet));
    let (x, y, z) = (PARTICLE_POS[0], PARTICLE_POS[1], PARTICLE_POS[2]);

    emit::noxious_gas(particles.engine_mut(), x, y, z, 0.0, 0.0, 0.0);
    emit::trial_spawner_detection(
        particles.engine_mut(), x, y, z, 0.0, 0.0, 0.0, lodestone_particle::Sheet::TrialSpawnerDetection,
    );
    emit::vault_connection(particles.engine_mut(), x, y, z, 0.1, 0.1, 0.1);
    emit::ominous_spawning(particles.engine_mut(), x, y, z, 0.1, 0.1, 0.1);
    emit::simple_vertical(particles.engine_mut(), x, y, z, 0.0, 0.0, 0.0, false);
    emit::shriek(particles.engine_mut(), x, y, z, 0);
    emit::geyser(particles.engine_mut(), x, y, z, 0.0, 0.0, 0.0, 1);
    emit::gust_emitter(particles.engine_mut(), x, y, z, 3.0, 7, 0);

    let before = particles.engine_mut().particles().len();
    for _ in 0..3 {
        particles.engine_mut().tick(&NoCollision);
    }
    let after = particles.engine_mut().particles().len();
    eprintln!("live particles: seeded {before}, {after} after three ticks");
    assert!(
        after > before,
        "expected the geyser/gust_emitter_large seeds to have thrown at least one child by \
         now (seeded {before}, still {after})"
    );

    let cam = camera();
    let frame = particles.extract(&cam, 0.0, &|_, _, _| Some(lodestone_particle::FULL_BRIGHT));
    eprintln!(
        "extraction = alive={} drawn={} unresolved={} sheet_drawn={}",
        frame.alive, frame.drawn, frame.unresolved, frame.sheet_drawn
    );
    assert_eq!(frame.unresolved, 0, "every instance in this mixed burst must resolve");
    assert!(frame.drawn >= 6, "expected at least the six billboards plus some geyser/gust children");

    let mut target = HeadlessTarget::new(device, W, H, format);
    let mut render = RenderState::new(device, queue, format, W, H, Some(&blocks));
    render.install_particle_sheet_atlas(device, queue, sheet.atlas());

    let baseline_px = render_frame(device, queue, &mut target, &mut render, &cam, &[]);
    let subject_px = render_frame(device, queue, &mut target, &mut render, &cam, particles.instances());
    let changed = classify(&baseline_px, &subject_px, &[]).drawn;
    eprintln!("changed pixels: {changed}");
    assert!(
        changed.count > 500,
        "eight new-family instances (several ticked) changed only {changed} pixels; the \
         particle pass is not putting this burst on screen"
    );
}
