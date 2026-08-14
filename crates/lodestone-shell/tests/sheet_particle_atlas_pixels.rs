//! Pixel gate: a **sheet** particle must be textured from the particle sheet,
//! not from the block atlas.
//!
//! # The bug, and why every existing gate was blind to it
//!
//! `Particles::sheet_uv` resolves `SpriteSource::Sheet` — flame, smoke, crits,
//! splashes — against `ParticleAtlas`, a **separate stitch** with its own
//! dimensions and its own packing. `gpu.rs` built exactly one particle bind
//! group, from the *block-model* atlas, and `ParticleRenderer::draw` bound only
//! that. So `/particle minecraft:flame` sampled block-atlas texels at
//! particle-sheet coordinates and drew fragments of arbitrary block textures.
//!
//! `tests/live_particles.rs` asserts `unresolved == 0` and **passes** — and it
//! is right to. The UVs genuinely do resolve; the assertion cannot see that they
//! resolve against the wrong image. `particles.rs`'s hermetic tests have the
//! same blind spot from the other side: they check a UV lands inside its
//! declared rect, which is true of both atlases. And `break_particles_pixels.rs`
//! renders the **demo palette**, which has no sheet particles at all.
//!
//! So a gate that can see this has to **sample colour** and compare it against
//! the particle sheet's own texels. That is what this one does.
//!
//! # The discriminator
//!
//! `textures/particle/flame.png` is 8×8 with exactly **22 opaque texels in four
//! colours** — `#ff0000`, `#ff6a00`, `#ffd800`, `#fff5c6` — every one of them
//! `R == 0xff` and strictly warm. Flame's particle colour is `[1, 1, 1]` and its
//! alpha is `1.0`, so at `FULL_BRIGHT` a drawn flame pixel is *exactly*
//! `255 · srgb_to_linear(texel)` in an `Rgba8Unorm` target — no blend with the
//! background, no tint, four permitted values. The expected set is derived from
//! the sprite at run time rather than hardcoded, so it cannot go stale against a
//! resource-pack change.
//!
//! The **control is the pre-fix renderer, reconstructed through public API**:
//! `install_particle_sheet_atlas` is handed the *block-model* atlas, which is
//! byte-for-byte the binding this file exists to have removed. It is executed on
//! every run and asserted to fail the subject's assertion — a control that
//! merely *would* fail is not evidence (`CLAUDE.md`).
//!
//! # Premises this checks rather than assumes
//!
//! `CLAUDE.md`: a control's premise can be false before the feature under test
//! ever existed, and that has happened four times here. So, in order:
//!
//! * **`sheet_drawn > 0`.** `drawn > 0` is satisfied by terrain debris; only
//!   this proves a *sheet* particle was in the frame at all.
//! * **Every flame texel is fully opaque or fully clear.** If any were partial,
//!   the drawn pixel would be a blend with the sky and the four-value prediction
//!   would be wrong. Measured, not assumed.
//! * **The two atlases genuinely disagree here.** The block-model atlas's texels
//!   over the *same* UV rect are counted against the permitted set; if the block
//!   atlas happened to be flame-coloured there, this whole gate would be
//!   measuring nothing, and it says so instead of passing.
//! * **The control drew something.** A control that draws zero pixels trivially
//!   "does not match the sheet" — the exact vacuity this test is about. Both
//!   frames must put a comparable number of pixels on screen.
//!
//! Every failure prints a **bounding box**, not a fraction: a percentage cannot
//! tell a uniform-but-wrong frame from a localised blob, and printing the box
//! has diagnosed two of this repo's four control-premise failures in one step.
//!
//! Run it explicitly (needs a GPU adapter *and* `.cache/mc/<version>/client.jar`
//! + `generated/reports/blocks.json`; per §12.52 it **fails** rather than skips
//! when either is missing, because a skip reads exactly like a pass):
//!
//! ```text
//! cargo test -p lodestone-shell --test sheet_particle_atlas_pixels -- --ignored --nocapture
//! ```

use std::path::{Path, PathBuf};

use lodestone::gpu::RenderState;
use lodestone::particles::Particles;
use lodestone_assets::{Atlas, ParticleAtlas, ResourceLocation, ResourceManager, ResourceSource, ZipSource};
use lodestone_render::{
    BlockAtlas, BlockModels, Camera, GpuContext, HeadlessTarget, RenderTarget, blocks_json_registry,
};

const W: u32 = 320;
const H: u32 = 320;

/// Where the flame billboards sit, and where the eye sits looking at them.
/// 0.45 blocks is close enough that a 0.1–0.2-block half-extent billboard spans
/// 100–200 of the 320 rows, i.e. ~12–25 screen pixels per sprite texel — well
/// into magnification, so the sampler's `mag_filter: Nearest` applies and each
/// drawn pixel is one whole texel rather than a mip blend.
const PARTICLE_POS: [f64; 3] = [0.5, 65.0, 0.45];
const EYE: [f32; 3] = [0.5, 65.0, 0.0];

/// Per-channel tolerance when matching a rendered pixel to a predicted texel.
/// The prediction is exact arithmetic; this only absorbs the GPU's sRGB decode
/// rounding.
const CHANNEL_TOLERANCE: i32 = 6;

/// A pixel counts as *drawn* when it moved this far (summed over RGB) from the
/// same frame rendered with no particles. Both frames come from a deterministic
/// renderer over an identical scene, so anything above trivial is real.
const DRAWN_THRESHOLD: i32 = 12;

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

/// Walk up for a pack root holding both files the atlases need, mirroring
/// `crate::resources::asset_root` (private) exactly as `break_particle_tint.rs`
/// does.
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

/// The live world's `BlockAtlas` **with models attached** — the shape
/// `RenderState::new` needs to build its model pass, whose atlas is the one the
/// particle pass binds for terrain debris. Without `with_models` there is no
/// `ModelRenderer` and the block half of the comparison would be the demo
/// world's packed cube atlas instead.
fn block_atlas(root: &Path, manager: &ResourceManager) -> BlockAtlas {
    let registry =
        blocks_json_registry(&root.join("generated/reports/blocks.json")).expect("blocks.json");
    let atlas = BlockAtlas::build(manager, &registry).expect("stitch block atlas");
    let models = BlockModels::build(manager, &registry).expect("bake block models");
    atlas.with_models(models)
}

// ---------------------------------------------------------------------------
// Colour model
// ---------------------------------------------------------------------------

/// sRGB byte -> linear, matching an `Rgba8UnormSrgb` texture fetch.
fn to_linear(c: u8) -> f32 {
    let c = f32::from(c) / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// What the particle shader writes for `texel` under `colour`, as the bytes an
/// **`Rgba8Unorm`** target reads back.
///
/// Deliberately *not* an sRGB target: with `Rgba8Unorm` the shader's linear
/// float is stored verbatim, so this is the whole transform — `texel * colour`
/// in linear space, per the fragment shader, then a scale to bytes. An sRGB
/// target would re-encode on write and add a second transform to model.
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

/// The texel range an absolute UV rect covers in `atlas`, as
/// `(x0, y0, x1, y1)` half-open in atlas pixels.
fn texel_range(atlas: &Atlas, uv: [f32; 4]) -> (u32, u32, u32, u32) {
    #[expect(clippy::cast_possible_truncation, reason = "UVs are in 0..1")]
    let map = |t: f32, extent: u32| ((t * extent as f32).floor() as u32).min(extent - 1);
    let (x0, x1) = (map(uv[0], atlas.width), map(uv[2], atlas.width));
    let (y0, y1) = (map(uv[1], atlas.height), map(uv[3], atlas.height));
    (x0, y0, (x1 + 1).min(atlas.width), (y1 + 1).min(atlas.height))
}

fn texel(atlas: &Atlas, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * atlas.width + x) * 4) as usize;
    [
        atlas.rgba[i],
        atlas.rgba[i + 1],
        atlas.rgba[i + 2],
        atlas.rgba[i + 3],
    ]
}

// ---------------------------------------------------------------------------
// Frame comparison
// ---------------------------------------------------------------------------

/// A set of pixel coordinates summarised by **where** they are, not just how
/// many. `CLAUDE.md`: a gate that reports only a fraction cannot tell a
/// uniform-but-wrong frame from a localised blob.
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
        write!(
            f,
            "{} px in x{}..{} y{}..{}",
            self.count, self.x0, self.x1, self.y0, self.y1
        )
    }
}

/// How one frame's particle pixels classify against the permitted sheet
/// colours, plus a census of what they actually were.
struct Classified {
    drawn: BBox,
    matching: BBox,
    mismatching: BBox,
    /// The most common colours among the mismatching pixels, for the failure
    /// message: "not flame" is far less useful than "it drew `#7a6a4f`".
    worst: Vec<([u8; 3], usize)>,
}

fn classify(baseline: &[u8], frame: &[u8], permitted: &[[u8; 3]]) -> Classified {
    let mut out = Classified {
        drawn: BBox::default(),
        matching: BBox::default(),
        mismatching: BBox::default(),
        worst: Vec::new(),
    };
    let mut census: std::collections::HashMap<[u8; 3], usize> = std::collections::HashMap::new();
    for y in 0..H {
        for x in 0..W {
            let i = ((y * W + x) * 4) as usize;
            let a = [baseline[i], baseline[i + 1], baseline[i + 2]];
            let b = [frame[i], frame[i + 1], frame[i + 2]];
            let delta: i32 = (0..3)
                .map(|c| (i32::from(a[c]) - i32::from(b[c])).abs())
                .sum();
            if delta <= DRAWN_THRESHOLD {
                continue;
            }
            out.drawn.add(x, y);
            if matches_any(b, permitted) {
                out.matching.add(x, y);
            } else {
                out.mismatching.add(x, y);
                *census.entry(b).or_default() += 1;
            }
        }
    }
    let mut worst: Vec<_> = census.into_iter().collect();
    worst.sort_by(|a, b| b.1.cmp(&a.1));
    worst.truncate(5);
    out.worst = worst;
    out
}

fn render_frame(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    target: &mut HeadlessTarget,
    render: &mut RenderState,
    camera: &Camera,
    instances: &[lodestone::particles::ParticleInstance],
) -> (Vec<u8>, lodestone::gpu::RenderStats) {
    let frame = target.acquire().expect("headless acquire");
    render.prepare_particles(device, queue, instances, camera);
    let stats = render.render(device, queue, frame.view(), camera, None, &[]);
    (target.read_texels(device, queue), stats)
}

// ---------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires a GPU adapter and a fetched vanilla client.jar + generated/reports/blocks.json"]
fn flame_particles_are_textured_from_the_particle_sheet_not_the_block_atlas() {
    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU test opted in but no adapter is available; do not treat this as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    // Unorm, not UnormSrgb: `predict` models exactly one transform (the sRGB
    // *decode* on texture fetch). An sRGB target would re-encode on write.
    let format = wgpu::TextureFormat::Rgba8Unorm;

    let root = pack_root();
    let manager = open_jar(&root);
    let blocks = block_atlas(&root, &manager);
    let (sheet, report) = ParticleAtlas::build_reported(&manager).expect("stitch particle atlas");
    assert!(
        report.missing_textures.is_empty(),
        "particle atlas is incomplete, so a colour comparison against it is not \
         trustworthy: {:?}",
        report.missing_textures
    );
    let models = blocks
        .models()
        .expect("with_models was called, so the baked model set must be present");
    let block_stitch = models.atlas();

    // ---- the permitted colour set, derived from the sprite -----------------
    let flame_loc = ResourceLocation::new("minecraft", "particle/flame").expect("literal location");
    let flame = sheet
        .sprite(&flame_loc)
        .expect("vanilla ships textures/particle/flame.png; a missing sprite is a broken fixture");
    let flame_uv = [
        flame.uv_min[0],
        flame.uv_min[1],
        flame.uv_max[0],
        flame.uv_max[1],
    ];
    eprintln!("=== sheet-particle atlas gate ===");
    eprintln!(
        "particle sheet   = {}x{}, definitions={} sprites={}",
        sheet.atlas().width,
        sheet.atlas().height,
        report.definitions,
        report.sprites
    );
    eprintln!(
        "block stitch     = {}x{}",
        block_stitch.width, block_stitch.height
    );
    eprintln!(
        "flame sprite     = {}x{} at ({},{}), uv {:?}",
        flame.width, flame.height, flame.x, flame.y, flame_uv
    );

    // ---- extract real flame instances --------------------------------------
    let mut particles = Particles::new(Some(models)).with_particle_atlas(Some(&sheet));
    for _ in 0..12 {
        lodestone_particle::emit::flame(
            particles.engine_mut(),
            PARTICLE_POS[0],
            PARTICLE_POS[1],
            PARTICLE_POS[2],
            0.0,
            0.0,
            0.0,
        );
    }
    let camera = Camera {
        position: glam::Vec3::new(EYE[0], EYE[1], EYE[2]),
        yaw: 0.0,
        pitch: 0.0,
        aspect: W as f32 / H as f32,
        ..Camera::default()
    };
    let frame = particles.extract(&camera, 0.0, &|_, _, _| {
        Some(lodestone_particle::FULL_BRIGHT)
    });
    eprintln!(
        "extraction       = alive={} drawn={} unresolved={} sheet_drawn={}",
        frame.alive, frame.drawn, frame.unresolved, frame.sheet_drawn
    );

    // Premise 1: a *sheet* particle is in this frame. `drawn > 0` alone is
    // satisfied by terrain debris, which samples the block atlas legitimately.
    assert_eq!(
        frame.unresolved, 0,
        "flame must resolve against the stitched sheet; unresolved particles draw nothing \
         and would make every pixel assertion below vacuous"
    );
    assert!(
        frame.sheet_drawn > 0,
        "no instance addresses the particle sheet, so this gate would be measuring \
         terrain debris (alive={}, drawn={})",
        frame.alive,
        frame.drawn
    );
    assert_eq!(
        frame.sheet_drawn, frame.drawn,
        "only flame was emitted, so every drawn instance must be a sheet instance"
    );

    // Every flame instance shares one colour (white × the full-bright shade),
    // read off the instances rather than assumed, because the prediction below
    // multiplies by it.
    let colour = instance_colour(&particles);
    eprintln!("instance colour  = {colour:?}");

    // ---- the permitted set, and the block atlas's answer at the same UVs ---
    let (fx0, fy0, fx1, fy1) = texel_range(sheet.atlas(), flame_uv);
    let mut permitted: Vec<[u8; 3]> = Vec::new();
    let mut partial_alpha = 0usize;
    for y in fy0..fy1 {
        for x in fx0..fx1 {
            let t = texel(sheet.atlas(), x, y);
            // Premise 2: fully opaque or fully clear. A partial texel would
            // blend with the sky and break the four-value prediction.
            if t[3] > 5 && t[3] < 250 {
                partial_alpha += 1;
            }
            if t[3] < 250 {
                continue;
            }
            let p = predict([t[0], t[1], t[2]], colour);
            if !permitted.iter().any(|&c| c == p) {
                permitted.push(p);
            }
        }
    }
    eprintln!(
        "permitted set    = {} colours from {} texels: {:02x?}",
        permitted.len(),
        (fx1 - fx0) * (fy1 - fy0),
        permitted
    );
    assert_eq!(
        partial_alpha, 0,
        "{partial_alpha} flame texels are partially transparent, so a drawn pixel is a \
         blend with the background and `predict` is the wrong model. Fix the model, do \
         not widen the tolerance."
    );
    assert!(
        permitted.len() >= 2,
        "a one-colour permitted set is a weak discriminator; flame has four"
    );

    // Premise 3: the two atlases actually disagree over this rect. If the block
    // atlas were flame-coloured here, the whole comparison would be measuring
    // nothing — and would pass.
    let (bx0, by0, bx1, by1) = texel_range(block_stitch, flame_uv);
    let (mut block_total, mut block_like_flame) = (0usize, 0usize);
    for y in by0..by1 {
        for x in bx0..bx1 {
            let t = texel(block_stitch, x, y);
            if t[3] < 250 {
                continue;
            }
            block_total += 1;
            if matches_any(predict([t[0], t[1], t[2]], colour), &permitted) {
                block_like_flame += 1;
            }
        }
    }
    #[expect(clippy::cast_precision_loss, reason = "counts are texel counts")]
    let block_overlap = block_like_flame as f32 / block_total.max(1) as f32;
    eprintln!(
        "block atlas over the same uv rect = {}x{} texels at ({bx0},{by0}); \
         {block_like_flame}/{block_total} ({:.2}%) would pass as flame",
        bx1 - bx0,
        by1 - by0,
        block_overlap * 100.0
    );
    assert!(
        block_total > 0,
        "the block atlas is fully transparent over flame's uv rect, so the pre-fix \
         renderer would have drawn nothing there and this gate cannot discriminate"
    );
    assert!(
        block_overlap < 0.25,
        "the block atlas is {:.1}% flame-coloured over flame's own uv rect, so \
         'the pixel looks like flame' does not distinguish the two atlases. Pick a \
         different sheet rather than trusting this result.",
        block_overlap * 100.0
    );

    // ---- subject and control ----------------------------------------------
    let mut target = HeadlessTarget::new(device, W, H, format);

    let mut subject = RenderState::new(device, queue, format, W, H, Some(&blocks));
    subject.install_particle_sheet_atlas(device, queue, sheet.atlas());
    assert!(subject.has_particle_sheet_atlas());

    // The control **is** the pre-fix renderer: the particle pass's sheet slot
    // bound to the block-model atlas, which is exactly what `gpu.rs` did before
    // That fix was fixed. Reconstructed through the same public API rather than
    // described, so it is executed and observed on every run.
    let mut control = RenderState::new(device, queue, format, W, H, Some(&blocks));
    control.install_particle_sheet_atlas(device, queue, block_stitch);

    let (baseline_px, baseline_stats) =
        render_frame(device, queue, &mut target, &mut subject, &camera, &[]);
    let (subject_px, subject_stats) = render_frame(
        device,
        queue,
        &mut target,
        &mut subject,
        &camera,
        particles.instances(),
    );
    let (control_px, control_stats) = render_frame(
        device,
        queue,
        &mut target,
        &mut control,
        &camera,
        particles.instances(),
    );

    eprintln!(
        "stats: baseline drawn={} sheet={} | subject drawn={} sheet={} bound={} | control drawn={} sheet={} bound={}",
        baseline_stats.particles_drawn,
        baseline_stats.particles_from_sheet,
        subject_stats.particles_drawn,
        subject_stats.particles_from_sheet,
        subject_stats.particle_sheet_atlas_bound,
        control_stats.particles_drawn,
        control_stats.particles_from_sheet,
        control_stats.particle_sheet_atlas_bound,
    );
    assert_eq!(
        baseline_stats.particles_drawn, 0,
        "the baseline frame must contain no particles, or 'drawn' below is not the \
         particles' own pixels"
    );
    assert_eq!(
        subject_stats.particles_from_sheet,
        frame.sheet_drawn,
        "the sheet instances extracted must be the sheet instances submitted"
    );

    let subject_class = classify(&baseline_px, &subject_px, &permitted);
    let control_class = classify(&baseline_px, &control_px, &permitted);

    eprintln!("subject: drawn {}", subject_class.drawn);
    eprintln!("subject: matching sheet {}", subject_class.matching);
    eprintln!("subject: NOT sheet      {}", subject_class.mismatching);
    for (c, n) in &subject_class.worst {
        eprintln!("         subject off-sheet colour #{:02x}{:02x}{:02x} × {n}", c[0], c[1], c[2]);
    }
    eprintln!("control: drawn {}", control_class.drawn);
    eprintln!("control: matching sheet {}", control_class.matching);
    eprintln!("control: NOT sheet      {}", control_class.mismatching);
    for (c, n) in &control_class.worst {
        eprintln!("         control block colour  #{:02x}{:02x}{:02x} × {n}", c[0], c[1], c[2]);
    }

    // Premise 4: both frames put pixels on screen. A control that drew nothing
    // would satisfy "does not look like flame" vacuously — the precise failure
    // mode this gate exists to rule out.
    assert!(
        subject_class.drawn.count > 500,
        "the subject drew only {} pixels; the particle pass is not reaching the \
         framebuffer at all and the colour comparison is meaningless",
        subject_class.drawn.count
    );
    assert!(
        control_class.drawn.count > 500,
        "the CONTROL drew only {} pixels, so 'the control does not look like flame' is \
         vacuous. The control must sample *something* from the block atlas — check that \
         install_particle_sheet_atlas(block_stitch) really rebinds.",
        control_class.drawn.count
    );

    #[expect(clippy::cast_precision_loss, reason = "pixel counts")]
    let subject_hit = subject_class.matching.count as f32 / subject_class.drawn.count as f32;
    #[expect(clippy::cast_precision_loss, reason = "pixel counts")]
    let control_hit = control_class.matching.count as f32 / control_class.drawn.count as f32;
    eprintln!(
        "sheet-colour agreement: subject {:.2}%  control {:.2}%",
        subject_hit * 100.0,
        control_hit * 100.0
    );

    assert!(
        subject_hit > 0.95,
        "only {:.1}% of the subject's drawn pixels are a colour flame.png can produce. \
         Off-sheet pixels: {}. The particle pass is sampling something other than the \
         particle sheet.",
        subject_hit * 100.0,
        subject_class.mismatching
    );
    assert!(
        control_hit < 0.25,
        "the control — the particle pass bound to the BLOCK atlas, i.e. the pre-fix \
         wiring — produced {:.1}% flame-coloured pixels, so this gate cannot tell the \
         two atlases apart and its pass above means nothing. Matching pixels: {}.",
        control_hit * 100.0,
        control_class.matching
    );
}

/// The instance colour every flame in this burst carries, asserted uniform.
///
/// Read out of the uploaded bytes rather than assumed to be `[1, 1, 1]`:
/// `predict` multiplies by it, so a `FlameParticle` that ever grew a tint or a
/// shade would silently invalidate the whole prediction. `ParticleInstance` is
/// `Pod` with layout `centre_size[0..4] uv[4..8] colour[8..12] roll[12..16]
/// atlas[16]`.
fn instance_colour(particles: &Particles) -> [f32; 3] {
    let mut seen: Option<[f32; 3]> = None;
    for inst in particles.instances() {
        let raw = bytemuck::bytes_of(inst);
        let f = |i: usize| f32::from_le_bytes(raw[i * 4..i * 4 + 4].try_into().unwrap());
        let colour = [f(8), f(9), f(10)];
        let alpha = f(11);
        assert!(
            (alpha - 1.0).abs() < 1e-6,
            "a flame instance has alpha {alpha}, so its pixels blend with the background \
             and `predict` is the wrong model"
        );
        match seen {
            None => seen = Some(colour),
            Some(prev) => assert!(
                (0..3).all(|c| (prev[c] - colour[c]).abs() < 1e-6),
                "flame instances disagree on colour ({prev:?} vs {colour:?}); the \
                 permitted set is derived from one of them"
            ),
        }
    }
    seen.expect("the burst produced no instances")
}
