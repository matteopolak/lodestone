//! Phase-5 acceptance gate: a **real** chunk from the live vanilla 26.2 server,
//! all the way to pixels.
//!
//! Everything else in this crate is proven on synthetic input. This is the one
//! test that connects the whole chain end to end and asserts on the result:
//!
//! ```text
//! live server → lodestone-client session driver → client-owned World
//!   → ClientHandle::loaded_chunks / sections_and_light_at (public query surface) [lodestone-client]
//!   → block state id → (name, properties)   [blocks.json registry]
//!   → blockstate variant/multipart          [lodestone-assets]
//!   → ResolvedModel → baked quads           [lodestone-assets BlockBaker]
//!   → real per-section sky/block light       [lodestone-world SectionLight]
//!   → mesh_models                           [lodestone-render]
//!   → ModelPipeline draw + pixel readback   [lodestone-render]
//! ```
//!
//! Nothing here names a version crate, a packet id, or a wire type. The gate
//! logs in through [`lodestone_client::ClientBuilder`], selecting the concrete
//! adapter *by protocol number* via [`lodestone_registry::adapter_for_protocol`]
//! (the one sanctioned place that names a version, behind its own feature).
//! The client drives login, applies `level_chunk_with_light` to its own `World`,
//! and this gate reads owned `Arc<ChunkSection>` + [`SectionLight`] snapshots back
//! through [`ClientHandle::sections_and_light_at`] — exactly the delivery path a
//! real consumer uses. That is the difference between proving "the mesher works on
//! a chunk we hand-decoded" and proving the whole client path.
//!
//! ## Seam gap this still exposes (reported upstream, not worked around)
//!
//! **Chunk streaming is bounded.** No version-free chunk-batch ack exists
//! (`ClientAction` has no such variant, and neither the adapter nor the driver
//! sends one), so the server stops after its initial unacknowledged batches.
//! That is enough to gate the render path, but it caps the sample size of the
//! measurement tests below.
//!
//! (The former "light is not reachable through the client's public surface" gap
//! is now **closed**: `ClientHandle::light_at` / `sections_and_light_at` expose
//! real per-section light, so this gate meshes live terrain at true brightness —
//! blocks and light pulled in one atomic snapshot — instead of the old
//! full-bright stand-in.)
//!
//! It is gated behind the `live-chunk-gate` feature AND `#[ignore]`, so the
//! default `cargo test` stays hermetic and headless. Run it with:
//!
//! ```text
//! cargo test -p lodestone-render --features live-chunk-gate -- --ignored live_gate
//! ```
//!
//! against the live server on `127.0.0.1:25565` with a fetched `client.jar` +
//! `generated/reports/blocks.json` under `.cache/mc/26.2/`.
//!
//! The assertions are chosen so **"correctly rendered nothing" cannot pass**:
//! non-trivial quad geometry *and* non-uniform pixels *and* a terrain-coloured
//! band at a known screen row. A plausible-looking empty sky, a black frame, or
//! a flat clear-colour fill all fail.
#![cfg(feature = "live-chunk-gate")]

use lodestone_testsupport::unique_username;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use lodestone_assets::{
    Atlas, AtlasBuilder, BakedQuad, BlockBaker, BlockStates, FirstWeight, ModelResolver,
    ResourceLocation, ResourceManager, TextureBinding, ZipSource,
};
use lodestone_client::{ChunkPos, ChunkSection, ClientBuilder, SectionLight};
use lodestone_model::{
    BlockStateRegistry, Identifier, LoginProfile, ResolvedBlockState, ServerAddress,
};
use lodestone_render::{
    Camera, CameraUniform, DepthBuffer, GpuAtlas, GpuContext, GpuModelMesh, HeadlessTarget,
    ModelMesh, ModelPipeline, ModelSectionView, RenderTarget, SECTION_SIZE, is_full_cube,
    is_packed_cube, mesh_models, model_camera_buffer,
};
use uuid::Uuid;

mod gate_harness;
use gate_harness::{require_blocks_report, require_client_jar};

// ---------------------------------------------------------------------------
// Asset + registry harness (reads only from .cache/mc/<version>/, never writes).
// Jar discovery lives in `gate_harness`: it selects the jar by *named version*
// (never by `read_dir` order) and fails closed on a missing jar/registry.
// ---------------------------------------------------------------------------

/// A [`BlockStateRegistry`] built from Mojang's data-generator `blocks.json`,
/// mapping the global palette id straight to `(block, properties)`. This is the
/// same harness `lodestone-assets`' own tests use; the real `v770` crate ships
/// an equivalent `BlockStateTable`, but building the table from the report keeps
/// this test from naming a second version-specific type.
#[derive(Debug)]
struct BlocksReport {
    entries: Vec<Option<(Identifier, BTreeMap<String, String>)>>,
}

impl BlocksReport {
    fn load(path: &Path) -> Option<Self> {
        let bytes = std::fs::read(path).ok()?;
        let root: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        let obj = root.as_object()?;
        let mut states = Vec::new();
        let mut max_id = 0u32;
        for (name, block) in obj {
            let id: Identifier = name.parse().ok()?;
            let Some(arr) = block.get("states").and_then(|s| s.as_array()) else {
                continue;
            };
            for state in arr {
                let sid = state.get("id").and_then(serde_json::Value::as_u64)? as u32;
                let mut props = BTreeMap::new();
                if let Some(p) = state.get("properties").and_then(|p| p.as_object()) {
                    for (k, v) in p {
                        if let Some(v) = v.as_str() {
                            props.insert(k.clone(), v.to_string());
                        }
                    }
                }
                max_id = max_id.max(sid);
                states.push((sid, id.clone(), props));
            }
        }
        let mut entries = vec![None; max_id as usize + 1];
        for (sid, id, props) in states {
            entries[sid as usize] = Some((id, props));
        }
        Some(Self { entries })
    }
}

impl BlockStateRegistry for BlocksReport {
    fn resolve(&self, id: u32) -> Option<ResolvedBlockState<'_>> {
        let (block, properties) = self.entries.get(id as usize)?.as_ref()?;
        Some(ResolvedBlockState { block, properties })
    }
    fn state_count(&self) -> u32 {
        self.entries.len() as u32
    }
}

/// Stitch every texture referenced by every blockstate model into one atlas, so
/// any block a live chunk contains has its sprites present. The baker and the
/// GPU upload MUST share this exact `Atlas` for the UVs to line up.
fn full_block_atlas(manager: &ResourceManager, resolver: &ModelResolver) -> Atlas {
    let mut textures: BTreeSet<ResourceLocation> = BTreeSet::new();
    for path in manager.list("assets/minecraft/blockstates/") {
        let Some(bytes) = manager.read(&path) else {
            continue;
        };
        let Ok(bs) = BlockStates::parse(&bytes) else {
            continue;
        };
        for r in bs.model_refs() {
            if let Ok(model) = resolver.resolve(&r.model) {
                for binding in model.textures.values() {
                    if let TextureBinding::Resolved(loc) = binding {
                        textures.insert(loc.clone());
                    }
                }
            }
        }
    }
    let mut builder = AtlasBuilder::new();
    for loc in &textures {
        let _ = builder.load(manager, loc);
    }
    builder.build().expect("build atlas")
}

// ---------------------------------------------------------------------------
// A baked section: per-cell baked quads + occlusion + face light, ready to mesh
// ---------------------------------------------------------------------------

const N: usize = SECTION_SIZE; // 16

fn idx(x: usize, y: usize, z: usize) -> usize {
    (y * N + z) * N + x
}

/// A [`ModelSectionView`] over one real chunk section: every cell's baked quads,
/// whether it fully occludes (full-cube proxy), and the light each cell's faces
/// should sample.
struct BakedSection {
    /// Per cell: index into `models`, or `usize::MAX` for empty (air/no quads).
    cell: Vec<usize>,
    /// De-duplicated baked models, indexed by `cell`.
    models: Vec<Vec<BakedQuad>>,
    /// Per cell: does it fully occlude an adjacent face?
    occ: Vec<bool>,
    /// Per cell: packed `sky<<4 | block` light **its faces should sample**.
    face_light: Vec<u8>,
    empty: Vec<BakedQuad>,
}

impl ModelSectionView for BakedSection {
    fn quads_at(&self, x: usize, y: usize, z: usize) -> &[BakedQuad] {
        let m = self.cell[idx(x, y, z)];
        if m == usize::MAX {
            &self.empty
        } else {
            &self.models[m]
        }
    }
    fn occludes_at(&self, x: i32, y: i32, z: i32) -> bool {
        if (0..N as i32).contains(&x) && (0..N as i32).contains(&y) && (0..N as i32).contains(&z) {
            self.occ[idx(x as usize, y as usize, z as usize)]
        } else {
            // Neighbour sections aren't loaded here; treat as open so boundary
            // faces are kept rather than silently culled.
            false
        }
    }
    fn light_at(&self, x: usize, y: usize, z: usize) -> u8 {
        self.face_light[idx(x, y, z)]
    }
}

// ---------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires a live 26.2 server on 127.0.0.1:25565 and a fetched client.jar"]
fn live_gate_real_chunk_to_pixels() {
    // Fail closed: this test is #[ignore]d, so running it is an explicit request
    // for the full chunk-to-pixels path. A missing jar/registry is an
    // environment failure, not a reason to silently pass.
    let jar = require_client_jar();
    let report_path = require_blocks_report(&jar);

    // --- 1. Pull one real terrain section from the live server via the client. ---
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let (pos, section_index, section, light) = match rt.block_on(collect_terrain_section()) {
        Ok(c) => c,
        Err(e) => panic!(
            "live gate: chunk collection from the {} server failed: {e} — is the \
             lodestone-mc262 container up on 127.0.0.1:25565?",
            gate_harness::GATE_VERSION,
        ),
    };
    eprintln!(
        "sampled chunk ({}, {}); terrain section index {section_index}, non-air {}",
        pos.x,
        pos.z,
        section.non_air_count()
    );

    // --- 2. Assets: manager → resolver → atlas → baker; registry from report. ---
    let source = ZipSource::open(&jar).expect("open client.jar");
    let manager = ResourceManager::new(vec![Box::new(source)]);
    let resolver = ModelResolver::new(&manager);
    let atlas = full_block_atlas(&manager, &resolver);
    let baker = BlockBaker::new(&manager, &resolver, &atlas);
    let registry = BlocksReport::load(&report_path).expect("load blocks.json");

    // --- 3. Bake every distinct state in the section, build the section view. ---
    let mut cache: HashMap<u32, usize> = HashMap::new();
    let mut models: Vec<Vec<BakedQuad>> = Vec::new();
    let mut cell = vec![usize::MAX; N * N * N];
    let mut occ = vec![false; N * N * N];
    let mut baked_states = 0usize;
    let mut failed_states: BTreeSet<u32> = BTreeSet::new();

    for y in 0..N {
        for z in 0..N {
            for x in 0..N {
                let state = section.get_block(x, y, z);
                if state == 0 {
                    continue; // air: no geometry
                }
                let slot = match cache.get(&state) {
                    Some(&s) => s,
                    None => {
                        let quads = match baker.bake_state(&registry, state, &FirstWeight) {
                            Ok(m) => m.quads,
                            Err(_) => {
                                failed_states.insert(state);
                                Vec::new()
                            }
                        };
                        let s = models.len();
                        models.push(quads);
                        cache.insert(state, s);
                        s
                    }
                };
                if !models[slot].is_empty() {
                    baked_states += 1;
                    cell[idx(x, y, z)] = slot;
                    occ[idx(x, y, z)] = is_full_cube(&models[slot]);
                }
            }
        }
    }

    // Face light from the live column's **real** per-section light, pulled in the
    // same atomic snapshot as the blocks (one lock epoch — see
    // `collect_terrain_section`). This retires the former full-bright stand-in now
    // that `ClientHandle` exposes light (`light_at` / `sections_and_light_at`).
    //
    // The model mesher samples light at a block's *own* cell, but a solid block's
    // own cell is dark (opaque blocks store 0). A visible face is lit by the AIR
    // cell it opens into, so we resolve each cell to the brightest light among
    // itself and its in-section neighbours — the vanilla "sample the exposed
    // neighbour" rule, pre-baked into the per-cell array the mesher reads. We do
    // **not** default out-of-section sky to 15 (that is the too-bright-nether
    // trap); only real stored values from in-bounds cells contribute, packed
    // `sky << 4 | block` exactly as the shader unpacks them.
    const NEIGHBOURS: [(i32, i32, i32); 6] = [
        (-1, 0, 0),
        (1, 0, 0),
        (0, -1, 0),
        (0, 1, 0),
        (0, 0, -1),
        (0, 0, 1),
    ];
    let mut face_light = vec![0u8; N * N * N];
    for y in 0..N {
        for z in 0..N {
            for x in 0..N {
                let mut sky = light.sky_at(x, y, z);
                let mut blk = light.block_at(x, y, z);
                for (dx, dy, dz) in NEIGHBOURS {
                    let (nx, ny, nz) = (x as i32 + dx, y as i32 + dy, z as i32 + dz);
                    if (0..N as i32).contains(&nx)
                        && (0..N as i32).contains(&ny)
                        && (0..N as i32).contains(&nz)
                    {
                        let (nx, ny, nz) = (nx as usize, ny as usize, nz as usize);
                        sky = sky.max(light.sky_at(nx, ny, nz));
                        blk = blk.max(light.block_at(nx, ny, nz));
                    }
                }
                face_light[idx(x, y, z)] = (sky << 4) | blk;
            }
        }
    }

    // Non-vacuous light: prove we meshed REAL light, not a uniform stand-in. A
    // live surface section spans sky-exposed cells (sky 15) and buried cells
    // (sky 0), so the resolved sky light must take more than one distinct value.
    // A degenerate/uniform world — the exact flaw that made an earlier light gate
    // vacuous — would collapse this to a single value and fail here, loudly.
    let distinct_sky: BTreeSet<u8> = face_light.iter().map(|b| b >> 4).collect();
    eprintln!(
        "resolved sky-light levels present: {:?}",
        distinct_sky.iter().collect::<Vec<_>>()
    );
    assert!(
        distinct_sky.len() > 1,
        "resolved sky light is uniform ({distinct_sky:?}) — real per-cell light was \
         not sampled, or the sampled section is degenerate; a single-value light \
         gate is vacuous and must fail rather than pass"
    );

    let view = BakedSection {
        cell,
        models,
        occ,
        face_light,
        empty: Vec::new(),
    };

    // Highest solid local y (for camera framing) and center-column surface.
    let top = (0..N)
        .rev()
        .find(|&y| (0..N).any(|z| (0..N).any(|x| view.cell[idx(x, y, z)] != usize::MAX)))
        .unwrap_or(0);
    eprintln!(
        "baked cells: {baked_states}, distinct models: {}, failed states: {} {:?}, top solid y={top}",
        view.models.len(),
        failed_states.len(),
        failed_states.iter().take(8).collect::<Vec<_>>()
    );

    // --- 4. Mesh the section (no greedy: every visible baked quad once). ---
    let mesh: ModelMesh = mesh_models(&view);
    let quad_count = mesh.quad_count();
    eprintln!(
        "mesh quads: {quad_count}, vertices: {}",
        mesh.vertices.len()
    );
    assert!(
        quad_count > 200,
        "expected non-trivial terrain geometry, got {quad_count} quads — a silent \
         empty/default-geometry failure looks exactly like this"
    );

    // --- 5. GPU: upload, render headless, read back pixels. ---
    // The one legitimate environmental limit — but still fail closed with an
    // actionable message rather than a silent `ok`. By this point the collect +
    // mesh half has already asserted for real (non-trivial geometry above), so a
    // green result genuinely requires reaching pixels.
    let ctx = GpuContext::new_headless_blocking().unwrap_or_else(|e| {
        panic!(
            "live gate: no GPU adapter ({e}); the chunk-to-pixels gate needs a real adapter. \
             Run on a machine with a GPU (this test is #[ignore]d, so running it is an \
             explicit request for the full render path)."
        )
    });
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let (w, h) = (192u32, 192u32);
    let mut target = HeadlessTarget::new(device, w, h, format);

    let gpu_atlas = GpuAtlas::from_atlas(device, queue, &atlas);
    let gpu_mesh = GpuModelMesh::upload(device, &mesh).expect("non-empty mesh uploads");

    // Look down at the terrain top from above and slightly to the -Z side, so the
    // top faces (grass) fill the frame. Section-local space; origin at zero.
    let camera = Camera {
        position: glam::Vec3::new(8.0, top as f32 + 10.0, 1.0),
        yaw: 0.0,
        pitch: 55.0, // positive looks down
        fov_y_degrees: 70.0,
        aspect: w as f32 / h as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(32, 0),
    };
    let cam_buf = model_camera_buffer(device, CameraUniform::new(&camera, [0.0, 0.0, 0.0]));

    let pipeline = ModelPipeline::new(device, format);
    let cam_bg = pipeline.camera_bind_group(device, &cam_buf);
    let atlas_bg = pipeline.atlas_bind_group(device, &gpu_atlas);
    let depth = DepthBuffer::new(device, w, h);

    let frame = target.acquire().unwrap();
    let mut encoder =
        device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("live gate pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: frame.view(),
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    // Distinct "sky" clear so a failure to draw terrain is obvious.
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.40,
                        g: 0.60,
                        b: 0.95,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth.view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&pipeline.pipeline);
        pass.set_bind_group(0, &cam_bg, &[]);
        pass.set_bind_group(1, &atlas_bg, &[]);
        pass.set_vertex_buffer(0, gpu_mesh.vertices.slice(..));
        pass.set_index_buffer(gpu_mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..gpu_mesh.index_count, 0, 0..1);
    }
    queue.submit(std::iter::once(encoder.finish()));
    let pixels = target.read_texels(device, queue);

    // --- 6. Assertions: geometry drew, pixels are non-uniform, terrain band. ---
    let at = |x: u32, y: u32| {
        let i = ((y * w + x) * 4) as usize;
        [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
    };
    let sky = [102u8, 153, 242]; // 0.40/0.60/0.95 in unorm ≈ these
    let is_sky = |p: [u8; 4]| {
        (p[0] as i32 - sky[0] as i32).abs() < 24
            && (p[1] as i32 - sky[1] as i32).abs() < 24
            && (p[2] as i32 - sky[2] as i32).abs() < 24
    };

    // Row-by-row diagnostics to locate the terrain band.
    eprintln!("=== row colour scan (every 24 rows, centre column) ===");
    for row in (0..h).step_by(24) {
        let p = at(w / 2, row);
        eprintln!(
            "row {row:3}: {p:?}{}",
            if is_sky(p) { " (sky)" } else { "" }
        );
    }

    // Non-uniform: the frame must contain BOTH sky and non-sky (terrain) pixels,
    // so neither "all sky" (drew nothing) nor "all one colour" can pass.
    let mut sky_px = 0usize;
    let mut terrain_px = 0usize;
    let mut sum = [0u64; 3];
    for y in 0..h {
        for x in 0..w {
            let p = at(x, y);
            if is_sky(p) {
                sky_px += 1;
            } else {
                terrain_px += 1;
                sum[0] += p[0] as u64;
                sum[1] += p[1] as u64;
                sum[2] += p[2] as u64;
            }
        }
    }
    let total = (w * h) as usize;
    eprintln!(
        "sky pixels: {sky_px} ({:.1}%), terrain pixels: {terrain_px} ({:.1}%)",
        100.0 * sky_px as f64 / total as f64,
        100.0 * terrain_px as f64 / total as f64
    );
    assert!(
        terrain_px > total / 20,
        "terrain covers <5% of the frame ({terrain_px}/{total}) — geometry likely not in view"
    );
    assert!(
        sky_px > total / 100,
        "no sky visible — is the whole frame a single fill? ({sky_px}/{total})"
    );

    let avg = [
        (sum[0] / terrain_px as u64) as u8,
        (sum[1] / terrain_px as u64) as u8,
        (sum[2] / terrain_px as u64) as u8,
    ];
    eprintln!("average terrain colour: {avg:?}");
    // Terrain must not be black (a lighting/atlas failure) and must read as an
    // earthy/green surface rather than the blue sky.
    let brightness = avg[0] as u32 + avg[1] as u32 + avg[2] as u32;
    assert!(
        brightness > 90,
        "terrain is near-black {avg:?} — lighting or atlas sampling failed"
    );
    assert!(
        avg[1] >= avg[2],
        "terrain is bluer than it is green {avg:?} — expected grass/earth, not sky"
    );

    eprintln!("=== PHASE-5 GATE PASSED: live chunk → pixels ===");
    eprintln!("chunk ({}, {}) section {section_index}", pos.x, pos.z);
    eprintln!("terrain quads rendered: {quad_count}");
    eprintln!("distinct baked models:  {}", view.models.len());
    eprintln!("average terrain colour: {avg:?}");
}

/// D1 datum on **real terrain**: what fraction of block *instances* bake to the
/// packed 8-byte cube format vs. the wide float format? The model census already
/// reports this by *distinct state* over the whole registry (~9% full cubes);
/// this measures the occurrence-weighted number a real world actually renders,
/// which is the figure that decides whether keeping two vertex formats pays off.
///
/// Caveat, stated loudly: the only live v770 server available is a **flat world**
/// (bedrock/dirt/grass — all full cubes), so the occurrence-weighted answer here
/// is expected to be ~100% packed. That is itself the point ("the full-cube
/// blocks are the ones that fill a world"), but it is NOT a diversity sample; the
/// distinct-state breadth comes from the census, not from this world.
#[test]
#[ignore = "requires a live 26.2 server on 127.0.0.1:25565 and a fetched client.jar"]
fn live_packed_wide_ratio() {
    let jar = require_client_jar();
    let report_path = require_blocks_report(&jar);

    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let sections = match rt.block_on(collect_sections(128)) {
        Ok(s) => s,
        Err(e) => panic!(
            "live gate: chunk collection failed: {e} — is the lodestone-mc262 container up \
             on 127.0.0.1:25565?"
        ),
    };
    assert!(!sections.is_empty(), "collected zero populated sections");
    let columns_sampled = sections
        .iter()
        .map(|(pos, _, _, _)| (pos.x, pos.z))
        .collect::<BTreeSet<_>>()
        .len();

    let source = ZipSource::open(&jar).expect("open client.jar");
    let manager = ResourceManager::new(vec![Box::new(source)]);
    let resolver = ModelResolver::new(&manager);
    let atlas = full_block_atlas(&manager, &resolver);
    let baker = BlockBaker::new(&manager, &resolver, &atlas);
    let registry = BlocksReport::load(&report_path).expect("load blocks.json");

    // Classify each distinct state once; weight by instance count.
    #[derive(Clone, Copy, PartialEq)]
    enum Kind {
        Packed,
        Wide,
        Empty,
    }
    let mut kind_of: HashMap<u32, Kind> = HashMap::new();
    let mut classify = |state: u32| -> Kind {
        *kind_of.entry(state).or_insert_with(|| {
            match baker.bake_state(&registry, state, &FirstWeight) {
                Ok(m) if m.quads.is_empty() => Kind::Empty,
                Ok(m) if is_packed_cube(&m.quads) => Kind::Packed,
                Ok(_) => Kind::Wide,
                Err(_) => Kind::Empty,
            }
        })
    };

    let mut inst_packed = 0u64;
    let mut inst_wide = 0u64;
    let mut inst_empty = 0u64;
    let mut states_seen: BTreeSet<u32> = BTreeSet::new();

    for (_pos, _si, section, _light) in &sections {
        for y in 0..N {
            for z in 0..N {
                for x in 0..N {
                    let state = section.get_block(x, y, z);
                    if state == 0 {
                        continue;
                    }
                    states_seen.insert(state);
                    match classify(state) {
                        Kind::Packed => inst_packed += 1,
                        Kind::Wide => inst_wide += 1,
                        Kind::Empty => inst_empty += 1,
                    }
                }
            }
        }
    }

    let geo = inst_packed + inst_wide; // instances that actually produce geometry
    let (mut d_packed, mut d_wide, mut d_empty) = (0usize, 0usize, 0usize);
    for &s in &states_seen {
        match kind_of[&s] {
            Kind::Packed => d_packed += 1,
            Kind::Wide => d_wide += 1,
            Kind::Empty => d_empty += 1,
        }
    }

    eprintln!("\n=== PACKED:WIDE RATIO ON LIVE TERRAIN ===");
    eprintln!("columns sampled             : {columns_sampled}");
    eprintln!("populated sections          : {}", sections.len());
    eprintln!(
        "block instances (non-air)   : {} (packed {inst_packed}, wide {inst_wide}, empty {inst_empty})",
        inst_packed + inst_wide + inst_empty
    );
    if geo > 0 {
        eprintln!(
            "occurrence-weighted         : {:.2}% packed, {:.2}% wide",
            100.0 * inst_packed as f64 / geo as f64,
            100.0 * inst_wide as f64 / geo as f64
        );
    }
    eprintln!(
        "distinct states seen        : {} (packed {d_packed}, wide {d_wide}, empty {d_empty})",
        states_seen.len()
    );
    let d_geo = d_packed + d_wide;
    if d_geo > 0 {
        eprintln!(
            "distinct-state (this world) : {:.1}% packed, {:.1}% wide",
            100.0 * d_packed as f64 / d_geo as f64,
            100.0 * d_wide as f64 / d_geo as f64
        );
    }
    eprintln!("per-state seen (id → block → kind):");
    for &s in &states_seen {
        let name = registry
            .resolve(s)
            .map(|r| r.block.to_string())
            .unwrap_or_else(|| "<unknown>".into());
        let kind = match kind_of[&s] {
            Kind::Packed => "packed",
            Kind::Wide => "wide",
            Kind::Empty => "empty",
        };
        let count = sections
            .iter()
            .flat_map(|(_, _, sec, _)| {
                (0..N).flat_map(move |y| {
                    (0..N).flat_map(move |z| (0..N).map(move |x| sec.get_block(x, y, z)))
                })
            })
            .filter(|&b| b == s)
            .count();
        eprintln!("  {s:>4} → {name:<28} {kind:<6} ×{count}");
    }
    eprintln!("=========================================\n");

    assert!(
        geo > 0,
        "no geometry-producing blocks found in live terrain"
    );
}

/// Bug-2 datum on **real terrain**: how much does greedy meshing actually reduce
/// quad count once merges are restricted to a single sprite (which the mesher
/// already enforces via `QuadKey`)? This is the number that decides whether the
/// greedy path earns its complexity, and it folds into the packed-vs-wide format
/// question (a shader that must carry an explicit sprite rect + tile span per
/// quad is more per-vertex data).
///
/// Method: each non-air block is treated as a packed opaque cube whose sprite id
/// *is its block-state id* (a faithful proxy — greedy merges coplanar faces iff
/// same sprite **and** same corner light, so same-state adjacency is exactly what
/// merges on real geometry). We mesh every populated section with `mesh_simple`
/// (one quad per visible face, the reference) and `mesh_greedy`, and report the
/// merge factor under **uniform full-bright light** — greedy's ceiling.
///
/// SEAM GAP: the real-light merge factor (where light discontinuities break
/// merges, which is what the shipping mesher actually sees) cannot be measured
/// through the client's public surface, because it exposes no per-cell light (see
/// the crate docs). That variant returns when a `ClientHandle` light accessor
/// lands; until then this reports the uniform upper bound only.
///
/// Caveat stated loudly: the only live world is **flat** (uniform 16×16
/// layers), which is greedy's *best* case — real varied terrain (caves, ores,
/// foliage, height variation) merges far less. Read this as an upper bound.
#[test]
#[ignore = "requires a live 26.2 server on 127.0.0.1:25565 and a fetched client.jar"]
fn live_greedy_merge_factor() {
    use lodestone_render::{
        Cell, SectionNeighborhood, SectionView, SpriteId, Surface, mesh_greedy, mesh_simple,
    };

    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let live = match rt.block_on(collect_sections(128)) {
        Ok(s) => s,
        Err(e) => panic!(
            "live gate: chunk collection failed: {e} — is the lodestone-mc262 container up \
             on 127.0.0.1:25565?"
        ),
    };
    assert!(!live.is_empty(), "collected zero populated sections");
    let columns_sampled = live
        .iter()
        .map(|(pos, _, _, _)| (pos.x, pos.z))
        .collect::<BTreeSet<_>>()
        .len();

    // A packed proxy over one real section: sprite = state id, full-bright light.
    // Real per-cell light is unavailable through the client surface (see the doc
    // comment), so this measures greedy's uniform-light ceiling.
    struct PackedProxy {
        sprite: [u16; N * N * N], // 0 = air
        occ: [bool; N * N * N],
    }
    impl SectionView for PackedProxy {
        fn cell(&self, x: usize, y: usize, z: usize) -> Cell {
            let i = idx(x, y, z);
            let s = self.sprite[i];
            if s == 0 {
                // Air: emits no geometry, and carries zero light so nothing
                // breaks a merge in this uniform model.
                return Cell {
                    occludes: false,
                    surface: None,
                    block_light: 0,
                    sky_light: 0,
                };
            }
            Cell {
                occludes: self.occ[i],
                surface: Some(Surface::uniform(SpriteId(s))),
                block_light: 0,
                sky_light: 15,
            }
        }
    }

    let build = |section: &ChunkSection| -> PackedProxy {
        let mut p = PackedProxy {
            sprite: [0u16; N * N * N],
            occ: [false; N * N * N],
        };
        for y in 0..N {
            for z in 0..N {
                for x in 0..N {
                    let i = idx(x, y, z);
                    let state = section.get_block(x, y, z);
                    // Proxy sprite: fold the 32-bit state id into the 11-bit
                    // sprite field the packed vertex actually carries, so the
                    // merge structure matches what ships. Distinct states stay
                    // distinct within a section in practice (few states/section).
                    p.sprite[i] = if state == 0 {
                        0
                    } else {
                        ((state & 0x7ff) as u16).max(1)
                    };
                    p.occ[i] = state != 0;
                }
            }
        }
        p
    };

    let mut section_count = 0usize;
    let (mut simple_u, mut greedy_u) = (0usize, 0usize);

    for (_pos, _si, section, _light) in &live {
        section_count += 1;
        let pu = build(section);
        let hu = SectionNeighborhood::centre_only(&pu);
        simple_u += mesh_simple(&hu).quad_count();
        greedy_u += mesh_greedy(&hu).quad_count();
    }

    eprintln!("\n=== GREEDY MERGE FACTOR ON LIVE TERRAIN (flat world — upper bound) ===");
    eprintln!("columns sampled           : {columns_sampled}");
    eprintln!("populated sections        : {section_count}");
    eprintln!(
        "uniform light  simple={simple_u:>7}  greedy={greedy_u:>7}  factor={:.2}×  (-{:.1}% quads)",
        simple_u as f64 / greedy_u.max(1) as f64,
        100.0 * (simple_u.saturating_sub(greedy_u)) as f64 / simple_u.max(1) as f64,
    );
    eprintln!("real-light merge factor   : unavailable (client exposes no light — seam gap)");
    eprintln!("=====================================================================\n");

    assert!(
        simple_u > 0,
        "no packed geometry produced from live terrain"
    );
    assert!(
        greedy_u <= simple_u,
        "greedy must never produce more quads than simple"
    );
}

// ---------------------------------------------------------------------------
// Live collection through lodestone-client's public API.
//
// The gate names no version crate, no packet id, and no wire type. It obtains a
// concrete adapter by protocol number from the registry (the one sanctioned
// aggregator), logs in through `ClientBuilder`, waits for chunks, and reads owned
// `Arc<ChunkSection>` snapshots back through `ClientHandle::sections_at`. Those
// Arcs stay valid after the session is dropped, so meshing runs off a stable
// snapshot exactly as a real consumer's mesh thread would.
//
// Fail-closed: this is an `#[ignore]`d test, so running it is an explicit opt-in.
// A failed connect/login, an unreachable server, or a stream that never delivers
// a chunk is an *environment failure* surfaced as `Err` for the caller to
// `panic!` on — never a silent empty success.
// ---------------------------------------------------------------------------

/// Sections to probe per column. The client exposes no `section_count`, so we
/// probe a generous fixed range and keep the populated slots (`sections_at`
/// yields `None` for absent / all-air / out-of-range, so over-probing is cheap
/// and safe). 32 sections covers any live overworld column height.
const MAX_SECTIONS: usize = 32;

/// Connect to the live server via the client, run until chunks arrive (bounded),
/// and return every populated section as an owned snapshot `(pos, index, arc)`.
async fn collect_sections(
    min_chunks: usize,
) -> Result<Vec<(ChunkPos, usize, Arc<ChunkSection>, SectionLight)>, String> {
    let server = ServerAddress {
        host: "127.0.0.1".into(),
        port: 25565,
    };
    let profile = LoginProfile {
        username: unique_username(),
        uuid: Uuid::new_v4(),
    };
    // Select the adapter by protocol number — the one sanctioned place a version
    // is named, behind the registry's own feature. The gate never names v770.
    let adapter = lodestone_registry::adapter_for_protocol(776).ok_or_else(|| {
        "registry has no adapter for protocol 776 — the v770 family must be compiled in \
         (lodestone-render enables it via the registry's `v770` feature)"
            .to_string()
    })?;

    let (handle, _events) = ClientBuilder::new(server, profile, adapter)
        .connect()
        .await
        .map_err(|e| format!("connect: {e}"))?;

    handle
        .wait_for_login(Duration::from_secs(30))
        .await
        .map_err(|e| {
            format!("login never completed: {e} — is the lodestone-mc262 container up on 127.0.0.1:25565?")
        })?;
    handle
        .wait_for_spawn(Duration::from_secs(30))
        .await
        .map_err(|e| format!("never spawned into the world: {e}"))?;
    // Best-effort: wait for a useful number of columns. Streaming is bounded by
    // the missing chunk-batch ack (see the crate docs), so tolerate the server
    // stalling after its initial unacknowledged batches rather than failing here.
    let _ = handle
        .wait_for_chunks(min_chunks, Duration::from_secs(30))
        .await;

    let columns = handle.loaded_chunks();
    if columns.is_empty() {
        return Err("logged in and spawned but the server streamed no chunk columns".to_string());
    }

    let mut out: Vec<(ChunkPos, usize, Arc<ChunkSection>, SectionLight)> = Vec::new();
    for pos in columns {
        // Blocks and light for each block section in one atomic snapshot: block
        // section i pairs with light section i+1 (light section 0 is the boundary
        // below the world). Reading both under a single lock means a LIGHT_UPDATE
        // or BLOCK_UPDATE landing mid-collect cannot hand us geometry from one tick
        // and light from another.
        let requests: Vec<(ChunkPos, usize, usize)> =
            (0..MAX_SECTIONS).map(|i| (pos, i, i + 1)).collect();
        for (index, (section, light)) in handle
            .sections_and_light_at(&requests)
            .into_iter()
            .enumerate()
        {
            if let (Some(section), Some(light)) = (section, light)
                && section.non_air_count() > 0
            {
                out.push((pos, index, section, light));
            }
        }
    }
    if out.is_empty() {
        return Err("chunks loaded but every section was empty (all air)".to_string());
    }
    // `handle`/`_events` drop here → the session shuts down; the `Arc`s we kept
    // are owned copy-on-write snapshots and stay valid and unchanged.
    Ok(out)
}

/// Pick one real **surface** terrain section from the live world: the highest
/// populated section index per column is the topmost terrain (grass on top, air
/// above), and among those we take the densest so the frame fills with terrain.
async fn collect_terrain_section()
-> Result<(ChunkPos, usize, Arc<ChunkSection>, SectionLight), String> {
    type SectionEntry = (ChunkPos, usize, Arc<ChunkSection>, SectionLight);
    let sections = collect_sections(1).await?;
    // Highest populated section per column keyed by (x, z).
    let mut top: HashMap<(i32, i32), SectionEntry> = HashMap::new();
    for (pos, index, section, light) in sections {
        top.entry((pos.x, pos.z))
            .and_modify(|entry| {
                if index > entry.1 {
                    *entry = (pos, index, section.clone(), light.clone());
                }
            })
            .or_insert((pos, index, section, light));
    }
    top.into_values()
        .max_by_key(|(_, _, section, _)| section.non_air_count())
        .ok_or_else(|| "no populated surface section found".to_string())
}
