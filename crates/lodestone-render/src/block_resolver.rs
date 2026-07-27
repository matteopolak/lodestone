//! Real vanilla block-texture resolution: `state_id -> block model -> atlas sprite`.
//!
//! This is the seam the shell's placeholder `blocks.rs` names as "the block-state
//! → model → atlas sprite mapping the library still owes". It takes two
//! version-free inputs — a [`ResourceManager`] over a real resource pack (a
//! vanilla `client.jar`) and a [`BlockStateRegistry`] (`state_id → block name +
//! properties`, satisfied by a version crate's generated table via
//! `lodestone-registry`, or a `blocks.json`-backed loader) — and produces:
//!
//! * a stitched [`Atlas`] of the real block textures (upload with
//!   [`GpuAtlas::from_atlas`](crate::GpuAtlas::from_atlas)),
//! * a `uv_table` of per-sprite atlas sub-rects (feed
//!   [`sprite_uv_buffer`](crate::block::sprite_uv_buffer)), and
//! * a [`BlockClassifier`] mapping each `state_id` to a render [`Cell`] whose
//!   [`Surface`] carries the *real* per-face [`SpriteId`]s.
//!
//! The renderer receives **vanilla global block-state ids** (the numbering in
//! Mojang's `generated/reports/blocks.json`) straight from a server's chunk
//! palette, so that is exactly the id space this resolver keys on.
//!
//! # Scope: cubes first
//!
//! Vanilla block models are not all full cubes — stairs, slabs, fences, panes and
//! cross-shaped plants carry real geometry in their `elements`. The current mesher
//! vocabulary is a per-face cube ([`Surface`] is six [`SpriteId`]s), so this pass
//! **projects full-cube models to their six face sprites** and defers true
//! non-cube geometry: a non-cube state renders as a non-occluding cube of its
//! particle texture (visible and obviously placeholder, and — crucially — it does
//! not cull its neighbours' faces, so deferred geometry leaves no holes). The
//! *resolver's* internal representation is per-face texture bindings plus a
//! full-cube flag, not a baked cube, so extending to real quads later does not
//! reshape this seam.
//!
//! # Tinting
//!
//! Grass, foliage and water sprites are greyscale and tinted by a biome-derived
//! colour; skipping that renders a grey world that reads as a lighting bug. The
//! current packed vertex has no per-vertex tint channel, so this pass applies a
//! **fixed default (plains) tint** by baking a tinted duplicate of each tinted
//! sprite into the atlas — the *which* comes from the real [`vanilla_tint_kind`]
//! classifier and the *colour* from the real colormap PNGs sampled at plains.
//! Per-biome tint (a vertex tint channel + a biome seam) is a deliberate
//! follow-up; the fixed tint is enough to make grass green rather than grey.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use lodestone_assets::{
    Atlas, AtlasBuilder, AtlasError, BlockStateError, BlockStates, Direction, Element, Image,
    ModelError, ModelResolver, ResolvedModel, ResourceLocation, ResourceManager,
    tint::{self, Colormap, TintKind, vanilla_tint_kind},
};
use lodestone_model::{BlockStateRegistry, Identifier};

use crate::section::{Cell, Face, SpriteId, Surface};
use crate::world::BlockClassifier;

/// The maximum number of atlas sprites the packed block vertex can address. The
/// vertex stores the sprite in an 11-bit field (`sprite = w1 & 2047`), so ids run
/// `0..=2047` — 2048 distinct sprites.
pub const MAX_SPRITES: usize = 2048;

/// Plains biome climate, used to sample the grass/foliage colormaps for the fixed
/// default tint. Matches vanilla's plains `temperature`/`downfall`.
const PLAINS_TEMPERATURE: f32 = 0.8;
const PLAINS_DOWNFALL: f32 = 0.4;

/// Vanilla's default water tint (`BiomeSpecialEffects` water colour), used where
/// no colormap applies.
const DEFAULT_WATER_TINT: u32 = 0x003F_76E4;

/// Errors from [`BlockAtlas::build`].
#[derive(Debug, thiserror::Error)]
pub enum BlockAtlasError {
    /// The stitched atlas failed to build.
    #[error("atlas build failed: {0}")]
    Atlas(#[from] AtlasError),
    /// A blockstate JSON failed to parse.
    #[error("blockstate parse failed for {block}: {source}")]
    BlockState {
        /// The offending block identifier.
        block: String,
        /// The underlying parse error.
        source: BlockStateError,
    },
    /// A model failed to resolve in a way that aborts the whole build (parse
    /// errors on individual models are tolerated and fall back to the missing
    /// sprite; this variant is reserved for unrecoverable resolver faults).
    #[error("model resolution failed: {0}")]
    Model(#[from] ModelError),
    /// The resolved atlas needs more sprites than the vertex format can address.
    #[error("resolved {count} sprites but the packed vertex allows at most {MAX_SPRITES}")]
    TooManySprites {
        /// The number of sprites the resolver needed.
        count: usize,
    },
}

/// The render classification of a single block state, precomputed at build time
/// so [`BlockClassifier::classify`] is a lock-free table lookup on the mesher's
/// hot path.
#[derive(Debug, Clone)]
struct StateClass {
    /// Whether the state fully occludes (a complete six-faced cube).
    occludes: bool,
    /// The per-face sprites, or `None` for a lit-but-empty cell (air / a model
    /// with no resolvable texture).
    surface: Option<Surface>,
}

/// A real vanilla block atlas plus the `state_id -> Cell` classification derived
/// from it. See the [module docs](self).
#[derive(Debug)]
pub struct BlockAtlas {
    atlas: Atlas,
    uv_table: Vec<[f32; 4]>,
    classes: Vec<StateClass>,
    missing: SpriteId,
    /// Forward index: a canonical `(block, sorted properties)` key → global state
    /// id, inverted from the reverse-only [`BlockStateRegistry`] so callers who
    /// hold a block-state *string* (e.g. a world generator emitting
    /// `"minecraft:grass_block[snowy=false]"`) can resolve it to the id the
    /// classifier keys on. See [`state_id_of`](BlockAtlas::state_id_of).
    name_to_id: HashMap<(Identifier, BTreeMap<String, String>), u32>,
}

/// A resolved per-face texture reference: the concrete texture location plus the
/// fixed tint colour (if the face is tinted). Internal to the resolver — the
/// general shape the cube projection consumes, kept model-agnostic on purpose.
#[derive(Debug, Clone)]
struct FaceTex {
    loc: ResourceLocation,
    tint: Option<u32>,
}

/// A state projected to render faces, before the atlas exists (texture locations,
/// not yet sprite ids).
#[derive(Debug, Clone)]
struct Projected {
    occludes: bool,
    faces: [Option<FaceTex>; 6],
}

impl BlockAtlas {
    /// Resolve every state in `registry` against the real assets in `manager`.
    ///
    /// `manager` is a resource manager over a vanilla resource pack (typically a
    /// `client.jar` opened with `ZipSource`); `registry` maps each `state_id` to
    /// its block and properties. Returns a [`BlockAtlas`] whose [`atlas`] and
    /// [`uv_table`] feed the GPU and whose [`BlockClassifier`] impl drives the
    /// mesher.
    ///
    /// [`atlas`]: BlockAtlas::atlas
    /// [`uv_table`]: BlockAtlas::uv_table
    pub fn build<R: BlockStateRegistry + ?Sized>(
        manager: &ResourceManager,
        registry: &R,
    ) -> Result<Self, BlockAtlasError> {
        let resolver = ModelResolver::new(manager);
        let colormaps = DefaultTints::load(manager);

        // Cache blockstate parses (per block) and model resolves (per location)
        // so a 32k-state pass touches each file once.
        let mut bs_cache: HashMap<String, Option<BlockStates>> = HashMap::new();
        let mut model_cache: HashMap<ResourceLocation, Option<ResolvedModel>> = HashMap::new();

        let count = registry.state_count();
        let mut projected: Vec<Option<Projected>> = Vec::with_capacity(count as usize);
        // Forward name→id index, inverted from the reverse-only registry. Built
        // over every resolvable id independent of whether its model projects, so a
        // block whose geometry fails to resolve still maps its string to the right
        // id (the classifier renders it lit-empty, but the id stays correct).
        let mut name_to_id: HashMap<(Identifier, BTreeMap<String, String>), u32> =
            HashMap::with_capacity(count as usize);

        // Texture sets to stitch: raw block textures plus tinted duplicates keyed
        // by (source location, tint colour).
        let mut base_textures: BTreeSet<ResourceLocation> = BTreeSet::new();
        let mut tinted_textures: BTreeSet<(ResourceLocation, u32)> = BTreeSet::new();

        for id in 0..count {
            let Some(state) = registry.resolve(id) else {
                projected.push(None);
                continue;
            };
            name_to_id.insert((state.block.clone(), state.properties.clone()), id);
            let block_key = state.block.to_string();

            let blockstates = bs_cache
                .entry(block_key.clone())
                .or_insert_with(|| load_blockstates(manager, state.block.path()));
            let Some(blockstates) = blockstates.as_ref() else {
                projected.push(None);
                continue;
            };

            let proj = project_state(
                blockstates,
                state.block,
                state.properties,
                &resolver,
                &mut model_cache,
                &colormaps,
            );

            if let Some(p) = &proj {
                for face in p.faces.iter().flatten() {
                    base_textures.insert(face.loc.clone());
                    if let Some(rgb) = face.tint {
                        tinted_textures.insert((face.loc.clone(), rgb));
                    }
                }
            }
            projected.push(proj);
        }

        // --- Build the atlas. ------------------------------------------------
        let mut builder = AtlasBuilder::new().with_mip_levels(4);

        for loc in &base_textures {
            // A missing texture is tolerated: the sprite lookup later falls back
            // to the magenta missing sprite. Only hard atlas faults abort.
            let _ = builder.load(manager, loc);
        }
        for (loc, rgb) in &tinted_textures {
            if let Some(img) = load_texture_image(manager, loc) {
                builder.add_texture(tinted_location(loc, *rgb), tint_image(&img, *rgb), None);
            }
        }
        // A deterministic magenta/black "missing" sprite so unresolved faces read
        // as obviously wrong rather than silently transparent.
        builder.add_texture(missing_location(), missing_image(), None);

        let atlas = builder.build()?;

        if atlas.sprites().len() > MAX_SPRITES {
            return Err(BlockAtlasError::TooManySprites {
                count: atlas.sprites().len(),
            });
        }

        // --- Sprite index + uv table. ---------------------------------------
        let mut sprite_index: HashMap<&ResourceLocation, u16> = HashMap::new();
        let mut uv_table: Vec<[f32; 4]> = Vec::with_capacity(atlas.sprites().len());
        for (i, sprite) in atlas.sprites().iter().enumerate() {
            sprite_index.insert(&sprite.location, i as u16);
            let rect = sprite.frame_uv(0, atlas.width, atlas.height).map_or_else(
                || {
                    [
                        sprite.uv_min[0],
                        sprite.uv_min[1],
                        sprite.uv_max[0] - sprite.uv_min[0],
                        sprite.uv_max[1] - sprite.uv_min[1],
                    ]
                },
                |(min, max)| [min[0], min[1], max[0] - min[0], max[1] - min[1]],
            );
            uv_table.push(rect);
        }
        let missing = SpriteId(
            *sprite_index
                .get(&missing_location())
                .expect("missing sprite was just added"),
        );

        // --- Project each state's face textures to sprite ids. ---------------
        let lookup = |ft: &FaceTex| -> SpriteId {
            let key = match ft.tint {
                Some(rgb) => tinted_location(&ft.loc, rgb),
                None => ft.loc.clone(),
            };
            sprite_index.get(&key).map_or(missing, |&i| SpriteId(i))
        };

        let classes = projected
            .into_iter()
            .map(|p| match p {
                None => StateClass {
                    occludes: false,
                    surface: None,
                },
                Some(p) => {
                    let any = p.faces.iter().any(Option::is_some);
                    let surface = any.then(|| {
                        let mut sprites = [missing; 6];
                        for (i, face) in p.faces.iter().enumerate() {
                            if let Some(ft) = face {
                                sprites[i] = lookup(ft);
                            }
                        }
                        Surface { sprites }
                    });
                    StateClass {
                        occludes: p.occludes,
                        surface,
                    }
                }
            })
            .collect();

        Ok(Self {
            atlas,
            uv_table,
            classes,
            missing,
            name_to_id,
        })
    }

    /// The stitched atlas, for [`GpuAtlas::from_atlas`](crate::GpuAtlas::from_atlas).
    #[must_use]
    pub fn atlas(&self) -> &Atlas {
        &self.atlas
    }

    /// Per-sprite atlas sub-rects `[u_min, v_min, u_size, v_size]`, indexed by
    /// [`SpriteId`], for [`sprite_uv_buffer`](crate::block::sprite_uv_buffer).
    #[must_use]
    pub fn uv_table(&self) -> &[[f32; 4]] {
        &self.uv_table
    }

    /// The number of sprites in the atlas (`<= `[`MAX_SPRITES`]).
    #[must_use]
    pub fn sprite_count(&self) -> usize {
        self.atlas.sprites().len()
    }

    /// The magenta "missing texture" sprite unresolved faces fall back to.
    #[must_use]
    pub fn missing_sprite(&self) -> SpriteId {
        self.missing
    }

    /// The vanilla global block-state id for a generator block-state string, or
    /// `None` if it names no known state.
    ///
    /// This is the forward companion to the reverse-only [`BlockStateRegistry`]:
    /// a world generator that emits real vanilla state *strings*
    /// (`"minecraft:grass_block[snowy=false]"`, or a bare `"minecraft:stone"`)
    /// resolves them here to the id the [`BlockClassifier`] keys on, so blocks and
    /// atlas share one id space with no lossy demo palette in between.
    ///
    /// Matching is **exact and structural**: the string is parsed into a
    /// `(block, properties)` pair and looked up against the registry's own
    /// `(block, full-property-set)` entries — property order and whitespace do not
    /// matter, but a *partial* property set (relying on omitted defaults) will not
    /// match, because the registry stores each state's complete property set. An
    /// unrecognised or partial string returns `None` **loudly** rather than
    /// resolving to a plausible-but-wrong id — the caller decides how to handle a
    /// miss (log, fall back), and no silent mis-mapping is possible. Vanilla's own
    /// `BlockState` string form always lists every property, so a faithful
    /// generator string round-trips exactly.
    #[must_use]
    pub fn state_id_of(&self, block_state: &str) -> Option<u32> {
        let key = parse_state_key(block_state)?;
        self.name_to_id.get(&key).copied()
    }
}

/// Parse a block-state string such as `"minecraft:oak_log[axis=y]"` (or a bare
/// `"minecraft:stone"`) into the canonical `(block, sorted properties)` key used
/// by the forward index. Returns `None` if the block identifier is malformed or a
/// property clause is not `key=value`.
fn parse_state_key(block_state: &str) -> Option<(Identifier, BTreeMap<String, String>)> {
    let trimmed = block_state.trim();
    let (name, props) = match trimmed.split_once('[') {
        Some((name, rest)) => {
            let inner = rest.strip_suffix(']')?;
            let mut map = BTreeMap::new();
            for clause in inner.split(',') {
                let clause = clause.trim();
                if clause.is_empty() {
                    continue;
                }
                let (k, v) = clause.split_once('=')?;
                map.insert(k.trim().to_string(), v.trim().to_string());
            }
            (name.trim(), map)
        }
        None => (trimmed, BTreeMap::new()),
    };
    let ident = name.parse::<Identifier>().ok()?;
    Some((ident, props))
}

impl BlockClassifier for BlockAtlas {
    fn classify(&self, state_id: u32, block_light: u8, sky_light: u8) -> Cell {
        match self.classes.get(state_id as usize) {
            // Unknown / air / empty: lit but empty, so neighbouring faces sample
            // real light rather than rendering black.
            None => Cell {
                occludes: false,
                surface: None,
                block_light,
                sky_light,
            },
            Some(c) => Cell {
                occludes: c.occludes,
                surface: c.surface,
                block_light,
                sky_light,
            },
        }
    }
}

/// Loads a block's `blockstates/<path>.json`, or `None` if absent/unparsable.
fn load_blockstates(manager: &ResourceManager, path: &str) -> Option<BlockStates> {
    let loc = ResourceLocation::parse(path).ok()?;
    let bytes = manager.read_asset(&loc, "blockstates", "json")?;
    BlockStates::parse(&bytes).ok()
}

/// Projects a single state to render faces (texture locations + tint), or `None`
/// for a lit-but-empty state (air, or a model with no resolvable geometry).
fn project_state(
    blockstates: &BlockStates,
    block: &lodestone_model::Identifier,
    props: &BTreeMap<String, String>,
    resolver: &ModelResolver,
    model_cache: &mut HashMap<ResourceLocation, Option<ResolvedModel>>,
    colormaps: &DefaultTints,
) -> Option<Projected> {
    // Cube-first: take the first applicable model reference for *this state's
    // properties* (first weight). `applicable_models` selects the matching
    // `variants` key or the applicable `multipart` cases — selecting by property
    // is essential: `oak_log` has axis=x/y/z variants and picking the file-first
    // one (axis=x, rotated) would map the top face to a side texture. Genuine
    // multipart geometry (fences, walls) falls through to the deferred path.
    let model_ref = blockstates
        .applicable_models(props)
        .into_iter()
        .flat_map(<[_]>::iter)
        .next()?;

    let resolved = model_cache
        .entry(model_ref.model.clone())
        .or_insert_with(|| resolver.resolve(&model_ref.model).ok())
        .as_ref()?;

    match cube_element(resolved) {
        Some(element) => {
            let mut faces: [Option<FaceTex>; 6] = Default::default();
            let mut present = 0u32;
            for (dir, face) in &element.faces {
                let Some(loc) = resolved.resolve_texture(&face.texture) else {
                    continue;
                };
                let tint = face.tintindex.and_then(|ti| {
                    let kind = vanilla_tint_kind(block, ti, props);
                    colormaps.color(kind)
                });
                let render_face = rotate_face(direction_to_face(*dir), model_ref.x, model_ref.y);
                faces[render_face.index()] = Some(FaceTex {
                    loc: loc.clone(),
                    tint,
                });
                present += 1;
            }
            if present == 0 {
                return None;
            }
            // A full six-faced cube occludes; a partial one (missing faces) must
            // not cull neighbours.
            let occludes = faces.iter().all(Option::is_some);
            Some(Projected { occludes, faces })
        }
        // Deferred non-cube geometry: render a non-occluding cube of the particle
        // texture so it stays visible without punching holes in its neighbours.
        // But a model with *no elements at all* is genuinely empty — air,
        // barrier, light, structure_void — and must render nothing. Vanilla's
        // `air` model carries `particle: missingno` with zero elements; treating
        // that as a particle cube fills the world with the missing-texture
        // sprite. Only defer when there is real (non-cube) geometry to stand in
        // for.
        None => {
            if resolved.elements.is_empty() {
                return None;
            }
            let particle = resolved.resolve_texture("particle")?;
            let ft = FaceTex {
                loc: particle.clone(),
                tint: None,
            };
            Some(Projected {
                occludes: false,
                faces: [
                    Some(ft.clone()),
                    Some(ft.clone()),
                    Some(ft.clone()),
                    Some(ft.clone()),
                    Some(ft.clone()),
                    Some(ft),
                ],
            })
        }
    }
}

/// Returns the first element that spans the full `0..16` cube, or `None` if the
/// model has no such element (empty or genuinely non-cube geometry).
fn cube_element(model: &ResolvedModel) -> Option<&Element> {
    model.elements.iter().find(|e| is_full_cube(e))
}

/// Whether an element spans the whole block volume with no shrinking rotation.
fn is_full_cube(element: &Element) -> bool {
    const EPS: f32 = 1e-3;
    let full_lo = element.from.iter().all(|&v| v.abs() < EPS);
    let full_hi = element.to.iter().all(|&v| (v - 16.0).abs() < EPS);
    full_lo && full_hi
}

/// Maps a model-space [`Direction`] to the renderer's [`Face`] by matching normals
/// (MC axes: north=-Z, south=+Z, east=+X, west=-X, up=+Y, down=-Y).
const fn direction_to_face(dir: Direction) -> Face {
    match dir {
        Direction::West => Face::NegX,
        Direction::East => Face::PosX,
        Direction::Down => Face::NegY,
        Direction::Up => Face::PosY,
        Direction::North => Face::NegZ,
        Direction::South => Face::PosZ,
    }
}

/// Applies a blockstate variant's `x`/`y` rotation (degrees, multiples of 90) to a
/// face, by rotating its normal. Vanilla applies the `x` rotation then the `y`
/// rotation to the model; a face pointing model-direction `d` ends up pointing
/// `R(d)`. Identity for the default (unrotated) states that dominate terrain and
/// every state the shell's worldgen emits; the rotated-variant path (e.g. logs on
/// their side) is best-effort pending gate validation.
fn rotate_face(face: Face, x_deg: i32, y_deg: i32) -> Face {
    let mut n = face.normal();
    let xs = (((x_deg / 90) % 4) + 4) % 4;
    let ys = (((y_deg / 90) % 4) + 4) % 4;
    for _ in 0..xs {
        // +90° about +X: [x, y, z] -> [x, -z, y]
        n = [n[0], -n[2], n[1]];
    }
    for _ in 0..ys {
        // +90° about +Y: [x, y, z] -> [z, y, -x]
        n = [n[2], n[1], -n[0]];
    }
    normal_to_face(n)
}

/// Inverse of [`Face::normal`] for unit face normals.
fn normal_to_face(n: [i32; 3]) -> Face {
    match n {
        [-1, 0, 0] => Face::NegX,
        [1, 0, 0] => Face::PosX,
        [0, -1, 0] => Face::NegY,
        [0, 1, 0] => Face::PosY,
        [0, 0, -1] => Face::NegZ,
        _ => Face::PosZ,
    }
}

/// The synthetic atlas location for a tinted duplicate of `base` at colour `rgb`.
fn tinted_location(base: &ResourceLocation, rgb: u32) -> ResourceLocation {
    ResourceLocation::parse(&format!(
        "lodestone:tinted/{:06x}/{}",
        rgb & 0xFF_FFFF,
        base.path()
    ))
    .expect("tinted location is always valid")
}

/// The synthetic location of the magenta "missing texture" sprite.
fn missing_location() -> ResourceLocation {
    ResourceLocation::parse("lodestone:missing").expect("missing location is valid")
}

/// Loads and decodes a texture PNG from the manager, or `None` if absent.
fn load_texture_image(manager: &ResourceManager, loc: &ResourceLocation) -> Option<Image> {
    let bytes = manager.read_asset(loc, "textures", "png")?;
    Image::decode_png(&bytes).ok()
}

/// Multiplies an image by an `0xRRGGBB` tint (alpha preserved).
fn tint_image(img: &Image, rgb: u32) -> Image {
    let r = (rgb >> 16) & 0xFF;
    let g = (rgb >> 8) & 0xFF;
    let b = rgb & 0xFF;
    let mut rgba = img.rgba.clone();
    for px in rgba.chunks_exact_mut(4) {
        px[0] = ((u32::from(px[0]) * r) / 255) as u8;
        px[1] = ((u32::from(px[1]) * g) / 255) as u8;
        px[2] = ((u32::from(px[2]) * b) / 255) as u8;
    }
    Image {
        width: img.width,
        height: img.height,
        rgba,
    }
}

/// A 16×16 magenta/black checker so unresolved faces are obviously wrong.
fn missing_image() -> Image {
    let mut rgba = vec![0u8; 16 * 16 * 4];
    for y in 0..16u32 {
        for x in 0..16u32 {
            let magenta = (x / 8 + y / 8) % 2 == 0;
            let px = ((y * 16 + x) * 4) as usize;
            rgba[px] = if magenta { 255 } else { 0 };
            rgba[px + 1] = 0;
            rgba[px + 2] = if magenta { 255 } else { 0 };
            rgba[px + 3] = 255;
        }
    }
    Image {
        width: 16,
        height: 16,
        rgba,
    }
}

/// The fixed default (plains) tint colours, sampled from the real colormap PNGs
/// when present and falling back to Mojang's documented constants otherwise.
#[derive(Debug)]
struct DefaultTints {
    grass: Option<Colormap>,
    foliage: Option<Colormap>,
    dry_foliage: Option<Colormap>,
}

impl DefaultTints {
    fn load(manager: &ResourceManager) -> Self {
        let load = |path: &str, default: u32| -> Option<Colormap> {
            let loc = ResourceLocation::parse(path).ok()?;
            let bytes = manager.read_asset(&loc, "textures", "png")?;
            let img = Image::decode_png(&bytes).ok()?;
            Colormap::from_image(&img, default).ok()
        };
        Self {
            grass: load("minecraft:colormap/grass", tint::colors::FOLIAGE_DEFAULT),
            foliage: load("minecraft:colormap/foliage", tint::colors::FOLIAGE_DEFAULT),
            dry_foliage: load(
                "minecraft:colormap/dry_foliage",
                tint::colors::DRY_FOLIAGE_DEFAULT,
            ),
        }
    }

    /// The fixed default tint colour for a [`TintKind`], or `None` if the kind is
    /// untinted.
    fn color(&self, kind: TintKind) -> Option<u32> {
        let sample = |map: &Option<Colormap>, fallback: u32| {
            map.as_ref()
                .map_or(fallback, |m| m.sample(PLAINS_TEMPERATURE, PLAINS_DOWNFALL))
        };
        match kind {
            TintKind::None => None,
            TintKind::Grass => Some(sample(&self.grass, tint::colors::FOLIAGE_DEFAULT)),
            TintKind::Foliage => Some(sample(&self.foliage, tint::colors::FOLIAGE_DEFAULT)),
            TintKind::DryFoliage => {
                Some(sample(&self.dry_foliage, tint::colors::DRY_FOLIAGE_DEFAULT))
            }
            TintKind::Water => Some(DEFAULT_WATER_TINT),
            TintKind::Constant(rgb) => Some(rgb),
            TintKind::RedstonePower(power) => Some(tint::redstone_power_color(power)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap as StdHashMap;

    // `lodestone_assets::Face` (the model face) under an alias so it doesn't clash
    // with the render `Face`.
    use lodestone_assets::Face as ModelFace;

    fn cube_element_fixture() -> Element {
        let mut faces = StdHashMap::new();
        for dir in [
            Direction::Down,
            Direction::Up,
            Direction::North,
            Direction::South,
            Direction::East,
            Direction::West,
        ] {
            faces.insert(
                dir,
                ModelFace {
                    uv: None,
                    texture: "#all".to_string(),
                    cullface: Some(dir),
                    rotation: 0,
                    tintindex: None,
                },
            );
        }
        Element {
            from: [0.0, 0.0, 0.0],
            to: [16.0, 16.0, 16.0],
            rotation: None,
            faces,
            shade: None,
            light_emission: None,
            name: None,
        }
    }

    #[test]
    fn direction_maps_to_face_by_normal() {
        assert_eq!(direction_to_face(Direction::Up), Face::PosY);
        assert_eq!(direction_to_face(Direction::Down), Face::NegY);
        assert_eq!(direction_to_face(Direction::North), Face::NegZ);
        assert_eq!(direction_to_face(Direction::South), Face::PosZ);
        assert_eq!(direction_to_face(Direction::East), Face::PosX);
        assert_eq!(direction_to_face(Direction::West), Face::NegX);
    }

    #[test]
    fn rotate_face_identity_is_a_noop() {
        for f in Face::ALL {
            assert_eq!(rotate_face(f, 0, 0), f, "no rotation must not move {f:?}");
        }
    }

    #[test]
    fn rotate_face_is_a_bijection_for_every_quarter_turn() {
        for x in [0, 90, 180, 270] {
            for y in [0, 90, 180, 270] {
                let mapped: Vec<Face> = Face::ALL.iter().map(|&f| rotate_face(f, x, y)).collect();
                let unique: BTreeSet<usize> = mapped.iter().map(|f| f.index()).collect();
                assert_eq!(
                    unique.len(),
                    6,
                    "rotation x={x} y={y} must permute the six faces, got {mapped:?}"
                );
            }
        }
    }

    #[test]
    fn rotate_face_yaw_spins_the_horizontal_ring() {
        // A 90° yaw keeps up/down fixed and rotates the horizontal faces.
        assert_eq!(rotate_face(Face::PosY, 0, 90), Face::PosY);
        assert_eq!(rotate_face(Face::NegY, 0, 90), Face::NegY);
        // Horizontal faces move to other horizontal faces, never to up/down.
        for f in [Face::NegX, Face::PosX, Face::NegZ, Face::PosZ] {
            let r = rotate_face(f, 0, 90);
            assert!(
                matches!(r, Face::NegX | Face::PosX | Face::NegZ | Face::PosZ),
                "{f:?} yawed to {r:?}, which is not horizontal"
            );
        }
    }

    #[test]
    fn full_cube_is_detected() {
        assert!(is_full_cube(&cube_element_fixture()));
    }

    #[test]
    fn partial_element_is_not_a_full_cube() {
        let mut e = cube_element_fixture();
        e.to = [16.0, 8.0, 16.0]; // a slab
        assert!(
            !is_full_cube(&e),
            "a half-height element is not a full cube"
        );
    }

    #[test]
    fn tinted_location_is_distinct_per_colour() {
        let base = ResourceLocation::parse("minecraft:block/grass_block_top").unwrap();
        let a = tinted_location(&base, 0x91_BD59);
        let b = tinted_location(&base, 0x77_AB2F);
        assert_ne!(
            a, b,
            "different tints must produce different atlas locations"
        );
        assert_ne!(a, base, "a tinted sprite must not alias its base texture");
    }

    #[test]
    fn tint_image_multiplies_channels() {
        let img = Image {
            width: 1,
            height: 1,
            rgba: vec![255, 255, 255, 200],
        };
        let out = tint_image(&img, 0x80_40_20);
        assert_eq!(out.rgba, vec![0x80, 0x40, 0x20, 200], "alpha is preserved");
    }

    #[test]
    fn parse_state_key_reads_name_and_properties() {
        let (block, props) = parse_state_key("minecraft:grass_block[snowy=false]").unwrap();
        assert_eq!(block.to_string(), "minecraft:grass_block");
        assert_eq!(props.get("snowy").map(String::as_str), Some("false"));
        assert_eq!(props.len(), 1);
    }

    #[test]
    fn parse_state_key_bare_name_has_no_properties() {
        let (block, props) = parse_state_key("minecraft:stone").unwrap();
        assert_eq!(block.to_string(), "minecraft:stone");
        assert!(props.is_empty());
        // Default namespace is applied, so a bare path resolves too.
        let (bare, _) = parse_state_key("stone").unwrap();
        assert_eq!(bare.to_string(), "minecraft:stone");
    }

    #[test]
    fn parse_state_key_is_order_and_whitespace_insensitive() {
        // Property order and surrounding whitespace must not change the key, so a
        // generator string and the registry's sorted BTreeMap compare equal.
        let a = parse_state_key("minecraft:oak_fence[north=true,east=false]").unwrap();
        let b = parse_state_key("minecraft:oak_fence[ east=false , north=true ]").unwrap();
        assert_eq!(a, b, "property order/whitespace must not affect the key");
    }

    #[test]
    fn parse_state_key_rejects_malformed_input() {
        // A property clause without '=' is malformed and must fail loudly rather
        // than resolve to a plausible-but-wrong key.
        assert!(parse_state_key("minecraft:oak_log[axis]").is_none());
        // An unterminated property list is malformed.
        assert!(parse_state_key("minecraft:oak_log[axis=y").is_none());
        // An empty identifier is malformed.
        assert!(parse_state_key("").is_none());
    }
}
