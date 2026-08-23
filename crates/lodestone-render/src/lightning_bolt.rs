//! The lightning bolt's procedural geometry — `LightningBoltRenderer.submit`
//! (26.2), transcribed.
//!
//! # What it is
//!
//! A bolt has **no geometry on the wire and no texture**. Everything visible is
//! generated from one 64-bit seed by a `java.util.Random` walk, rebuilt every
//! frame: four concentric shells around one path, three branches each, drawn as
//! hollow square tubes in a flat translucent blue-white and blended additively.
//!
//! This module is the pure half — a seed in, vertices out, no device and no
//! entity. See `docs/lightning-rendering.md` for the pass that consumes it and
//! for what is deliberately not ported.
//!
//! # The scale is real, and it surprises people
//!
//! Segment heights are `h * 16` for `h` in `0..=8`, in **blocks** — the pose
//! stack is unscaled entity space. A bolt is therefore **128 blocks tall** and
//! wanders ±5 blocks per level. Vanilla's `affectedByCulling` returns `false`
//! for exactly this reason: no hitbox comes close to bounding it, and
//! `lodestone_data::entity_dimensions` records a lightning bolt as having no
//! box at all.

/// One bolt vertex: a world-space position and a straight RGBA colour. No UV
/// and no light — the bolt is untextured and unlit, which is why it cannot
/// share `ModelVertex` or any of the entity pipelines.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoltVertex {
    /// Position **relative to the bolt's own origin**, in blocks. The caller
    /// adds the entity position; keeping it local means the walk is
    /// position-independent and a gate can assert its shape without a world.
    pub position: [f32; 3],
    /// Straight RGBA, not premultiplied. Every vertex of every bolt carries
    /// the same value — see [`BOLT_COLOR`].
    pub color: [f32; 4],
}

/// `LightningBoltRenderer.quad`'s colour, on every vertex of every quad:
/// `setColor(0.45, 0.45, 0.5, 0.3)`.
///
/// The alpha is **not** an opacity in the usual sense. Vanilla's
/// `BlendFunction.LIGHTNING` is `(SRC_ALPHA, ONE)` — additive with a
/// source-alpha scale — so `0.3` is how much of the bolt's colour each layer
/// *adds*, and four overlapping shells is what makes the core read as white
/// rather than as the dim blue-grey this triple is on its own.
///
/// Two decompiler artefacts in the source are worth naming so nobody hunts for
/// them: `float br = 0.5F` is declared and never used, and the local
/// `boltRed`/`boltGreen`/`boltBlue` are shadowed by the same literals passed
/// straight to `quad`. Neither changes a pixel.
pub const BOLT_COLOR: [f32; 4] = [0.45, 0.45, 0.5, 0.3];

/// Quads emitted for one bolt: 4 shells x 14 segments x 4 faces.
///
/// Stated as a constant because it is the cheap tell that the nested loops
/// below were transcribed with the right bounds — the segment count is
/// `8 + 3 + 3`, not `8 * 3`.
pub const BOLT_QUADS: usize = 4 * (8 + 3 + 3) * 4;

/// Triangle-list vertices one bolt emits — six per quad.
///
/// Vanilla submits `PrimitiveTopology.QUADS`, which this engine does not have,
/// so each quad becomes two triangles.
pub const BOLT_VERTICES: usize = BOLT_QUADS * 6;

/// `java.util.Random`, bit-exact, because the bolt's whole shape is its output.
///
/// # This is the fourth copy in the workspace
///
/// `lodestone_assets::entity_models`' `ghast_model` (tentacle lengths),
/// `lodestone-audio`'s `select` and `lodestone-particle`'s `rng` each carry
/// their own. Consolidating them is a real cleanup and is deliberately not done
/// here — it spans four crates, one of which was being edited concurrently.
/// Recorded rather than silently repeated, per this repo's rule about the same
/// fix being discovered twice.
///
/// The rejection loop in [`Self::next_int`] is load-bearing: 11 and 31 are not
/// powers of two, so `next(31) % bound` alone is *not* what Java produces for
/// the values this module draws.
struct JavaRandom(i64);

impl JavaRandom {
    fn new(seed: i64) -> Self {
        Self((seed ^ 0x5DEE_CE66D) & ((1i64 << 48) - 1))
    }

    fn next(&mut self, bits: u32) -> i32 {
        self.0 = self.0.wrapping_mul(0x5DEE_CE66D).wrapping_add(0xB) & ((1i64 << 48) - 1);
        (self.0 >> (48 - bits)) as i32
    }

    fn next_int(&mut self, bound: i32) -> i32 {
        if bound & -bound == bound {
            return ((i64::from(bound)).wrapping_mul(i64::from(self.next(31))) >> 31) as i32;
        }
        loop {
            let bits = self.next(31);
            let val = bits % bound;
            if bits - val + (bound - 1) >= 0 {
                return val;
            }
        }
    }
}

/// Build one bolt's whole geometry from its seed, as a triangle list of
/// exactly [`BOLT_VERTICES`] vertices in bolt-local space.
///
/// # The structure, and the one thing that reads as a bug and is not
///
/// * An **anchor pre-pass** walks eight height levels downward from `h = 7`,
///   stepping `nextInt(11) - 5` in x and z, recording each level's offset and
///   keeping the value after the eighth step as `final_x`/`final_z`. Those two
///   exist only to be subtracted, which is what lands the bottom of the trunk
///   on the entity's own origin.
/// * **Four shells** (`r` in `0..4`). The random source is **re-created from
///   the same seed inside the shell loop**, so all four trace the *identical*
///   walk and differ only in width: four nested tubes around one path, not four
///   paths. That re-seeding is the thing that reads as a transcription mistake
///   and is not — it is also why the trunk retraces the pre-pass exactly,
///   since it consumes the same sixteen draws in the same order.
/// * **Three branches** per shell: the trunk (`h` from 7 down to 0, step
///   `±5`) and two forks (`h` from 6 to 4 and from 5 to 3, step `±15`) that
///   re-anchor onto the trunk at their own height and jitter three times as
///   wide.
/// * **Four faces** per segment, forming a hollow square tube.
///
/// Only the trunk **tapers**: its half-width is scaled by `h * 0.1 + 1` at the
/// top vertex and `(h - 1) * 0.1 + 1` at the bottom, so it is widest at the sky
/// and narrowest at the strike point. The branches are untapered.
///
/// Note `rr1` pairs with the **upper** vertex (`h + 1`) and `rr2` with the
/// lower one, and that the lower vertex carries the *newly walked* offset while
/// the upper carries the previous one.
///
/// Swapping the width pair is the subtle one, and it is subtle in a way worth
/// stating because the obvious gate misses it: the bolt still tapers downward
/// either way, just by the wrong amounts (the outermost shell measures
/// `1.19`/`0.63` half-width top and bottom when correct, `1.12`/`0.70` when
/// swapped). `the_trunk_taper_matches_the_predicted_half_widths` in
/// `tests/lightning_bolt_walk.rs` asserts those exact figures rather than the
/// direction, and was written that way after the direction-only version was
/// measured passing under a deliberate swap.
#[must_use]
pub fn lightning_bolt_vertices(seed: i64) -> Vec<BoltVertex> {
    let mut x_offs = [0.0f32; 8];
    let mut z_offs = [0.0f32; 8];
    let mut x_off = 0.0f32;
    let mut z_off = 0.0f32;
    let mut random = JavaRandom::new(seed);
    for h in (0..8usize).rev() {
        x_offs[h] = x_off;
        z_offs[h] = z_off;
        x_off += (random.next_int(11) - 5) as f32;
        z_off += (random.next_int(11) - 5) as f32;
    }
    let final_x = x_off;
    let final_z = z_off;

    let mut out: Vec<BoltVertex> = Vec::with_capacity(BOLT_VERTICES);
    for r in 0..4i32 {
        let mut random = JavaRandom::new(seed);
        for p in 0..3i32 {
            let h_start = if p > 0 { 7 - p } else { 7 };
            let h_end = if p > 0 { h_start - 2 } else { 0 };
            let mut xo0 = x_offs[h_start as usize] - final_x;
            let mut zo0 = z_offs[h_start as usize] - final_z;

            for h in (h_end..=h_start).rev() {
                let xo1 = xo0;
                let zo1 = zo0;
                if p == 0 {
                    xo0 += (random.next_int(11) - 5) as f32;
                    zo0 += (random.next_int(11) - 5) as f32;
                } else {
                    xo0 += (random.next_int(31) - 15) as f32;
                    zo0 += (random.next_int(31) - 15) as f32;
                }

                let mut rr1 = 0.1 + r as f32 * 0.2;
                let mut rr2 = 0.1 + r as f32 * 0.2;
                if p == 0 {
                    rr1 *= h as f32 * 0.1 + 1.0;
                    rr2 *= (h as f32 - 1.0) * 0.1 + 1.0;
                }

                let y0 = (h * 16) as f32;
                let y1 = ((h + 1) * 16) as f32;
                // The four faces, in `submit`'s own order. Each tuple is
                // `(px1, pz1, px2, pz2)` — which corner of the square section
                // each of the quad's two edges sits on.
                for (px1, pz1, px2, pz2) in [
                    (false, false, true, false),
                    (true, false, true, true),
                    (true, true, false, true),
                    (false, true, false, false),
                ] {
                    let sign = |flag: bool, r: f32| if flag { r } else { -r };
                    let corners = [
                        [xo0 + sign(px1, rr2), y0, zo0 + sign(pz1, rr2)],
                        [xo1 + sign(px1, rr1), y1, zo1 + sign(pz1, rr1)],
                        [xo1 + sign(px2, rr1), y1, zo1 + sign(pz2, rr1)],
                        [xo0 + sign(px2, rr2), y0, zo0 + sign(pz2, rr2)],
                    ];
                    // Vanilla's QUADS topology as two triangles, wound the
                    // same way every other quad in this crate is. Winding is
                    // not load-bearing for visibility — the pass disables
                    // culling, as it must for geometry a player can stand
                    // inside — but keeping it consistent means a future
                    // culled pipeline would not silently drop half the tube.
                    for index in [0, 1, 2, 0, 2, 3] {
                        out.push(BoltVertex {
                            position: corners[index],
                            color: BOLT_COLOR,
                        });
                    }
                }
            }
        }
    }
    out
}

/// A stable per-bolt seed derived from its network entity id.
///
/// # Why this is not vanilla's seed, and why that is acceptable
///
/// `LightningBolt`'s `seed` field is a plain `public long`, **not synched**:
/// the constructor rolls `random.nextLong()` on both sides independently, so a
/// vanilla client's bolt never matches its own server's and nothing about the
/// shape is on the wire. Any seed is therefore exactly as faithful as any
/// other, and one derived from the entity id has the property a fresh roll
/// does not: it is stable for the bolt's lifetime and identical across a
/// reconnect.
///
/// **What is not ported is the re-roll.** `LightningBolt.tick` re-rolls `seed`
/// between each of its `rand(3) + 1` flashes, and that reseeding is what makes
/// a vanilla bolt visibly *flicker into a different shape*. Reproducing it
/// needs per-bolt `life`/`flashes` state on this side, which does not exist —
/// so a bolt here holds one shape for as long as the server keeps the entity
/// alive. The sky flash, which is the larger half of the effect, already
/// pulses correctly through `crate::weather`.
///
/// The mixing is `java.util.Random`'s own seed scramble applied to the id, so
/// two bolts with adjacent ids do not produce near-identical walks.
#[must_use]
pub fn bolt_seed_for_entity(entity_id: i32) -> i64 {
    (i64::from(entity_id) ^ 0x5DEE_CE66D).wrapping_mul(0x5DEE_CE66D) & ((1i64 << 48) - 1)
}
