//! Multi-noise biome selection: the six climate fields quantised to a target
//! point, matched against a biome parameter list through a bucketed search tree,
//! plus the end's island-height source.
//!
//! The tree is a literal port of the real structure, because ties between
//! equidistant rows are broken by tree order and by a last-result hint. The hint
//! lives in a [`ClimateCursor`] owned by the caller; replaying queries in the
//! reference order (a chunk's sections bottom-up, then x, y, z within a section)
//! reproduces the reference answers.

use serde_json::Value;

use super::biome::{BiomeId, BiomeTable};
use super::sampler::Ctx;
use super::settings::TerrainGenerator;
use super::volume::Volume;

/// Climate coordinates are quantised to ten-thousandths.
#[must_use]
pub fn quantize(value: f32) -> i64 {
    (value * 10000.0_f32) as i64
}

/// One climate value of a row: a closed interval, quantised.
type Span = (i64, i64);

fn span_distance(s: Span, target: i64) -> i64 {
    let above = target - s.1;
    let below = s.0 - target;
    if above > 0 { above } else { below.max(0) }
}

/// A quantised climate sample: temperature, humidity, continentalness, erosion,
/// depth, weirdness.
pub type Target = [i64; 6];

#[derive(Debug)]
struct Node {
    space: [Span; 7],
    kind: Kind,
}

#[derive(Debug)]
enum Kind {
    Leaf(BiomeId),
    Sub(Vec<usize>),
}

/// The biome parameter list and its search tree.
#[derive(Debug)]
pub struct ClimateTree {
    nodes: Vec<Node>,
    root: usize,
}

/// The previous search's leaf, which seeds the next search.
#[derive(Clone, Copy, Debug, Default)]
pub struct ClimateCursor(Option<usize>);

const CHILDREN_PER_NODE: usize = 19;

impl ClimateTree {
    /// Builds the tree from `[tmin,tmax,hmin,hmax,cmin,cmax,emin,emax,dmin,dmax,wmin,wmax,offset,"biome"]` rows.
    ///
    /// # Errors
    /// On a malformed row, an unknown biome or an empty list.
    pub fn from_json(json: &str, biomes: &BiomeTable) -> Result<Self, String> {
        let doc: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
        let rows = doc.as_array().ok_or("climate list is not an array")?;
        let mut nodes = Vec::with_capacity(rows.len() * 2);
        let mut leaves = Vec::with_capacity(rows.len());
        for row in rows {
            let r = row.as_array().filter(|r| r.len() == 14).ok_or("climate row must have 14 fields")?;
            let n = |i: usize| r[i].as_i64().ok_or_else(|| format!("climate field {i} is not an integer"));
            let mut space = [(0, 0); 7];
            for (d, s) in space.iter_mut().enumerate().take(6) {
                *s = (n(2 * d)?, n(2 * d + 1)?);
            }
            space[6] = (n(12)?, n(12)?);
            let name = r[13].as_str().ok_or("climate biome is not a string")?;
            let id = biomes.id(name).ok_or_else(|| format!("unknown biome {name}"))?;
            leaves.push(nodes.len());
            nodes.push(Node { space, kind: Kind::Leaf(id) });
        }
        if leaves.is_empty() {
            return Err("empty climate list".into());
        }
        let root = build(&mut nodes, leaves);
        Ok(Self { nodes, root })
    }

    fn distance(&self, node: usize, target: &[i64; 7]) -> i64 {
        let space = &self.nodes[node].space;
        (0..7).map(|i| span_distance(space[i], target[i]).pow(2)).sum()
    }

    /// The closest row's biome, updating the cursor's last-result hint.
    pub fn search(&self, target: &Target, cursor: &mut ClimateCursor) -> BiomeId {
        let t = [target[0], target[1], target[2], target[3], target[4], target[5], 0];
        let leaf = self.search_node(self.root, &t, cursor.0).expect("a non-empty tree yields a leaf");
        cursor.0 = Some(leaf);
        match self.nodes[leaf].kind {
            Kind::Leaf(b) => b,
            Kind::Sub(_) => unreachable!("search returns leaves"),
        }
    }

    fn search_node(&self, node: usize, target: &[i64; 7], candidate: Option<usize>) -> Option<usize> {
        match &self.nodes[node].kind {
            Kind::Leaf(_) => Some(node),
            Kind::Sub(children) => {
                let mut min = candidate.map_or(i64::MAX, |c| self.distance(c, target));
                let mut closest = candidate;
                for &child in children {
                    let cd = self.distance(child, target);
                    if min > cd {
                        let leaf = self.search_node(child, target, closest);
                        let ld = if leaf == Some(child) { cd } else { self.distance(leaf.expect("leaf"), target) };
                        if min > ld {
                            min = ld;
                            closest = leaf;
                        }
                    }
                }
                closest
            }
        }
    }

    /// The row count, for sanity checks.
    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.iter().filter(|n| matches!(n.kind, Kind::Leaf(_))).count()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn center(s: Span) -> i64 {
    (s.0 + s.1) / 2
}

fn space_of(nodes: &[Node], children: &[usize]) -> [Span; 7] {
    let mut out = nodes[children[0]].space;
    for &c in &children[1..] {
        for d in 0..7 {
            let s = nodes[c].space[d];
            out[d] = (out[d].0.min(s.0), out[d].1.max(s.1));
        }
    }
    out
}

fn sort_by_dimension(nodes: &[Node], list: &mut [usize], dimension: usize, absolute: bool) {
    let key = |n: usize, d: usize| {
        let c = center(nodes[n].space[d]);
        if absolute { c.abs() } else { c }
    };
    list.sort_by(|&a, &b| {
        for k in 0..7 {
            let d = (dimension + k) % 7;
            let o = key(a, d).cmp(&key(b, d));
            if o != std::cmp::Ordering::Equal {
                return o;
            }
        }
        std::cmp::Ordering::Equal
    });
}

fn build(nodes: &mut Vec<Node>, mut children: Vec<usize>) -> usize {
    if children.len() == 1 {
        return children[0];
    }
    if children.len() <= CHILDREN_PER_NODE {
        children.sort_by_key(|&c| (0..7).map(|d| center(nodes[c].space[d]).abs()).sum::<i64>());
        let space = space_of(nodes, &children);
        nodes.push(Node { space, kind: Kind::Sub(children) });
        return nodes.len() - 1;
    }
    let mut min_cost = i64::MAX;
    let mut min_dimension = 0;
    let mut min_buckets: Vec<Vec<usize>> = Vec::new();
    for d in 0..7 {
        sort_by_dimension(nodes, &mut children, d, false);
        let buckets = bucketize(&children);
        let cost: i64 = buckets
            .iter()
            .map(|b| space_of(nodes, b).iter().map(|s| (s.1 - s.0).abs()).sum::<i64>())
            .sum();
        if min_cost > cost {
            min_cost = cost;
            min_dimension = d;
            min_buckets = buckets;
        }
    }
    // The buckets are sorted as sub-trees, by their bounding spaces.
    let mut order: Vec<(usize, [Span; 7])> = min_buckets.iter().enumerate().map(|(i, b)| (i, space_of(nodes, b))).collect();
    order.sort_by(|a, b| {
        for k in 0..7 {
            let d = (min_dimension + k) % 7;
            let o = center(a.1[d]).abs().cmp(&center(b.1[d]).abs());
            if o != std::cmp::Ordering::Equal {
                return o;
            }
        }
        std::cmp::Ordering::Equal
    });
    let subs: Vec<usize> = order.into_iter().map(|(i, _)| build(nodes, std::mem::take(&mut min_buckets[i]))).collect();
    let space = space_of(nodes, &subs);
    nodes.push(Node { space, kind: Kind::Sub(subs) });
    nodes.len() - 1
}

fn bucketize(nodes: &[usize]) -> Vec<Vec<usize>> {
    let expected = (CHILDREN_PER_NODE as f64)
        .powf(((nodes.len() as f64 - 0.01).ln() / (CHILDREN_PER_NODE as f64).ln()).floor()) as usize;
    let mut buckets = Vec::new();
    let mut current = Vec::new();
    for &n in nodes {
        current.push(n);
        if current.len() >= expected {
            buckets.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        buckets.push(current);
    }
    buckets
}

/// The five biomes of the end's island source.
#[derive(Clone, Copy, Debug)]
pub struct EndBiomes {
    pub end: BiomeId,
    pub highlands: BiomeId,
    pub midlands: BiomeId,
    pub small_islands: BiomeId,
    pub barrens: BiomeId,
}

impl EndBiomes {
    /// # Errors
    /// If the table lacks one of the five end biomes.
    pub fn from_table(t: &BiomeTable) -> Result<Self, String> {
        let get = |n: &str| t.id(n).ok_or_else(|| format!("unknown biome {n}"));
        Ok(Self {
            end: get("the_end")?,
            highlands: get("end_highlands")?,
            midlands: get("end_midlands")?,
            small_islands: get("small_end_islands")?,
            barrens: get("end_barrens")?,
        })
    }
}

/// Where a dimension's biomes come from.
#[derive(Debug)]
pub enum BiomeSource {
    MultiNoise(ClimateTree),
    End(EndBiomes),
}

/// A chunk's biomes at quart resolution.
#[derive(Clone, Debug)]
pub struct ChunkBiomes {
    pub min_quart_y: i32,
    pub quarts_y: i32,
    /// `y + (x + z * 4) * quarts_y`, with `x`/`z` local to the chunk and `y` relative to `min_quart_y`.
    pub ids: Vec<BiomeId>,
}

impl ChunkBiomes {
    #[must_use]
    pub fn get(&self, x: i32, quart_y: i32, z: i32) -> BiomeId {
        self.ids[((quart_y - self.min_quart_y) + (x + z * 4) * self.quarts_y) as usize]
    }
}

impl TerrainGenerator {
    /// The quantised climate at one quart position, sampled point by point with
    /// no caches (the path biome queries outside generation take).
    pub fn climate_at(&self, qx: i32, qy: i32, qz: i32, ctx: &mut Ctx) -> Target {
        let r = &self.router;
        let (x, y, z) = (qx.wrapping_mul(4), qy.wrapping_mul(4), qz.wrapping_mul(4));
        [r.temperature, r.vegetation, r.continents, r.erosion, r.depth, r.ridges]
            .map(|root| quantize(self.program.value(ctx, root, x, y, z)))
    }

    /// The quantised climate over a chunk's quart grid, sampled as bulk volumes
    /// through one cached context in the generator's order. Indexed like
    /// [`ChunkBiomes::ids`].
    pub fn chunk_climate(&self, chunk_x: i32, chunk_z: i32, ctx: &mut Ctx) -> Vec<Target> {
        let quarts_y = self.height >> 2;
        let volume = Volume::new([4, quarts_y, 4], [chunk_x * 16, self.min_y, chunk_z * 16], [4, 4, 4]);
        let r = &self.router;
        let n = (4 * 4 * quarts_y) as usize;
        let mut fields = Vec::with_capacity(6);
        for root in [r.temperature, r.vegetation, r.continents, r.erosion, r.depth, r.ridges] {
            let mut out = vec![0.0f32; n];
            self.program.volume(ctx, root, &mut out, &volume);
            fields.push(out);
        }
        (0..n).map(|i| [0, 1, 2, 3, 4, 5].map(|f| quantize(fields[f][i]))).collect()
    }

    /// The end source's biome for one quart position.
    fn end_biome(&self, e: &EndBiomes, qx: i32, qy: i32, qz: i32, ctx: &mut Ctx) -> BiomeId {
        let (bx, by, bz) = (qx.wrapping_mul(4), qy.wrapping_mul(4), qz.wrapping_mul(4));
        let (cx, cz) = (bx >> 4, bz >> 4);
        if i64::from(cx) * i64::from(cx) + i64::from(cz) * i64::from(cz) <= 4096 {
            return e.end;
        }
        let height = f64::from(self.program.value(ctx, self.router.erosion, (cx * 2 + 1).wrapping_mul(8), by, (cz * 2 + 1).wrapping_mul(8)));
        if height > 0.25 {
            e.highlands
        } else if height >= -0.0625 {
            e.midlands
        } else if height < -0.21875 {
            e.small_islands
        } else {
            e.barrens
        }
    }

    /// The biome at one quart position through the point resolver: uncached
    /// sampling, as for biome queries outside chunk generation.
    pub fn biome_at_quart(&self, source: &BiomeSource, cursor: &mut ClimateCursor, qx: i32, qy: i32, qz: i32, ctx: &mut Ctx) -> BiomeId {
        match source {
            BiomeSource::MultiNoise(tree) => tree.search(&self.climate_at(qx, qy, qz, ctx), cursor),
            BiomeSource::End(e) => self.end_biome(e, qx, qy, qz, ctx),
        }
    }

    /// A chunk's biomes as generation resolves them: bulk climate volumes for the
    /// multi-noise sources, the cached point resolver for the end. Cells are
    /// searched section by section (bottom-up), then x, y, z.
    pub fn chunk_biomes(&self, source: &BiomeSource, cursor: &mut ClimateCursor, chunk_x: i32, chunk_z: i32) -> ChunkBiomes {
        let quarts_y = self.height >> 2;
        let min_quart_y = self.min_y >> 2;
        let mut ctx = Ctx::new(&self.program);
        let climate = match source {
            BiomeSource::MultiNoise(_) => Some(self.chunk_climate(chunk_x, chunk_z, &mut ctx)),
            BiomeSource::End(_) => None,
        };
        let mut ids = vec![BiomeId(0); (16 * quarts_y) as usize];
        for section in 0..quarts_y / 4 {
            for x in 0..4 {
                for y in 0..4 {
                    for z in 0..4 {
                        let ly = section * 4 + y;
                        let idx = (ly + (x + z * 4) * quarts_y) as usize;
                        ids[idx] = match (source, &climate) {
                            (BiomeSource::MultiNoise(tree), Some(c)) => tree.search(&c[idx], cursor),
                            (BiomeSource::End(e), _) => self.end_biome(e, chunk_x * 4 + x, min_quart_y + ly, chunk_z * 4 + z, &mut ctx),
                            _ => unreachable!("multi-noise sources always have a climate grid"),
                        };
                    }
                }
            }
        }
        ChunkBiomes { min_quart_y, quarts_y, ids }
    }
}
