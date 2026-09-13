//! Extends `distant_flat_terrain_holes.rs` to genuinely **uneven** terrain.
//! The flat gate cannot speak to chunk-to-chunk elevation changes because its
//! single-height world deliberately excludes them.
//!
//! # The invariant, generalised rather than reinvented
//!
//! The flat harness's detector (`find_sandwiched_background`, reused
//! unmodified here) asserts that within a single screen column, a
//! background-classified run can never reappear below a terrain-classified
//! one. Its own doc justifies that from flatness: a flat plane can only be
//! crossed once by a ray as pitch increases. **That argument generalises to
//! any heightfield with no overhangs, not just a flat one**, which is why
//! this file needs no new detector — only a world that actually varies in
//! height.
//!
//! The proof: fix the camera position and yaw (one screen column), and let
//! `H(d)` be the terrain height at horizontal distance `d` along that ray's
//! ground track — a well-defined function for any *heightfield* (single
//! solid-below/air-above height per horizontal position; a "staircase" of
//! flat tiers and vertical risers, exactly what this file builds, still
//! qualifies — `H` is a step function, still single-valued). The ray's own
//! height at distance `d` and pitch `θ` is `h(d, θ) = eye_y - d·tan(θ)`, a
//! *straight line through the eye* for fixed `θ`. First-hit distance is the
//! smallest `d` where `h(d, θ) ≤ H(d)`. As `θ` increases (steeper look-down),
//! `h(d, θ)` decreases for every fixed `d > 0` — the line pivots downward
//! around the eye — so if a ray at `θ₁` first hits at distance `D`
//! (`h(D, θ₁) = H(D)`), a steeper ray at `θ₂ > θ₁` satisfies
//! `h(D, θ₂) ≤ h(D, θ₁) = H(D)`, i.e. it has **already** crossed the terrain
//! by `D`. So first-hit distance is non-increasing in pitch, which is exactly
//! "once a column goes from sky to terrain, steeper rays (further down the
//! screen) never go back to sky" — **independent of whether `H` is constant**.
//! Overhangs are the one thing that breaks the proof (`H` stops being a
//! single-valued function of horizontal position), and this world has none.
//!
//! So on this world, exactly as on the flat one, any background run
//! reappearing below a terrain run in the same screen column is not a
//! legitimate cliff, ridge or ledge — every one of those is already excluded
//! by construction — it is a bug. What changes from the flat harness is only
//! that the *world* now has real elevation, so the three candidate causes
//! that ground themselves in unevenness specifically (a hole punched through
//! the interior of one riser face, a gap at a chunk seam where an elevation
//! step also lands, and depth-test discard/bias on a near-vertical face) can
//! actually be exercised. Missing geometry is excluded exactly as before —
//! every column loads before any section is meshed
//! (`ColumnSource::Complete`) — and mip/gutter sampling is excluded on the
//! same already-fixed basis `distant_flat_terrain_holes.rs` records.
//!
//! # The proof above is wrong for an off-centre column, and the residual gate failures are exactly that case
//!
//! Three passes of renderer suspicion (geometry inflation, 4x MSAA) both made
//! the aggregate hole count *worse*, and a raw pixel dump at the crux column
//! showed byte-identical pure sky for dozens of consecutive rows — no
//! sub-pixel blend, which a coverage artefact would produce. That sent the
//! search back to this file's own proof, and the proof has a hole in it.
//!
//! The proof says: "fix the camera position **and yaw** (one screen
//! column)," then varies only pitch and argues the ray's ground track is the
//! same line for every row. That is true when varying the *whole camera's*
//! pitch. It is **not** true when sweeping *screen rows within one rendered
//! frame at one fixed camera pose* — which is what a screen column actually
//! is. `Camera::basis`'s own closed form makes the gap visible: `right`'s
//! components never depend on pitch, but `up`'s do (`up.z = cos(yaw)*sin(pitch)`),
//! so a fixed-column, varying-row sweep changes not only the ray's vertical
//! angle but also its horizontal bearing — by an amount that is exactly zero
//! at the image's centre column (`ndc_x = 0`, so the `up`-borne term never
//! multiplies against a nonzero horizontal offset from `right`) and grows
//! with distance from centre. So the monotonic-first-hit argument holds
//! exactly on the centre column and is not proven anywhere else — and every
//! measured hole here sits off-centre (`x=106`/`540` out of a `320`-centre
//! frame, `x=54`, `x=592`), never on it.
//!
//! What that bearing drift does at a convex, chunk-seam-free corner: a row
//! near the top of a riser's silhouette can graze past the corner's own tip
//! into a real, unobstructed line of sight to farther, lower ground, then a
//! row further down the same column reacquires that farther ground (or a
//! *different* nearby riser) as solid again. That is a genuine
//! terrain→sky→terrain sequence in one column with **no missing geometry
//! anywhere** — legal grazing-corner sky, not a crack — and the original
//! proof's "no legitimate cliff/ridge/ledge" claim silently assumed the
//! centre-column case generalised. It does not.
//!
//! # The fix: an independent ray-cast oracle replaces "any hole is a bug"
//!
//! Verified two ways before touching this file: an offline Python transcription
//! of `Camera::basis`'s formula against this world's exact block layout (sharing
//! no code with the renderer or with this file) reproduced the renderer's own
//! reported hole rows almost exactly — the `x=54` config's `(54,80)-(54,111)`
//! sandwiched band predicted as rows 80–111, an *exact* match, and the `x=106`
//! band predicted as rows 187–219 against the renderer's reported 187–225,
//! matching at the near edge and within a few rows at the far one (march-step
//! resolution, not a disagreement in kind). Both predicted the exact same
//! world-space corner the renderer's own geometry reading named as the
//! culprit.
//!
//! So `oracle_ray_dir`/`oracle_ray_hits_solid` below port that check into the
//! gate itself, walking `World::block_state_at` directly — no mesher, no
//! rasteriser, no shader — using a ray direction transcribed (not called) from
//! `Camera::basis`'s documented closed form. **The invariant this file now
//! enforces is not "no sandwiched sky pixel exists" — it is "every sandwiched
//! sky pixel's own ray, cast independently against the real block data, agrees
//! that it should be sky."** A pixel where the renderer shows sky and the
//! oracle says the ray should have hit a block is still a hard failure; a pixel
//! where both agree is legal grazing-corner geometry inherent to a stepped
//! heightfield's silhouette, inherited from the very unevenness this file
//! exists to test, and is not one.
//!
//! `classify_holes` also has to know the renderer's own view-distance cull
//! (`lodestone_render::cull::within_view_distance`) — a hit the oracle finds
//! beyond that radius is exactly as legitimate as one behind a corner, because
//! the renderer is correctly not drawing it. `render_frame` used to leave
//! `RenderState` at its constructor default (`render_distance_chunks = 8`)
//! while the rest of this fixture (`RD_CHUNKS`, the camera's far clip) assumed
//! `10` — an unstated mismatch the oracle surfaced immediately as a wave of
//! false "genuine bug" reports at the loaded world's own far edge. Fixed by
//! calling `RenderState::set_fog` with `RD_CHUNKS` explicitly rather than
//! silently trusting the constructor default to agree with it.
//!
//! # What is left after both fixes: a real, much smaller, different defect
//!
//! With the corner-legality oracle and the view-distance fix both applied,
//! 213 of the original 215 flagged pixels across all six configs are
//! oracle-confirmed legal grazing-corner sky. **Two are not**, both in the
//! `pitch = 20°` ("steep down") config, at ordinary flat ground roughly 170
//! blocks out — nowhere near a riser or a corner. Each sits exactly one
//! screen row before the row where the renderer starts drawing that same
//! ground, and each flagged pixel is byte-identical to the no-terrain
//! reference (a hard miss, not a blend) — the same "not an antialiasing
//! artefact" signature the original corner investigation used to rule out a
//! coverage-blend explanation there.
//!
//! # A later pass discriminated further: not a cull bug, not a far-plane bug
//!
//! Three of this module doc's own four candidate causes are now ruled out
//! with direct measurements, not inference:
//!
//! - **Not CPU-side culling.** Instrumenting `TerrainCull::classify` directly
//!   (temporarily, reverted after) for the section that owns the failing
//!   pixel's oracle hit (`(52.56, 63.999996, 31.208405)`, section coord
//!   `(3, 3, 1)`) showed `CullVerdict::Visible` on the exact frame that
//!   renders the hole — not `Distance`, not `Frustum`, not `Occlusion`. The
//!   section is resident, its mesh is non-empty (`classify` is only reached
//!   after `section.mesh.as_ref()` succeeds), and it is pushed into the
//!   frame's draw list. Whatever drops this pixel happens after the draw
//!   call is issued, not before it.
//! - **Not the far plane.** `Camera::far_for_render_distance(10, 0)` is `640`
//!   blocks; the oracle hit is ~169 blocks out — a 3.8× margin, nowhere near
//!   the clip plane. `DEPTH_FORMAT` is `Depth32Float`, so 32-bit float
//!   precision is available at that fraction of the range; this is not a
//!   depth-buffer-precision collision either.
//! - **Row-independent, and that pins the mechanism.** `Camera::basis`'s
//!   closed form makes the *vertical* component of a ray's direction
//!   (`ray.y = -sin(pitch) + cos(pitch)·ndc_y·half_y`) depend only on screen
//!   row, never on column (`right.y` is always `0`, and `up.y = cos(pitch)`
//!   carries no yaw/column term) — an exact algebraic fact, not an
//!   approximation. So for flat ground, the distance to the terrain-height
//!   plane is a pure function of row, identical for every column. Measured:
//!   the terrain/sky transition sits at the *same* row (7 → 8) across five
//!   widely separated columns on both sides of the frame (`x = 91..95` and
//!   `x = 551..555`), confirming the boundary is a row effect, not a
//!   column-local one. Walking the oracle's own hits by row in that same
//!   column shows *each row* lands in a different, closer Z-chunk than the
//!   last (row 4 → chunk z = 8, row 5 → z = 5, row 6 → z = 3, row 7 → z = 1,
//!   row 8 → z = 0) — i.e. at this range and grazing angle, **one whole
//!   16-block chunk of world depth projects to under one screen pixel of
//!   height**. That is an inherent single-sample (no-MSAA) rasterisation
//!   regime, not a logic bug: a pixel-center sample test can legitimately
//!   miss a triangle whose true screen-space footprint is a fraction of a
//!   pixel tall, exactly the species this module doc's MSAA experiment
//!   already measured for the corner crack (made the aggregate *worse*, and
//!   the flagged pixel was a hard miss with no partial-coverage blend at
//!   4×). The two residual pixels only became *visible* as flagged holes
//!   because a nearby column's ring1-corner geometry happens to fill the
//!   identical screen-space band for neighbouring columns (`x = 94, 95`),
//!   masking the same underlying compression there; `x = 93` and `x = 553`
//!   are simply the columns where nothing else paints over the gap **and**
//!   the compressed row's oracle hit still happens to land inside the
//!   circular, chunk-quantised view-distance buffer.
//!
//! This reads as a one-row-late silhouette edge at a shallow, long-range
//! viewing angle over *ordinary* terrain — a different mechanism from the
//! corner crack in its trigger (distance compression, not a convex corner)
//! but the same mechanism in kind (a sub-pixel triangle footprint under
//! single-sample rasterisation), small enough (2 pixels out of 6 × 307,200)
//! that it was invisible inside the original 215-pixel aggregate. **Not
//! fixed in this pass**: the only remedies that would touch it (MSAA,
//! geometry inflation) are the same two this module doc already measured
//! against the corner crack — one made the aggregate worse, the other
//! touches shared, high-traffic render code — and neither is a targeted fix
//! for *this* pixel pair specifically. The gate below is left reporting it,
//! by name, rather than being loosened to hide it: an oracle-based gate that
//! cannot report an unexplained residual is not more trustworthy than the
//! aggregate one it replaced.
//!
//! # The world: a ziggurat, not a bump
//!
//! Three tiers, each a full section (16 blocks) taller than the last —
//! `LEVEL0_Y = 64` (the outer plain, identical to the flat harness's own
//! `SURFACE_Y`), `LEVEL1_Y = 80`, `LEVEL2_Y = 96` — nested square rings
//! centred on the world origin with half-widths `RING1_HALF = 32` and
//! `RING2_HALF = 16`. Both half-widths are chosen as **exact multiples of the
//! 16-block chunk width**, so every riser (the vertical step between tiers)
//! sits exactly on a chunk seam: this is deliberately the "gap along a chunk
//! seam where both sides are solid and coplanar" candidate the module doc
//! calls out, made unavoidable rather than incidental. Every riser height
//! (16 blocks) is also exactly one section, matching the flat harness's own
//! choice to land its solid/air interface on a *vertical* section boundary
//! too.
//!
//! The camera stands on the outer plain, offset from the ziggurat along `+Z`
//! at a **moderate distance** (matching the report's own framing and the
//! flat harness's own distance band) rather than adjacent to it, so both
//! risers are crossed by the same shallow, "walking around looking at the
//! horizon" rays the report describes — not a contrived close-up.
//!
//! ```text
//! cargo test -p lodestone-shell --test uneven_terrain_holes -- --ignored --nocapture
//! ```

use lodestone::gpu::{RenderState, SKY_COLOR, ThirdPersonBodyState};
use lodestone::mesher::{SectionGeometry, SectionKey, mesh_snapshot_models, snapshot_section, snapshot_visibility};
use lodestone::resources::BlockResources;
use lodestone_render::{
    Camera, GpuContext, HeadlessTarget, ModelMesh, RenderTarget, cull::within_view_distance,
    entity_anim::AnimInput, fog::FogSettings,
};
use lodestone_world::{ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind, World};

/// See `distant_flat_terrain_holes.rs`'s identical helper for why this exists
/// at all — `RenderState`'s unconditional first-person bare arm draws a
/// fixed-screen-space 29px rect regardless of camera angle, which reads as a
/// hole and is not one.
fn suppress_first_person_arm(state: &mut RenderState) {
    state.set_third_person_body_source(|| {
        Some(ThirdPersonBodyState {
            // No skin: this fixture installs a body to suppress the first-person
            // arm, not to assert a sheet. The draw falls back to the model's own
            // texture, exactly as it did before this field existed.
            player_skin: None,
            feet: glam::Vec3::new(0.0, -10_000.0, 0.0),
            body_yaw_deg: 0.0,
            anim: AnimInput::default(),
            scale: 1.0,
            swim_amount: 0.0,
            slim: false,
            equipment: Vec::new(),
            equipment_skin: Vec::new(),
        })
    });
}

const W: u32 = 640;
const H: u32 = 480;
const FOV_Y_DEGREES: f32 = 40.0;

/// Same render distance as the flat harness (10 chunks / 160 blocks).
const RD_CHUNKS: i32 = 10;
const MIN_Y: i32 = 0;

/// Tier heights — see the module doc for why each is a section boundary.
const LEVEL0_Y: i32 = 64;
const LEVEL1_Y: i32 = 80;
const LEVEL2_Y: i32 = 96;
/// Ring half-widths — exact multiples of the 16-block chunk width, so every
/// riser lands exactly on a chunk seam (see the module doc).
const RING1_HALF: i32 = 32;
const RING2_HALF: i32 = 16;

/// Sections per column: `LEVEL2_Y` (96) plus two sections (32 blocks) of
/// headroom, all measured from `MIN_Y`.
const SECTION_COUNT: usize = ((LEVEL2_Y - MIN_Y) / 16 + 2) as usize;

fn differs(subject: &[u8], reference: &[u8]) -> bool {
    let d = (i32::from(subject[0]) - i32::from(reference[0])).abs()
        + (i32::from(subject[1]) - i32::from(reference[1])).abs()
        + (i32::from(subject[2]) - i32::from(reference[2])).abs();
    d > DIFFERS_FROM_REFERENCE
}

/// See `distant_flat_terrain_holes.rs`'s identical constant's doc: derived
/// per-pixel against a no-terrain reference render of the same camera, never
/// a fixed sky colour.
const DIFFERS_FROM_REFERENCE: i32 = 60;

fn first_state_named(name: &str) -> u32 {
    (0..lodestone_data::block_states::STATE_COUNT)
        .find(|&id| lodestone_data::block_states::block_name(id) == Some(name))
        .unwrap_or_else(|| panic!("{name} is not in the 26.2 block-state table"))
}

#[derive(Debug, Clone, Copy)]
struct Rect {
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
}

fn grow(rect: Option<Rect>, x: u32, y: u32) -> Rect {
    match rect {
        None => Rect { x0: x, y0: y, x1: x, y1: y },
        Some(r) => Rect {
            x0: r.x0.min(x),
            y0: r.y0.min(y),
            x1: r.x1.max(x),
            y1: r.y1.max(y),
        },
    }
}

/// Identical to `distant_flat_terrain_holes.rs`'s function of the same name —
/// see its doc for the full argument. The module doc above is what changed:
/// the geometric justification now covers any heightfield, not just a flat
/// one, and this world actually exercises that generalisation. Also returns
/// every flagged pixel's own coordinates (not just the aggregate count/rect),
/// which the oracle check below needs to interrogate each one individually.
fn find_sandwiched_background(
    subject: &[u8],
    reference: &[u8],
    w: u32,
    h: u32,
) -> (usize, Option<Rect>, usize, Vec<(u32, u32)>) {
    let mut total = 0usize;
    let mut rect: Option<Rect> = None;
    let mut hole_columns = 0usize;
    let mut pixels = Vec::new();
    for x in 0..w {
        let mut seen_terrain = false;
        let mut column_has_hole = false;
        for y in 0..h {
            let idx = ((y * w + x) * 4) as usize;
            let sub = &subject[idx..idx + 4];
            let ref_px = &reference[idx..idx + 4];
            if differs(sub, ref_px) {
                seen_terrain = true;
            } else if seen_terrain {
                total += 1;
                column_has_hole = true;
                rect = Some(grow(rect, x, y));
                pixels.push((x, y));
            }
        }
        if column_has_hole {
            hole_columns += 1;
        }
    }
    (total, rect, hole_columns, pixels)
}

/// Independent ray-cast oracle for the flagged pixels above — see the module
/// doc's "The fix" section for why this exists and what it measured before
/// landing. Shares no code with the renderer, mesher or rasteriser: the ray
/// direction is transcribed (not called) from `Camera::basis`'s documented
/// closed form, and solidity comes straight from `World::block_state_at`.
fn oracle_ray_dir(camera: &Camera, px: u32, py: u32, w: u32, h: u32) -> glam::Vec3 {
    let (sy, cy) = camera.yaw.to_radians().sin_cos();
    let (sp, cp) = camera.pitch.to_radians().sin_cos();
    let right = glam::Vec3::new(-cy, 0.0, -sy);
    let up = glam::Vec3::new(-sy * sp, cp, cy * sp);
    let forward = glam::Vec3::new(-sy * cp, -sp, cy * cp);
    let half_y = (camera.fov_y_degrees.to_radians() * 0.5).tan();
    let half_x = half_y * camera.aspect;
    let ndc_x = 2.0 * (px as f32 + 0.5) / w as f32 - 1.0;
    let ndc_y = 1.0 - 2.0 * (py as f32 + 0.5) / h as f32;
    (forward + right * (ndc_x * half_x) + up * (ndc_y * half_y)).normalize()
}

/// Marches a ray through `world`'s real block data in small steps. A `None`
/// read (chunk not loaded, or `y` outside the column) is treated as air —
/// exactly what the renderer itself would show there. Returns the first
/// solid-hit world point, or `None` if the ray reaches `max_dist` clean.
fn oracle_ray_hits_solid(
    world: &World,
    air: u32,
    origin: glam::Vec3,
    dir: glam::Vec3,
    max_dist: f32,
) -> Option<glam::Vec3> {
    const STEP: f32 = 0.02;
    let mut t = 0.0f32;
    while t < max_dist {
        let p = origin + dir * t;
        let (bx, by, bz) = (p.x.floor() as i32, p.y.floor() as i32, p.z.floor() as i32);
        if let Some(state) = world.block_state_at(bx, by, bz) {
            if state != air {
                return Some(p);
            }
        }
        t += STEP;
    }
    None
}

/// Classifies every flagged pixel in `hole_pixels` by independently
/// ray-casting it: pixels the oracle also calls sky are legal grazing-corner
/// geometry (see the module doc); pixels the oracle says should have hit a
/// block are genuine renderer bugs — **unless that hit itself sits beyond the
/// renderer's own configured view distance**, in which case the renderer is
/// correctly applying the same circular cull
/// (`lodestone_render::cull::within_view_distance`, vanilla's own view
/// membership rule) that a real client applies, and finding nothing to draw
/// there is expected, not a defect. `render_distance_chunks` must match what
/// the frame under test was actually configured with (`render_frame` now
/// sets it explicitly — see that function's doc for why the default was
/// wrong here). Returns `(legitimate, genuine_bugs)`, the second as
/// `(x, y, world point the oracle hit)` for a failure message that names
/// exactly where to look.
fn classify_holes(
    world: &World,
    air: u32,
    camera: &Camera,
    render_distance_chunks: u32,
    hole_pixels: &[(u32, u32)],
    w: u32,
    h: u32,
) -> (usize, Vec<(u32, u32, glam::Vec3)>) {
    let camera_chunk = (
        (camera.position.x / 16.0).floor() as i32,
        (camera.position.z / 16.0).floor() as i32,
    );
    let mut legitimate = 0usize;
    let mut genuine_bugs = Vec::new();
    for &(x, y) in hole_pixels {
        let dir = oracle_ray_dir(camera, x, y, w, h);
        match oracle_ray_hits_solid(world, air, camera.position, dir, camera.far) {
            Some(hit) => {
                let hit_chunk = ((hit.x / 16.0).floor() as i32, (hit.z / 16.0).floor() as i32);
                if within_view_distance(camera_chunk, hit_chunk, render_distance_chunks) {
                    genuine_bugs.push((x, y, hit));
                } else {
                    legitimate += 1;
                }
            }
            None => legitimate += 1,
        }
    }
    (legitimate, genuine_bugs)
}

/// Build the ziggurat world described in the module doc: a flat `LEVEL0_Y`
/// plain everywhere, with two nested, chunk-aligned square rings raised to
/// `LEVEL1_Y` and `LEVEL2_Y`. Filled outermost-tier-first so each smaller,
/// taller fill only *adds* height within its own footprint — order-agnostic
/// correctness, see the module doc's construction note.
fn ziggurat_world(stone: u32, air: u32) -> World {
    let mut world = World::new();
    for cx in -RD_CHUNKS..=RD_CHUNKS {
        for cz in -RD_CHUNKS..=RD_CHUNKS {
            let column = ChunkColumn::new(
                MIN_Y,
                SECTION_COUNT,
                PaletteKind::block_states(),
                PaletteKind::biomes(),
                air,
                0,
            );
            let mut light = ColumnLight::new(SECTION_COUNT);
            for i in 0..light.light_section_count() {
                *light.sky_mut(i) = lodestone_world::LightData::Uniform(15);
                *light.block_mut(i) = lodestone_world::LightData::Uniform(0);
            }
            world.load(
                ChunkPos::new(cx, cz),
                LoadedChunk::new(column, light, Heightmaps::new(), Vec::new()),
            );
        }
    }

    let lo = -RD_CHUNKS * 16;
    let hi = RD_CHUNKS * 16 + 15;

    let base = world.fill_region([lo, MIN_Y, lo], [hi, LEVEL0_Y - 1, hi], stone);
    assert!(base > 0, "fixture: base tier must actually write blocks");

    let ring1 = world.fill_region(
        [-RING1_HALF, MIN_Y, -RING1_HALF],
        [RING1_HALF - 1, LEVEL1_Y - 1, RING1_HALF - 1],
        stone,
    );
    assert!(ring1 > 0, "fixture: ring1 tier must actually write blocks");

    let ring2 = world.fill_region(
        [-RING2_HALF, MIN_Y, -RING2_HALF],
        [RING2_HALF - 1, LEVEL2_Y - 1, RING2_HALF - 1],
        stone,
    );
    assert!(ring2 > 0, "fixture: ring2 tier must actually write blocks");

    world
}

#[allow(clippy::too_many_arguments)]
fn upload_all(
    world: &World,
    models: &lodestone_render::BlockModels,
    state: &mut RenderState,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    skip: Option<SectionKey>,
    upload_terrain: bool,
) -> usize {
    let mut uploaded = 0usize;
    if !upload_terrain {
        return uploaded;
    }
    for cx in -RD_CHUNKS..=RD_CHUNKS {
        for cz in -RD_CHUNKS..=RD_CHUNKS {
            for si in 0..SECTION_COUNT {
                let key = SectionKey { cx, cz, si, min_y: MIN_Y };
                if Some(key) == skip {
                    continue;
                }
                let Some(snap) = snapshot_section(world, key) else {
                    continue;
                };
                let opaque = mesh_snapshot_models(&snap, models, false);
                // See `distant_flat_terrain_holes.rs`'s identical comment: a
                // zero-quad section still needs uploading so it enters the
                // occlusion graph.
                let visibility = snapshot_visibility(&snap, models);
                let geometry = SectionGeometry::Model {
                    opaque,
                    water: ModelMesh::default(),
                    translucent_blocks: ModelMesh::default(),
                    visibility,
                };
                state.upload_section(device, queue, key, &geometry);
                uploaded += 1;
            }
        }
    }
    uploaded
}

/// See `distant_flat_terrain_holes.rs`'s identical constant's doc: the fade
/// clock defect this file was originally written to diagnose. `render_frame`
/// below advances past it exactly as the fixed flat harness does.
const FADE_COMPLETE_TICK: u64 = 200;

#[allow(clippy::too_many_arguments)]
fn render_frame(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    target: &mut HeadlessTarget,
    atlas: &lodestone_render::BlockAtlas,
    world: &World,
    models: &lodestone_render::BlockModels,
    camera: &Camera,
    skip: Option<SectionKey>,
    upload_terrain: bool,
) -> (Vec<u8>, lodestone::gpu::RenderStats) {
    let mut state = RenderState::new(device, queue, format, W, H, Some(atlas));
    suppress_first_person_arm(&mut state);
    // `RenderState::new` defaults `render_distance_chunks` to
    // `DEFAULT_RENDER_DISTANCE_CHUNKS` (8), not this fixture's own `RD_CHUNKS`
    // (10) — the world, and `camera_at`'s far clip, are both built assuming
    // 10. Left at the default, the renderer's own view-distance cull
    // (`TerrainCull`, vanilla's circular `within_view_distance`) silently
    // disagreed with what the rest of the fixture assumed was drawable, which
    // the oracle below caught: pixels near the render-distance edge looked
    // like "genuine bugs" only because the renderer was, correctly, applying
    // a *tighter* cull than the fixture's other constants implied.
    state.set_fog(
        FogSettings::for_render_distance(SKY_COLOR, RD_CHUNKS as u32),
        RD_CHUNKS as u32,
    );
    let uploaded = upload_all(world, models, &mut state, device, queue, skip, upload_terrain);
    assert!(
        !upload_terrain || uploaded > 0,
        "fixture: some sections must have uploaded when terrain is requested"
    );
    state.update_animation(queue, FADE_COMPLETE_TICK);
    let frame = target.acquire().expect("headless acquire");
    let stats = state.render(device, queue, frame.view(), camera, None, &[]);
    (target.read_texels(device, queue), stats)
}

/// Camera standing on the outer (`LEVEL0_Y`) plain, offset `STANDOFF_Z`
/// blocks from the ziggurat's centre along `-Z`, looking toward it (`yaw`
/// `0` in this engine points `+Z`, matching `distant_flat_terrain_holes.rs`'s
/// own convention). `STANDOFF_Z` puts the near riser (`RING1_HALF` = 32
/// blocks from centre) at a genuinely moderate distance —
/// `STANDOFF_Z - RING1_HALF` blocks away — rather than adjacent to the
/// camera, matching the report's "far away" framing.
const STANDOFF_Z: i32 = 130;

fn camera_at(pitch_degrees: f32, yaw_degrees: f32) -> Camera {
    Camera {
        position: glam::Vec3::new(0.5, LEVEL0_Y as f32 + 1.62, -(STANDOFF_Z as f32) + 0.5),
        yaw: yaw_degrees,
        pitch: pitch_degrees,
        fov_y_degrees: FOV_Y_DEGREES,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(RD_CHUNKS as u32, 0),
    }
}

fn gpu() -> GpuContext {
    GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    )
}

fn load_vanilla() -> (BlockResources, std::sync::Arc<lodestone_render::BlockAtlas>) {
    let resources = BlockResources::load(true);
    let atlas = resources.vanilla_atlas.clone().unwrap_or_else(|| {
        panic!(
            "vanilla assets did not load (banner: {:?}) — this gate needs a real \
             client.jar under .cache/mc/26.2 (LODESTONE_ASSETS)",
            resources.banner
        )
    });
    (resources, atlas)
}

/// The main gate: the ziggurat world, rendered from moderate distance at
/// several yaws (edge-on to a riser, corner-on where two risers meet, and
/// tangential/flat-only as a sanity repeat of the already-clean flat result)
/// and pitches (the report's own "same level as me" shallow look-down, plus
/// steep-down and slight-up controls). A sandwiched sky pixel is only a
/// failure if its own independent ray-cast (`classify_holes`, below) agrees
/// it should have hit a block — see the module doc's "The proof above is
/// wrong for an off-centre column" section for why the naive "any hole is a
/// bug" version of this gate was itself wrong on this world.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn uneven_terrain_at_moderate_distance_has_no_sky_holes() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let (_resources, atlas) = load_vanilla();
    let models = atlas.models().expect("vanilla atlas must carry baked models");

    let stone = first_state_named("minecraft:stone");
    let air = first_state_named("minecraft:air");
    let world = ziggurat_world(stone, air);

    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);

    // yaw 0: dead centre, edge-on to both risers (the primary reproduction
    // of the report). yaw ±14: still within the ziggurat's angular width at
    // this distance but off-centre enough to cross a *corner* of the square
    // rings — two perpendicular riser faces meeting, the sharpest local
    // geometry in the scene. yaw 90: tangential, sees only the flat plain —
    // a cheap repeat of the already-proven-clean flat result, now inside the
    // same world and camera rig as the uneven configs. Pitches: the report's
    // own shallow "same level as me" as the primary probe, plus a steep
    // look-down (control: well below eye level, crosses the near riser's
    // base) and a slight look-up (control: above eye level).
    let configs: &[(&str, f32, f32)] = &[
        ("centred on risers, level", 2.0, 0.0),
        ("ring corner, level", 2.0, 14.0),
        ("ring corner, level (other side)", 2.0, -14.0),
        ("tangential (flat only), level", 2.0, 90.0),
        ("centred on risers, steep down", 20.0, 0.0),
        ("centred on risers, slight up", -10.0, 0.0),
    ];

    let mut all_genuine_bugs: Vec<(&str, u32, u32, glam::Vec3)> = Vec::new();
    for &(label, pitch, yaw) in configs {
        let camera = camera_at(pitch, yaw);
        let (reference, _) = render_frame(
            device, queue, format, &mut target, &atlas, &world, models, &camera, None, false,
        );
        let (subject, stats) = render_frame(
            device, queue, format, &mut target, &atlas, &world, models, &camera, None, true,
        );

        // Sanity: the scene must actually show *something* other than flat
        // ground for the riser-facing configs, or this gate would be exactly
        // as vacuous as the original zero-pixel-readback state
        // `distant_flat_terrain_holes.rs` records. Count pixels that differ
        // from the reference at all (terrain of any tier, not just holes).
        let mut terrain_px = 0usize;
        for i in (0..subject.len()).step_by(4) {
            if differs(&subject[i..i + 4], &reference[i..i + 4]) {
                terrain_px += 1;
            }
        }

        let (hole_px, bbox, hole_cols, hole_pixels) =
            find_sandwiched_background(&subject, &reference, W, H);
        let (legitimate, genuine_bugs) =
            classify_holes(&world, air, &camera, RD_CHUNKS as u32, &hole_pixels, W, H);
        eprintln!(
            "=== {label} (pitch={pitch}, yaw={yaw}) ===\n\
             sections drawn        = {}\n\
             sections culled dist  = {}\n\
             sections culled frust = {}\n\
             sections culled occl  = {} (occlusion_active={})\n\
             terrain pixels        = {terrain_px} / {}\n\
             sandwiched-sky pixels = {hole_px}\n\
             sandwiched-sky columns= {hole_cols} / {W}\n\
             bounding box          = {bbox:?}\n\
             oracle-legitimate     = {legitimate} (ray genuinely reaches sky — grazing corner)\n\
             oracle-genuine bugs   = {} (ray should have hit a block)",
            stats.sections_drawn,
            stats.sections_culled_distance,
            stats.sections_culled_frustum,
            stats.sections_culled_occlusion,
            stats.occlusion_active,
            W * H,
            genuine_bugs.len(),
        );
        assert!(
            terrain_px > 0,
            "{label}: zero pixels differed from the no-terrain reference — this scene is \
             vacuous (see distant_flat_terrain_holes.rs's own zero-pixel-readback history) \
             and a clean sandwiched-sky result here would prove nothing"
        );
        for (x, y, hit) in genuine_bugs {
            all_genuine_bugs.push((label, x, y, hit));
        }
    }

    assert!(
        all_genuine_bugs.is_empty(),
        "the ziggurat world produced sky pixels sandwiched inside its own terrain silhouette \
         whose own independent ray-cast says the ray should have hit a block instead — this is \
         not the legal grazing-corner case the module doc's oracle exists to exonerate. \
         Mismatches (config, x, y, world point the oracle hit): {all_genuine_bugs:?}. \
         Every column was loaded before any section was meshed, so this cannot be the \
         streaming-neighbour-arrival cause. As measured at this file's last update: the residual \
         mismatches (two pixels, only in the steep-down config, at ordinary flat ground roughly \
         170 blocks out — not near any riser/corner) sit exactly one screen row before the row \
         where the renderer starts drawing that same distant ground, and the flagged pixel itself \
         is byte-identical to the no-terrain reference (a hard miss, not an antialiased blend). \
         Direct instrumentation of TerrainCull::classify confirmed the owning section is \
         CullVerdict::Visible on the failing frame (not culled by distance/frustum/occlusion), and \
         camera.far (640 blocks) is a 3.8x margin past the ~169-block hit, ruling out both a cull \
         bug and a far-plane clip. Camera::basis makes a ray's vertical component depend only on \
         screen row, never column, and at this range one whole 16-block chunk of world depth \
         measures under one screen pixel of height — an inherent single-sample (no-MSAA) \
         rasterisation limit, the same species already measured (and left unfixed) for the corner \
         crack, not a renderer logic bug. See the module doc's 'A later pass discriminated \
         further' section for the full measurement. Not chased further in this pass: the only \
         candidate remedies are the same two already measured against the corner crack (MSAA made \
         the aggregate worse; geometry inflation touches shared, high-traffic render code)."
    );
}

/// Control: does the detector actually fire on a real, known hole **in this
/// uneven scene**, not just the flat one? Skips uploading the surface section
/// of the column forming the near riser's exposed face (`RING1_HALF`,
/// `LEVEL1_Y`'s own surface section — the vertical wall a viewer walking up
/// to it would see) and asserts the sandwiched-sky detector finds it.
///
/// Uses its own close-up camera (not the main gate's moderate-distance rig):
/// `distant_flat_terrain_holes.rs`'s own control already measured that a
/// victim at moderate distance can project to under one screen row and be
/// undetectable even when genuinely missing — this reproduces that lesson's
/// fix (a *close* victim) in the uneven scene rather than re-deriving it.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_deliberately_missing_riser_section_is_detected() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let (_resources, atlas) = load_vanilla();
    let models = atlas.models().expect("vanilla atlas must carry baked models");

    let stone = first_state_named("minecraft:stone");
    let air = first_state_named("minecraft:air");
    let world = ziggurat_world(stone, air);

    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);

    // The ring1 riser's own surface section, on the column forming its
    // `-Z`-facing wall (`z = -RING1_HALF`, i.e. chunk `cz = -RING1_HALF/16`).
    // `LEVEL1_Y` (80) is an exact section boundary, so its surface section is
    // index `LEVEL1_Y/16 - 1`.
    let victim = SectionKey {
        cx: 0,
        cz: -RING1_HALF / 16,
        si: (LEVEL1_Y / 16 - 1) as usize,
        min_y: MIN_Y,
    };

    // Close-up: standing just short of the riser, looking straight at it.
    let camera = Camera {
        position: glam::Vec3::new(0.5, LEVEL0_Y as f32 + 1.62, -(RING1_HALF as f32) - 8.5),
        yaw: 0.0,
        pitch: 8.0,
        fov_y_degrees: FOV_Y_DEGREES,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(RD_CHUNKS as u32, 0),
    };

    let (reference, _) = render_frame(
        device, queue, format, &mut target, &atlas, &world, models, &camera, None, false,
    );
    let (subject, stats) = render_frame(
        device, queue, format, &mut target, &atlas, &world, models, &camera, Some(victim), true,
    );

    let (hole_px, bbox, hole_cols, hole_pixels) =
        find_sandwiched_background(&subject, &reference, W, H);
    // The victim section was only skipped in `upload_all` — `world` itself
    // still holds the real stone there — so the oracle (which reads `world`
    // directly) must call every one of these pixels a genuine bug, never
    // legitimate grazing-corner sky. This is the control-of-the-control the
    // module doc promises: proof the new oracle-based invariant still catches
    // a real missing-upload defect and does not launder it into "legal
    // geometry" the way the old aggregate assertion could not have told
    // apart from the residual grazing-corner failures either.
    let (legitimate, genuine_bugs) =
        classify_holes(&world, air, &camera, RD_CHUNKS as u32, &hole_pixels, W, H);
    eprintln!(
        "=== control: riser section {victim:?} never uploaded ===\n\
         sections drawn         = {}\n\
         sandwiched-sky pixels  = {hole_px}\n\
         sandwiched-sky columns = {hole_cols} / {W}\n\
         bounding box           = {bbox:?}\n\
         oracle-legitimate      = {legitimate} (expect 0 — nothing here is a legal grazing corner)\n\
         oracle-genuine bugs    = {} (expect {hole_px} — the oracle must catch all of them)",
        stats.sections_drawn,
        genuine_bugs.len(),
    );
    assert!(
        hole_px > 0,
        "control failed to fire: a riser section was deliberately never uploaded to the GPU \
         and the sandwiched-sky detector found nothing in this uneven scene. Either the \
         victim section projects off-screen, or the detector cannot see a real hole here — \
         fix this before trusting a clean result from the main gate."
    );
    assert_eq!(
        genuine_bugs.len(),
        hole_px,
        "control's own oracle check failed: {legitimate} of {hole_px} deliberately-missing-section \
         pixels were classified as legitimate grazing-corner sky instead of a genuine bug. The \
         world still holds real stone at the victim section (only the GPU upload was skipped), so \
         the oracle should have flagged every one of them — an oracle that can be fooled by this \
         would also launder a real production missing-upload bug into a false pass on the main gate."
    );
}
