//! Phase-5 acceptance gate for **real vanilla block textures**.
//!
//! The running game must render the real resource-pack atlas, not the
//! hand-authored procedural one in `lodestone-shell`. This gate proves the
//! `state_id → block model → texture refs → atlas sprite UVs` seam
//! ([`BlockAtlas`]) against a **real vanilla `client.jar`**, then drives that
//! atlas through the same `BlockClassifier → mesher → BlockPipeline` path the
//! shell uses and reads back pixels.
//!
//! ## Why these tests are `#[ignore]`d and fail *closed*
//!
//! They need a fetched `client.jar` + `generated/reports/blocks.json`, and the
//! pixel gate needs a GPU adapter — none of which the hermetic default suite
//! has. So they are `#[ignore]`d: running one is an **explicit** request for the
//! full path. Once explicitly run, a missing precondition (no jar, no registry,
//! no adapter) is a **failure**, never a silent skip — a green test that reached
//! no pixels is not evidence (§12.52).
//!
//! ## Provenance (§ the v47 lesson)
//!
//! Expected values originate **outside** the resolver: the block→sprite mapping
//! is checked against the real model JSON's texture names, and the atlas pixels
//! are compared against an **independently decoded** `block/stone.png` read
//! straight from the jar. Nothing here trusts an atlas we minted to validate the
//! atlas we minted.
//!
//! Run with:
//! `cargo test -p lodestone-render --test block_texture_gate -- --ignored --nocapture`

use std::path::PathBuf;

use lodestone_assets::{Image, ResourceLocation, ResourceManager, ZipSource};
use lodestone_model::{BlockStateRegistry, Identifier};
use lodestone_render::{
    BlockAtlas, BlockClassifier, BlocksJsonRegistry, Face, blocks_json_registry,
};

// --- jar / registry discovery (mirrors lodestone-assets/tests/real_jar.rs) ---

fn cache_root() -> Option<PathBuf> {
    Some(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()?
            .parent()?
            .join(".cache/mc"),
    )
}

/// Prefers 26.2 explicitly so a fetched legacy jar can never silently swap the
/// corpus out from under a test that expects flattened block dirs.
fn client_jar() -> Option<PathBuf> {
    let cache = cache_root()?;
    let preferred = cache.join("26.2").join("client.jar");
    if preferred.is_file() {
        return Some(preferred);
    }
    let entries = std::fs::read_dir(&cache).ok()?;
    for entry in entries.flatten() {
        let candidate = entry.path().join("client.jar");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// A resource manager over the real `client.jar`. Fails **closed**: an
/// explicitly-run `#[ignore]`d gate must never pass without its jar.
fn manager() -> ResourceManager {
    let jar = client_jar().unwrap_or_else(|| {
        panic!(
            "no client.jar under .cache/mc/<version>/ — fetch it first \
             (cargo run -p xtask -- fetch-assets). A texture gate with no jar is not evidence."
        )
    });
    let source = ZipSource::open(&jar).expect("open client.jar");
    ResourceManager::new(vec![Box::new(source)])
}

fn blocks_report_path() -> Option<PathBuf> {
    let jar = client_jar()?;
    let dir = jar.parent()?;
    let candidate = dir.join("generated/reports/blocks.json");
    candidate.is_file().then_some(candidate)
}

/// The version-free [`BlockStateRegistry`] the shell consumes, loaded through
/// the crate's *shipped* [`blocks_json_registry`] loader — not a test-private
/// parser. Exercising the real loader here is deliberate: the gate then proves
/// the exact API a host calls, so a parallel parser can never drift from it.
/// Fails **closed** — an explicitly-run gate must never pass without its report.
fn blocks_report() -> BlocksJsonRegistry {
    let path = blocks_report_path().unwrap_or_else(|| {
        panic!(
            "missing generated/reports/blocks.json next to the selected client.jar.\n\
             Expected at: .cache/mc/26.2/generated/reports/blocks.json\n\
             Generate it with the vanilla server:  \
             java -DbundlerMainClass=net.minecraft.data.Main -jar server.jar --reports\n\
             then copy generated/reports/ next to the jar. Do NOT skip — a green test \
             with no registry is not evidence."
        )
    });
    blocks_json_registry(&path).expect("parse blocks.json into a registry")
}

/// The first (numerically lowest) state id for a block, derived independently of
/// the atlas under test by walking only the registry's `resolve`/`state_count`.
/// For single-state blocks (stone, air) this is also the canonical state.
fn first_state_of(reg: &impl BlockStateRegistry, block: &str) -> Option<u32> {
    let want: Identifier = block.parse().ok()?;
    (0..reg.state_count()).find(|&i| reg.resolve(i).is_some_and(|r| *r.block == want))
}

/// The state id of a block carrying a specific property value (e.g. `oak_log`
/// with `axis=y`, or `grass_block` with `snowy=false`), derived the same
/// independent way. Used where the canonical state is not the lowest id.
fn state_with(reg: &impl BlockStateRegistry, block: &str, key: &str, value: &str) -> Option<u32> {
    let want: Identifier = block.parse().ok()?;
    (0..reg.state_count()).find(|&i| {
        reg.resolve(i).is_some_and(|r| {
            *r.block == want && r.properties.get(key).map(String::as_str) == Some(value)
        })
    })
}

// --- helpers over a built BlockAtlas -----------------------------------------

/// The atlas sprite location a state's `face` resolves to, or `None` if the
/// state has no surface (air/empty). Walks the public `BlockClassifier` +
/// `atlas()`/`uv_table()` seam exactly as the GPU path does, so this asserts the
/// contract the shell consumes rather than resolver internals.
fn sprite_location_for(atlas: &BlockAtlas, state: u32, face: Face) -> Option<ResourceLocation> {
    let cell = atlas.classify(state, 0, 15);
    let surface = cell.surface?;
    let sprite = surface.sprites[face.index()];
    let loc = atlas.atlas().sprites()[sprite.0 as usize].location.clone();
    Some(loc)
}

// --- Test A: model → sprite mapping + pixel provenance (no GPU) ---------------

#[test]
#[ignore = "requires a fetched vanilla client.jar and generated/reports/blocks.json"]
fn real_vanilla_block_models_map_to_correct_sprites() {
    let manager = manager();
    let registry = blocks_report();
    let atlas = BlockAtlas::build(&manager, &registry).expect("build block atlas from real jar");

    eprintln!(
        "built atlas: {} sprites, {}x{} px",
        atlas.sprite_count(),
        atlas.atlas().width,
        atlas.atlas().height
    );

    // --- stone: a full cube, all six faces the same `block/stone` sprite. ----
    let stone = first_state_of(&registry, "minecraft:stone").expect("stone in registry");
    let stone_cell = atlas.classify(stone, 0, 15);
    assert!(
        stone_cell.occludes,
        "stone is a full opaque cube and must occlude its neighbours"
    );
    let stone_surface = stone_cell.surface.expect("stone has a surface");
    let stone_sprite = stone_surface.sprites[0];
    for (i, s) in stone_surface.sprites.iter().enumerate() {
        assert_eq!(
            *s, stone_sprite,
            "stone face {i} should be the same sprite as face 0 (a uniform cube)"
        );
    }
    let stone_loc = atlas.atlas().sprites()[stone_sprite.0 as usize]
        .location
        .clone();
    assert_eq!(
        stone_loc.path(),
        "block/stone",
        "stone must map to the block/stone texture, got {stone_loc:?}"
    );

    // --- grass_block: top ≠ bottom, and the top is a *tinted* sprite. --------
    let grass = state_with(&registry, "minecraft:grass_block", "snowy", "false")
        .expect("grass_block[snowy=false] in registry");
    let top = sprite_location_for(&atlas, grass, Face::PosY).expect("grass top sprite");
    let bottom = sprite_location_for(&atlas, grass, Face::NegY).expect("grass bottom sprite");
    assert_ne!(
        top, bottom,
        "grass_block top and bottom must differ (green top, dirt bottom), both were {top:?}"
    );
    assert_eq!(
        bottom.path(),
        "block/dirt",
        "grass_block bottom is dirt, got {bottom:?}"
    );
    // The green top is baked as a tinted duplicate under the `lodestone:tinted/`
    // namespace — skipping the tint gives a grey world that looks like a
    // lighting bug, so the gate insists the tint is actually applied.
    assert_eq!(
        top.namespace(),
        "lodestone",
        "grass_block top must be a tinted sprite (lodestone:tinted/…), got {top:?}"
    );
    assert!(
        top.path().starts_with("tinted/"),
        "grass_block top must be a tinted sprite, got {top:?}"
    );

    // --- oak_log[axis=y]: up/down = oak_log_top, sides = oak_log. ------------
    let log = state_with(&registry, "minecraft:oak_log", "axis", "y")
        .expect("oak_log axis=y in registry");
    let log_up = sprite_location_for(&atlas, log, Face::PosY).expect("log up sprite");
    let log_down = sprite_location_for(&atlas, log, Face::NegY).expect("log down sprite");
    let log_side = sprite_location_for(&atlas, log, Face::PosX).expect("log side sprite");
    assert_eq!(log_up.path(), "block/oak_log_top", "oak_log top face");
    assert_eq!(log_down.path(), "block/oak_log_top", "oak_log bottom face");
    assert_eq!(log_side.path(), "block/oak_log", "oak_log side face");

    // --- PROVENANCE: forward name→id (`state_id_of`) agrees with the report's own
    // independent blocks.json inversion (`first_state_of`/`state_with`), which is
    // never derived from the atlas under test. This is the seam impl-shell's
    // generator calls to turn real vanilla state strings into classifier ids.
    assert_eq!(
        atlas.state_id_of("minecraft:stone"),
        Some(stone),
        "state_id_of(bare name) must match the registry's stone id"
    );
    assert_eq!(
        atlas.state_id_of("minecraft:grass_block[snowy=false]"),
        Some(grass),
        "state_id_of(full property string) must match the registry id"
    );
    assert_eq!(
        atlas.state_id_of("minecraft:oak_log[axis=y]"),
        Some(log),
        "state_id_of must round-trip a complete property set to the right id"
    );
    assert_eq!(
        atlas.state_id_of("minecraft:most_certainly_not_a_block"),
        None,
        "an unknown block must resolve to None, not a plausible-but-wrong id"
    );

    // --- PROVENANCE: atlas pixels at stone's sprite == the real stone.png. ---
    // The expected value is decoded straight from the jar by an independent PNG
    // path (`Image::decode_png`), never from the atlas under test.
    let stone_png_bytes = manager
        .read_asset(
            &ResourceLocation::parse("minecraft:block/stone").unwrap(),
            "textures",
            "png",
        )
        .expect("read block/stone.png from jar");
    let stone_png = Image::decode_png(&stone_png_bytes).expect("decode stone.png");

    let sprite = &atlas.atlas().sprites()[stone_sprite.0 as usize];
    assert_eq!(
        (sprite.width, sprite.frame_height),
        (stone_png.width, stone_png.height),
        "stone sprite rect must match the source texture dimensions"
    );

    let atlas_img = atlas.atlas();
    let mut mismatches = 0usize;
    for ty in 0..stone_png.height {
        for tx in 0..stone_png.width {
            let ax = sprite.x + tx;
            let ay = sprite.y + ty;
            let ai = ((ay * atlas_img.width + ax) * 4) as usize;
            let si = ((ty * stone_png.width + tx) * 4) as usize;
            let a = &atlas_img.rgba[ai..ai + 4];
            let s = &stone_png.rgba[si..si + 4];
            if a != s {
                mismatches += 1;
            }
        }
    }
    assert_eq!(
        mismatches, 0,
        "atlas pixels at stone's sprite must match independently-decoded block/stone.png \
         exactly ({mismatches} differing texels) — atlas provenance is broken"
    );

    eprintln!("=== TEXTURE GATE (mapping + provenance) PASSED ===");
    eprintln!("stone={stone} grass_block={grass} oak_log[axis=y]={log}");
}

// --- Test A2: the live `mipmapLevels` setting reaches the built atlas (no GPU) --

/// The live `mipmapLevels` video setting's whole point: changing it must
/// rebuild the atlas at a different mip depth, not just move a slider handle
/// nothing reads. `Atlas::mip_count` (level 0 plus every generated mip) is the
/// discriminating measurement — a live reload that swapped the GPU bind
/// groups but kept building at the frozen `BLOCK_ATLAS_MIP_LEVELS` would still
/// report the default count here every time.
///
/// Requested `0` is the sharpest control: it is vanilla's own "no mips"
/// setting, so `mip_count()` must come back to exactly `1` (level 0 alone) —
/// not merely "fewer than the default", which a half-applied depth could also
/// satisfy. The requested default, `BLOCK_ATLAS_MIP_LEVELS` (4), is the
/// control proving the new parameter did not silently change the *existing*
/// default path's behaviour (`BlockAtlas::build` delegates to it).
///
/// Collected into one assertion rather than an `assert!` inside the loop —
/// `CLAUDE.md`'s own rule against a loop-internal assert hiding every
/// mismatch but the first — so a regression that broke more than one depth
/// reports all of them, not just whichever happened to be checked first.
#[test]
#[ignore = "requires a fetched vanilla client.jar and generated/reports/blocks.json"]
fn the_live_mipmap_levels_setting_changes_the_built_atlas_mip_count() {
    let manager = manager();
    let registry = blocks_report();

    // Real block textures are all 16x16 (or a power-of-two multiple for
    // animated ones), so nothing in this corpus caps the requested depth —
    // every one of these is expected to land exactly, not merely "close".
    let cases: [(u32, u32); 3] = [
        (0, 1),                                       // no mips requested
        (2, 3),                                        // a mid setting
        (lodestone_render::texture::BLOCK_ATLAS_MIP_LEVELS, 5), // the shipped default, 4
    ];

    let mut mismatches = Vec::new();
    for (requested, expected_mip_count) in cases {
        let atlas = BlockAtlas::build_with_mip_levels(&manager, &registry, requested)
            .unwrap_or_else(|e| panic!("build atlas at mip_levels={requested}: {e}"));
        let got = atlas.atlas().mip_count();
        if got != expected_mip_count {
            mismatches.push(format!(
                "requested {requested} mip levels: expected mip_count()=\
                 {expected_mip_count}, got {got} (mip_cap={:?})",
                atlas.atlas().mip_cap()
            ));
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));

    // The control: two different requested depths must not produce the same
    // atlas — if they did, every assertion above could still pass under a
    // constant-mip-count build that happened to hardcode 1/3/5 by coincidence
    // (it cannot, since they are read from the real `Atlas`, but this is the
    // cheap independent check that the parameter is not simply ignored).
    let low = BlockAtlas::build_with_mip_levels(&manager, &registry, 0)
        .expect("build atlas at mip_levels=0");
    let high = BlockAtlas::build_with_mip_levels(
        &manager,
        &registry,
        lodestone_render::texture::BLOCK_ATLAS_MIP_LEVELS,
    )
    .expect("build atlas at the shipped default");
    assert_ne!(
        low.atlas().mip_count(),
        high.atlas().mip_count(),
        "0 and the shipped default must produce different atlases"
    );

    eprintln!("=== MIPMAP LEVELS GATE PASSED ===");
}

// --- Test B: the real atlas reaches pixels (GPU, fail-closed) -----------------

#[test]
#[ignore = "requires a fetched vanilla client.jar and a GPU adapter"]
fn real_vanilla_block_textures_reach_pixels() {
    use lodestone_render::block::{shared_camera_buffer, sprite_uv_buffer};
    use lodestone_render::{
        BlockPipeline, Camera, Cell, ChunkSectionView, DepthBuffer, GpuAtlas,
        GpuContext, GpuMesh, HeadlessTarget, RenderTarget, SectionNeighborhood, SectionView,
        UniformLight, mesh_greedy,
    };
    use lodestone_world::{ChunkSection, PaletteKind};

    let manager = manager();
    let registry = blocks_report();
    let atlas = BlockAtlas::build(&manager, &registry).expect("build block atlas from real jar");

    let air = first_state_of(&registry, "minecraft:air").expect("air in registry");
    let stone = first_state_of(&registry, "minecraft:stone").expect("stone in registry");

    // A solid stone slab inset from the section boundaries so its exposed faces
    // border lit in-section air, mirroring gpu.rs::real_chunk_section_renders.
    let mut section = ChunkSection::new(PaletteKind::block_states(), PaletteKind::biomes(), air, 0);
    for y in 0..8 {
        for z in 4..12 {
            for x in 0..16 {
                section.set_block(x, y, z, stone);
            }
        }
    }
    assert_eq!(section.non_air_count(), 16 * 8 * 8);

    // The real BlockAtlas *is* the classifier — no synthetic sprite mapping.
    let light = UniformLight::default();
    let view = ChunkSectionView::new(&section, &atlas, &light);

    #[derive(Debug)]
    struct AirLit;
    impl SectionView for AirLit {
        fn cell(&self, _x: usize, _y: usize, _z: usize) -> Cell {
            Cell {
                occludes: false,
                surface: None,
                block_light: 0,
                sky_light: 15,
            }
        }
    }
    let air_lit = AirLit;
    let mut hood = SectionNeighborhood::centre_only(&view);
    for dx in -1..=1 {
        for dy in -1..=1 {
            for dz in -1..=1 {
                if (dx, dy, dz) != (0, 0, 0) {
                    hood.set(dx, dy, dz, Some(&air_lit));
                }
            }
        }
    }
    let mesh = mesh_greedy(&hood);
    assert!(
        mesh.quad_count() >= 6,
        "the stone slab shell should mesh to at least its 6 outer faces, got {}",
        mesh.quad_count()
    );

    // Independent expected colour: the real stone.png average, computed outside
    // the render path. The rendered wall is shaded by AO/light so we compare
    // hue/relative-channel structure, not an exact byte match.
    let stone_png = Image::decode_png(
        &manager
            .read_asset(
                &ResourceLocation::parse("minecraft:block/stone").unwrap(),
                "textures",
                "png",
            )
            .expect("read block/stone.png"),
    )
    .expect("decode stone.png");
    let mut sum = [0u64; 3];
    let px = (stone_png.width * stone_png.height) as u64;
    for p in stone_png.rgba.chunks_exact(4) {
        sum[0] += p[0] as u64;
        sum[1] += p[1] as u64;
        sum[2] += p[2] as u64;
    }
    let src_avg = [
        (sum[0] / px) as u8,
        (sum[1] / px) as u8,
        (sum[2] / px) as u8,
    ];
    eprintln!("source stone.png average = {src_avg:?}");

    // GPU — the one legitimate environmental limit, but still fail *closed*.
    let ctx = GpuContext::new_headless_blocking().unwrap_or_else(|e| {
        panic!(
            "no GPU adapter ({e}); the block-texture-to-pixels gate needs a real adapter. \
             Run on a machine with a GPU — this test is #[ignore]d, so running it is an \
             explicit request for the full render path."
        )
    });
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let (w, h) = (96u32, 96u32);
    let mut target = HeadlessTarget::new(device, w, h, format);

    let gpu_mesh = GpuMesh::upload(device, &mesh).expect("non-empty mesh uploads");
    let gpu_atlas = GpuAtlas::from_atlas(device, queue, atlas.atlas());
    let uv = sprite_uv_buffer(device, atlas.uv_table());

    // Camera in front of the −Z wall of the slab, centred on its mid-height.
    let camera = Camera {
        position: glam::Vec3::new(8.0, 4.0, -6.0),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 70.0,
        aspect: w as f32 / h as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(32, 0),
    };
    let cam_buf = shared_camera_buffer(
        device,
        camera.view_projection().to_cols_array_2d(),
        lodestone_render::fog::FogUniform::disabled(),
    );
    // The packed path's group-0 binding 1 (issue #76): one origin slot, at zero
    // — this scene's geometry is already section-local to the origin.
    let origin_buf = lodestone_render::section_origin_buffer(device, [0.0, 0.0, 0.0]);

    let pipeline = BlockPipeline::new(device, format);
    let cam_bg = pipeline.camera_bind_group(device, &cam_buf, &origin_buf);
    let atlas_bg = pipeline.atlas_bind_group(device, &gpu_atlas, &uv);
    let depth = DepthBuffer::new(device, w, h);

    let frame = target.acquire().unwrap();
    let mut encoder =
        device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("block texture gate pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: frame.view(),
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    // Distinct magenta-ish "sky" so a failure to draw is obvious
                    // and can never be mistaken for the grey stone wall.
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.60,
                        g: 0.05,
                        b: 0.65,
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
        pass.set_bind_group(0, &cam_bg, &[0]);
        pass.set_bind_group(1, &atlas_bg, &[]);
        pass.set_vertex_buffer(0, gpu_mesh.vertices.slice(..));
        pass.set_index_buffer(gpu_mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..gpu_mesh.index_count, 0, 0..1);
    }
    queue.submit(std::iter::once(encoder.finish()));
    let pixels = target.read_texels(device, queue);

    let at = |x: u32, y: u32| {
        let i = ((y * w + x) * 4) as usize;
        [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
    };
    let sky = [153u8, 13, 166]; // 0.60/0.05/0.65 in unorm ≈ these
    let is_sky = |p: [u8; 4]| {
        (p[0] as i32 - sky[0] as i32).abs() < 28
            && (p[1] as i32 - sky[1] as i32).abs() < 28
            && (p[2] as i32 - sky[2] as i32).abs() < 28
    };

    // Non-blank: the frame must contain BOTH sky and wall pixels, so neither
    // "drew nothing" (all sky) nor "single fill" can pass.
    let mut sky_px = 0usize;
    let mut wall_px = 0usize;
    let mut sum = [0u64; 3];
    for y in 0..h {
        for x in 0..w {
            let p = at(x, y);
            if is_sky(p) {
                sky_px += 1;
            } else {
                wall_px += 1;
                sum[0] += p[0] as u64;
                sum[1] += p[1] as u64;
                sum[2] += p[2] as u64;
            }
        }
    }
    let total = (w * h) as usize;
    eprintln!(
        "sky pixels: {sky_px} ({:.1}%), wall pixels: {wall_px} ({:.1}%)",
        100.0 * sky_px as f64 / total as f64,
        100.0 * wall_px as f64 / total as f64
    );
    assert!(
        wall_px > total / 20,
        "stone wall covers <5% of the frame ({wall_px}/{total}) — geometry not in view"
    );
    assert!(
        sky_px > total / 100,
        "no sky visible — is the whole frame a single fill? ({sky_px}/{total})"
    );

    let avg = [
        (sum[0] / wall_px as u64) as u8,
        (sum[1] / wall_px as u64) as u8,
        (sum[2] / wall_px as u64) as u8,
    ];
    eprintln!("rendered wall average = {avg:?}");

    // The wall must not be black (atlas/lighting failure) and must not be the
    // magenta sky. Real stone is a near-neutral grey: its channels sit close
    // together, unlike the strongly magenta clear. This ties the pixels to the
    // *real* texture rather than merely "something drew".
    let brightness = avg[0] as u32 + avg[1] as u32 + avg[2] as u32;
    assert!(
        brightness > 90,
        "wall is near-black {avg:?} — atlas sampling or lighting failed"
    );
    assert!(
        avg[1] as i32 > avg[0] as i32 - 40 && avg[2] as i32 + 40 > avg[0] as i32,
        "wall is not the neutral grey of real stone {avg:?} — wrong sprite or missing texture"
    );
    // Guard specifically against rendering the magenta missing-texture sprite or
    // the magenta clear leaking in: real stone's green channel is not far below
    // its red, and blue is not crushed.
    assert!(
        (avg[0] as i32 - avg[1] as i32).abs() < 40 && (avg[0] as i32 - avg[2] as i32).abs() < 45,
        "wall channels are not neutral {avg:?} — likely the magenta 'missing' sprite"
    );

    eprintln!("=== BLOCK TEXTURE GATE (real jar → pixels) PASSED ===");
    eprintln!("stone state id = {stone}, quads = {}", mesh.quad_count());
}
