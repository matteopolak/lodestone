//! W7 — real vanilla terrain in the browser from **real server bytes**.
//!
//! This is the honest end-to-end decode→render path, exercised against a
//! fixture of real `level_chunk_with_light` payloads captured from the live
//! vanilla 26.2 server (see `fixtures/chunks.bin`). It deliberately uses only
//! the public APIs of the crates this spike may depend on, and touches none of
//! them:
//!
//! ```text
//! fixture bytes
//!   → LevelChunkWithLight::decode            (lodestone-v770 parser)
//!   → ChunkColumn / ChunkSection             (lodestone-world storage)
//!   → ChunkSectionView + BlockClassifier     (lodestone-render seam)
//!   → SectionNeighborhood::centre_only → mesh_greedy → Mesh
//! ```
//!
//! The classifier is built from a **trimmed real resource pack** (blockstates +
//! models + textures for exactly the blocks the fixture contains), resolved with
//! the real `lodestone-assets` model resolver and stitched into a real atlas —
//! the same path native uses, minus the `std::fs` discovery step (bytes arrive
//! by `fetch` instead).
//!
//! When the live socket lands (blocked on `lodestone-client`'s tokio/`!Send`
//! seam) this becomes a **source swap**, and nothing downstream changes. Per the
//! `ClientEvent::ChunkLoaded` ruling the event stays a bare `{ pos }` dirty
//! signal — it does *not* carry section data — so the browser reacts to it by
//! querying the client-owned world (`world.chunk(pos) -> Option<Arc<LoadedChunk>>`)
//! for the `ChunkColumn`, exactly as this module already meshes from
//! `DecodedChunk.column`. Idempotent queries also survive a tab attaching
//! mid-stream, which a lossy event fold would not.

use std::collections::HashMap;

use lodestone_assets::{
    Atlas, AtlasBuilder, BlockStates, Direction, ModelResolver, ResourceLocation, ResourceManager,
};
use lodestone_core::Reader;
use lodestone_render::{
    BlockClassifier, Cell, ChunkSectionView, Face, Mesh, SectionNeighborhood, SpriteId, Surface,
    UniformLight, mesh_greedy,
};
use lodestone_v770::block_states::{block_name, properties};
use lodestone_v770::packets::chunk::{ChunkShape, LevelChunkWithLight};
use lodestone_world::ChunkColumn;

/// A chunk decoded from the fixture: its column position plus real block storage.
pub struct DecodedChunk {
    pub x: i32,
    pub z: i32,
    pub column: ChunkColumn,
}

/// Parses the `LSCH` fixture format and decodes every chunk payload with the
/// real v770 parser. Format: `b"LSCH" | u32 count | (i32 x, i32 z, u32 len,
/// payload)*`, all little-endian. Each `payload` is exactly the packet body a
/// `level_chunk_with_light` carries (no packet-id varint) — i.e. what a live
/// socket would hand us — so this parser is unchanged when the socket lands.
pub fn parse_fixture(bytes: &[u8]) -> Result<Vec<DecodedChunk>, String> {
    if bytes.len() < 8 || &bytes[0..4] != b"LSCH" {
        return Err("fixture: bad magic (expected LSCH)".into());
    }
    let count = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
    let shape = ChunkShape::overworld_1_21();
    let mut off = 8;
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        if off + 12 > bytes.len() {
            return Err(format!("fixture: truncated header at chunk {i}"));
        }
        let x = i32::from_le_bytes(bytes[off..off + 4].try_into().unwrap());
        off += 4;
        let z = i32::from_le_bytes(bytes[off..off + 4].try_into().unwrap());
        off += 4;
        let len = u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap()) as usize;
        off += 4;
        if off + len > bytes.len() {
            return Err(format!("fixture: truncated payload at chunk {i}"));
        }
        let payload = &bytes[off..off + len];
        off += len;
        let chunk = LevelChunkWithLight::decode(&mut Reader::new(payload), &shape)
            .map_err(|e| format!("decode chunk {i} ({x},{z}): {e}"))?;
        out.push(DecodedChunk {
            x,
            z,
            column: chunk.column,
        });
    }
    Ok(out)
}

/// The distinct non-air block-state ids present across all chunks.
pub fn distinct_block_ids(chunks: &[DecodedChunk]) -> Vec<u32> {
    let mut seen = std::collections::BTreeSet::new();
    for c in chunks {
        let base = c.column.min_y();
        for si in 0..c.column.section_count() {
            if c.column.section(si).is_none() {
                continue;
            }
            let sec_base = base + (si as i32) * 16;
            for y in sec_base..sec_base + 16 {
                for x in 0..16 {
                    for z in 0..16 {
                        let id = c.column.get_block(x, y, z);
                        if id != 0 {
                            seen.insert(id);
                        }
                    }
                }
            }
        }
    }
    seen.into_iter().collect()
}

/// Maps a render [`Face`] to the assets [`Direction`] naming used in models.
fn face_to_direction(face: Face) -> Direction {
    match face {
        Face::NegX => Direction::West,
        Face::PosX => Direction::East,
        Face::NegY => Direction::Down,
        Face::PosY => Direction::Up,
        Face::NegZ => Direction::North,
        Face::PosZ => Direction::South,
    }
}

/// A block-state-id → [`Cell`] classifier backed by a lookup table built from a
/// real resource pack. Air and any unknown id resolve to a lit-but-empty cell,
/// as [`BlockClassifier`] requires, so exposed faces stay correctly lit.
pub struct MapClassifier {
    cells: HashMap<u32, Cell>,
}

impl BlockClassifier for MapClassifier {
    fn classify(&self, state_id: u32, block_light: u8, sky_light: u8) -> Cell {
        match self.cells.get(&state_id) {
            Some(c) => Cell {
                block_light,
                sky_light,
                ..*c
            },
            None => Cell {
                occludes: false,
                surface: None,
                block_light,
                sky_light,
            },
        }
    }
}

/// The classifier plus the atlas and per-sprite UV rects that its [`SpriteId`]s
/// index into.
pub struct TerrainAssets {
    pub classifier: MapClassifier,
    pub atlas: Atlas,
    pub uv_rects: Vec<[f32; 4]>,
    /// Human-readable summary for the HUD.
    pub summary: String,
}

/// Builds the atlas + classifier for `ids` from a resource pack loaded into
/// `manager`, using the real model resolver to pick each block's per-face
/// textures. Non-cube blocks are rendered as textured cubes (an honest
/// simplification, stated in the report). Biome tint is **not** applied, so
/// tinted greyscale textures (e.g. `grass_block_top`) render grey.
pub fn build_terrain_assets(
    manager: &ResourceManager,
    ids: &[u32],
) -> Result<TerrainAssets, String> {
    let resolver = ModelResolver::new(manager);

    // Assign a SpriteId per unique texture location, and remember each block's
    // six-face texture locations so we can build the Surface after stitching.
    let mut sprite_index: HashMap<String, u16> = HashMap::new();
    let mut sprite_locs: Vec<ResourceLocation> = Vec::new();
    let mut faces_by_id: HashMap<u32, [u16; 6]> = HashMap::new();

    let intern = |loc: &ResourceLocation,
                      idx: &mut HashMap<String, u16>,
                      locs: &mut Vec<ResourceLocation>|
     -> u16 {
        let key = loc.to_string();
        if let Some(&s) = idx.get(&key) {
            return s;
        }
        let s = locs.len() as u16;
        idx.insert(key, s);
        locs.push(loc.clone());
        s
    };

    for &id in ids {
        let name = block_name(id).ok_or_else(|| format!("unknown block id {id}"))?;
        let loc = ResourceLocation::parse(name).map_err(|e| format!("bad loc {name}: {e}"))?;

        let bs_bytes = manager
            .read_asset(&loc, "blockstates", "json")
            .ok_or_else(|| format!("missing blockstates for {name}"))?;
        let props: std::collections::BTreeMap<String, String> = properties(id)
            .unwrap_or(&[])
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        let bs = BlockStates::parse(&bs_bytes).map_err(|e| format!("blockstates {name}: {e}"))?;

        let model_loc = bs
            .select_variant(&props)
            .and_then(|r| r.first())
            .map(|m| m.model.clone())
            .or_else(|| {
                bs.applicable_models(&props)
                    .into_iter()
                    .flatten()
                    .next()
                    .map(|m| m.model.clone())
            })
            .ok_or_else(|| format!("no model for {name} {props:?}"))?;

        let resolved = resolver
            .resolve(&model_loc)
            .map_err(|e| format!("resolve {model_loc}: {e}"))?;

        // Prefer the full 0..16 cube element; fall back to the first element.
        let element = resolved
            .elements
            .iter()
            .find(|e| e.from == [0.0, 0.0, 0.0] && e.to == [16.0, 16.0, 16.0])
            .or_else(|| resolved.elements.first());

        // A sensible fallback texture: "particle", else "all", else any bound.
        let fallback = resolved
            .resolve_texture("particle")
            .or_else(|| resolved.resolve_texture("all"))
            .or_else(|| {
                resolved
                    .textures
                    .keys()
                    .find_map(|k| resolved.resolve_texture(k))
            })
            .cloned();

        let mut sprites = [0u16; 6];
        for face in Face::ALL {
            let dir = face_to_direction(face);
            let tex_loc = element
                .and_then(|el| el.faces.get(&dir))
                .and_then(|f| resolved.resolve_texture(&f.texture))
                .cloned()
                .or_else(|| fallback.clone())
                .ok_or_else(|| format!("no texture for {name} face {face:?}"))?;
            sprites[face.index()] = intern(&tex_loc, &mut sprite_index, &mut sprite_locs);
        }
        faces_by_id.insert(id, sprites);
    }

    // Stitch every referenced texture into one atlas.
    let mut builder = AtlasBuilder::new();
    for loc in &sprite_locs {
        builder
            .load(manager, loc)
            .map_err(|e| format!("atlas load {loc}: {e}"))?;
    }
    let atlas = builder.build().map_err(|e| format!("atlas build: {e}"))?;

    // UV rect per SpriteId, in the order sprites were interned. The block shader
    // reads each as `[min.x, min.y, size.x, size.y]` (origin + span), *not*
    // `[min, max]` — `uv = rect.xy + tile * rect.zw`. Passing max here samples
    // the atlas padding (black), so we hand it the span.
    let mut uv_rects = vec![[0.0f32, 0.0, 1.0, 1.0]; sprite_locs.len()];
    for (i, loc) in sprite_locs.iter().enumerate() {
        if let Some(sp) = atlas.sprite(loc) {
            uv_rects[i] = [
                sp.uv_min[0],
                sp.uv_min[1],
                sp.uv_max[0] - sp.uv_min[0],
                sp.uv_max[1] - sp.uv_min[1],
            ];
        }
    }

    // Classifier: each known id → an opaque cube whose faces carry its sprites.
    let mut cells = HashMap::new();
    for (&id, sprites) in &faces_by_id {
        cells.insert(
            id,
            Cell {
                occludes: true,
                surface: Some(Surface {
                    sprites: sprites.map(SpriteId),
                }),
                block_light: 0,
                sky_light: 15,
            },
        );
    }

    let summary = format!(
        "atlas: real vanilla pack, {} blocks → {} sprites, {}×{} px (deflate+PNG decoded in-browser)",
        faces_by_id.len(),
        sprite_locs.len(),
        atlas.width,
        atlas.height,
    );

    Ok(TerrainAssets {
        classifier: MapClassifier { cells },
        atlas,
        uv_rects,
        summary,
    })
}

/// A greedy-meshed section ready to upload, positioned at its world origin.
pub struct SectionMesh {
    /// World-space origin `[x*16, section_min_y, z*16]` for `CameraUniform`.
    pub origin: [f32; 3],
    pub mesh: Mesh,
}

/// Meshes every non-empty section of every fixture chunk, building a real
/// [`SectionNeighborhood`] from the surrounding fixture chunks so faces between
/// two loaded sections are culled (only the region's true outer surfaces
/// remain). This is what makes the lit top plane dominate instead of a grid of
/// dark inter-chunk skirts. Returns one [`SectionMesh`] per section that
/// produced geometry, plus the world-space AABB of all meshed blocks.
pub fn mesh_chunks(
    chunks: &[DecodedChunk],
    classifier: &MapClassifier,
) -> (Vec<SectionMesh>, [f32; 3], [f32; 3]) {
    let light = UniformLight::default();
    // Index chunks by column position so neighbours are O(1) to find.
    let by_pos: HashMap<(i32, i32), &DecodedChunk> =
        chunks.iter().map(|c| ((c.x, c.z), c)).collect();

    let mut meshes = Vec::new();
    let mut min = [f32::MAX; 3];
    let mut max = [f32::MIN; 3];

    for c in chunks {
        let base = c.column.min_y();
        let sc = c.column.section_count();
        for si in 0..sc {
            if c.column.section(si).is_none() {
                continue;
            }

            // Gather the (up to) 27 neighbour section views. Each borrows a real
            // `ChunkSection` from `chunks`, the shared classifier, and `light`;
            // all outlive the mesh call below.
            let mut views: Vec<(i32, i32, i32, ChunkSectionView<'_, MapClassifier, UniformLight>)> =
                Vec::new();
            for dx in -1..=1 {
                for dz in -1..=1 {
                    let Some(nc) = by_pos.get(&(c.x + dx, c.z + dz)) else {
                        continue;
                    };
                    for dy in -1..=1 {
                        let nsi = si as i32 + dy;
                        if nsi < 0 || nsi as usize >= sc {
                            continue;
                        }
                        if let Some(sec) = nc.column.section(nsi as usize) {
                            views.push((dx, dy, dz, ChunkSectionView::new(sec, classifier, &light)));
                        }
                    }
                }
            }

            let mut hood = SectionNeighborhood::default();
            for (dx, dy, dz, v) in &views {
                hood.set(*dx, *dy, *dz, Some(v));
            }
            let mesh = mesh_greedy(&hood);
            if mesh.quad_count() == 0 {
                continue;
            }
            let origin = [
                (c.x * 16) as f32,
                (base + (si as i32) * 16) as f32,
                (c.z * 16) as f32,
            ];
            for k in 0..3 {
                min[k] = min[k].min(origin[k]);
                max[k] = max[k].max(origin[k] + 16.0);
            }
            meshes.push(SectionMesh { origin, mesh });
        }
    }
    if meshes.is_empty() {
        min = [0.0; 3];
        max = [0.0; 3];
    }
    (meshes, min, max)
}
