//! A fast, non-cryptographic [`BuildHasher`] for the engine's **internal
//! lookup tables** — never for anything vanilla parity depends on.
//!
//! # Why this exists
//!
//! U17 of `docs/plans/worldgen-rewrite.md`. A `samply` profile of the release
//! bench binary at `b8763712`, `threadCPUDelta`-weighted, measured **21.01% of
//! all worldgen CPU as self time inside SipHash** — the second-largest item in
//! the pipeline, behind only `place_placed_feature::recurse`. Attributed by
//! nearest non-boring caller, four containers account for ~85% of it, all of
//! them keyed by small integers or by our own block-state names:
//!
//! | container | key | share of the hash time |
//! |---|---|---|
//! | `feature::region_view::RegionView::overlay` | `(i32, i32, i32)` | 39.5% |
//! | `crate::interner::StateInterner::ids` (in `lodestone-worldgen`) | `&'static str` | 20.8% |
//! | `overworld::decorate`'s `ocean_floor_wg` | `(i32, i32)` | 12.8% |
//! | `dense_grid::DenseBlockGrid::index_of` | `StateId` (a `u16`) | 11.8% |
//!
//! `std`'s default `RandomState` is SipHash-1-3 with a per-process random key.
//! That is the right default for a map whose keys can come from an attacker; it
//! is roughly an order of magnitude more work than these keys need. A chunk
//! coordinate is not adversarial input, and neither is `"minecraft:oak_log"`.
//!
//! # The trap this module is one half of, and why it is only half
//!
//! **Changing a hasher changes `HashMap` iteration order.** This repo has
//! shipped that bug: `overworld/mod.rs`'s module doc carries the post-mortem of
//! a `RandomState` iteration-order defect, and palette order reaches the wire
//! (`DenseBlockGrid::into_palette_and_blocks` must emit a byte-identical
//! `Vec<String>`). So this type is **not** licence to re-hash a map; it is only
//! the fast half of a two-part argument. The other half is per-map and has to be
//! established at the map:
//!
//! * a map whose iteration order is **never observed** — a pure reverse-lookup
//!   accelerator beside an ordered `Vec`, which is what `index_of` and
//!   `StateInterner::ids` are — is safe to re-hash; and
//! * a map that **is** iterated is safe only if the consumer imposes a total
//!   order of its own, which is what `RegionView::centre_writes_in_scan_order`
//!   does (it sorts by the full key, deliberately, and says so).
//!
//! Anything else — an order that feeds a palette, a seed, a draw sequence or a
//! serialised structure — must not use this. Use a `BTreeMap`, an index-keyed
//! `Vec`, or insertion-order storage instead. `FxHashMap` is a performance
//! decision; order-independence is a *separate* claim that this module cannot
//! make for you.
//!
//! # Never for parity
//!
//! [`super::md5`] and [`super::java_string_hash`] are the parity hashes: they
//! reproduce a value vanilla also computes, and their output is load-bearing.
//! This hasher's output is load-bearing for **nothing** — it may change between
//! commits without breaking a single gate, which is exactly why it must never be
//! used to derive a seed, an id, or any value that reaches the wire.
//!
//! # How it works, and how to change it
//!
//! The `FxHash` construction rustc itself uses for its interner tables: fold
//! each word into the accumulator with `(h.rotate_left(5) ^ word) * K` for an
//! odd 64-bit constant `K`. `finish` returns the accumulator **unrotated**, and
//! that is a deliberate, measured choice rather than an omission.
//!
//! `hashbrown` splits a hash two ways: the bucket index is the **low** bits
//! (`h1(hash) = hash as usize & bucket_mask`) and the control byte is the **top
//! 7** (`h2(hash) = hash >> (bits - 7)`). A bare multiply by an odd constant is
//! ideal for *both* at once:
//!
//! * the top bits are where a multiply mixes best, which is the control byte; and
//! * multiplication by an odd constant is a **bijection modulo `2^n`**, so the
//!   low `n` bits of the hash are a permutation of the low `n` bits of the key.
//!   `StateId`s are handed out `0, 1, 2, …`, so a `StateId`-keyed table collides
//!   **never** rather than rarely.
//!
//! An earlier draft of this file ended `finish` with `rotate_left(20)`, copying
//! `rustc-hash`'s shape without checking whether it helped *here*. It does not:
//! the rotation moves bits 44..56 into the bucket index and destroys the
//! bijection. Measured by `sequential_u16_keys_are_collision_free_in_the_low_bits`
//! below — 4096 sequential keys into 4096 buckets occupy **3931** distinct
//! buckets with the rotation and **4096** without. (3931 is still far better than
//! the ~2589 a uniformly random hash would give, which is precisely why this
//! would have survived review as "fine".) Multi-word keys are unaffected either
//! way, because their low bits already depend on every word through the
//! `rotate_left(5)` fold — asserted, with a coordinate-dropping negative control,
//! by `dense_3d_positions_spread_across_buckets`.
//!
//! Hand-rolled rather than pulled from `rustc-hash` on purpose: the crate would
//! be a `Cargo.lock` edit in a shared checkout for ~30 lines of arithmetic that
//! has to be documented here anyway, and this crate is inside the wasm-confined
//! set (`cargo xtask check-isolation`), where a zero-dependency module cannot
//! regress anything.

use std::hash::{BuildHasherDefault, Hasher};

/// `FxHash`'s 64-bit multiplier — odd, full-width, and the same constant
/// `rustc-hash` uses. Oddness is load-bearing (see the module doc).
const K: u64 = 0x517c_c1b7_2722_0a95;

/// A [`Hasher`] roughly an order of magnitude cheaper than SipHash-1-3, for
/// non-adversarial keys only. See the module doc before using it.
#[derive(Debug, Clone, Copy, Default)]
pub struct FastHasher {
    hash: u64,
}

impl FastHasher {
    #[inline]
    fn add(&mut self, word: u64) {
        self.hash = (self.hash.rotate_left(5) ^ word).wrapping_mul(K);
    }
}

impl Hasher for FastHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let mut rest = bytes;
        while let Some((chunk, tail)) = rest.split_first_chunk::<8>() {
            self.add(u64::from_le_bytes(*chunk));
            rest = tail;
        }
        if !rest.is_empty() {
            let mut buf = [0u8; 8];
            buf[..rest.len()].copy_from_slice(rest);
            self.add(u64::from_le_bytes(buf));
        }
        // The length must participate, or `"a"` and `"a\0"` hash equal — the
        // zero-padded tail above cannot otherwise distinguish them.
        self.add(bytes.len() as u64);
    }

    // Integer keys are the whole point: each of these is one multiply, where the
    // default `Hasher` impl would route through `write` and pay a length fold.
    #[inline]
    fn write_u8(&mut self, i: u8) {
        self.add(u64::from(i));
    }
    #[inline]
    fn write_u16(&mut self, i: u16) {
        self.add(u64::from(i));
    }
    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.add(u64::from(i));
    }
    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.add(i);
    }
    #[inline]
    fn write_u128(&mut self, i: u128) {
        self.add(i as u64);
        self.add((i >> 64) as u64);
    }
    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.add(i as u64);
    }
    #[inline]
    fn write_i8(&mut self, i: i8) {
        self.write_u8(i as u8);
    }
    #[inline]
    fn write_i16(&mut self, i: i16) {
        self.write_u16(i as u16);
    }
    #[inline]
    fn write_i32(&mut self, i: i32) {
        self.write_u32(i as u32);
    }
    #[inline]
    fn write_i64(&mut self, i: i64) {
        self.write_u64(i as u64);
    }
    #[inline]
    fn write_isize(&mut self, i: isize) {
        self.write_usize(i as usize);
    }

    #[inline]
    fn finish(&self) -> u64 {
        // Deliberately unrotated: `hashbrown` indexes buckets with the low bits,
        // and multiplication by an odd `K` makes those a bijection of the key's
        // low bits. Rotating here measurably *loses* that. See the module doc.
        self.hash
    }
}

/// [`FastHasher`]'s `BuildHasher`. Stateless and seedless — two runs of the
/// process hash identically, unlike `RandomState`.
pub type FastBuildHasher = BuildHasherDefault<FastHasher>;

/// A `HashMap` using [`FastHasher`]. **Read the module doc's ordering argument
/// before switching a map to this.**
pub type FastMap<K, V> = std::collections::HashMap<K, V, FastBuildHasher>;

/// A `HashSet` using [`FastHasher`]. Same ordering caveat as [`FastMap`].
pub type FastSet<T> = std::collections::HashSet<T, FastBuildHasher>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::hash::{BuildHasher, Hash};

    fn h<T: Hash>(v: &T) -> u64 {
        FastBuildHasher::default().hash_one(v)
    }

    /// The property `write`'s length fold exists for. Without it these collide,
    /// and the control is that they collide when the fold is removed — which is
    /// checked by construction here: both inputs are ≤ 8 bytes, so they differ
    /// *only* in the length word.
    #[test]
    fn zero_padding_does_not_alias_a_shorter_key() {
        assert_ne!(h(&"a"), h(&"a\0"));
        assert_ne!(h(&"minecraft:air"), h(&"minecraft:air\0"));
        // Same shape one word up, so the multi-chunk path is covered too.
        assert_ne!(h(&"abcdefgh"), h(&"abcdefgh\0"));
    }

    /// The bijection property the module doc claims for sequential integer keys.
    /// `StateId`s are handed out `0, 1, 2, …`, and this is the reason that costs
    /// zero collisions rather than a few. Asserted over the low 12 bits, i.e. a
    /// 4096-bucket table.
    #[test]
    fn sequential_u16_keys_are_collision_free_in_the_low_bits() {
        const N: u64 = 4096;
        let seen: std::collections::HashSet<u64> =
            (0u16..N as u16).map(|k| h(&k) & (N - 1)).collect();
        assert_eq!(
            seen.len(),
            N as usize,
            "multiplication by an odd constant must permute the low bits; \
             {} of {N} sequential keys shared a bucket",
            N as usize - seen.len()
        );
    }

    /// A coordinate-key spread check with an expectation derived from outside
    /// the hasher: for `n` keys in `m` buckets the expected number of occupied
    /// buckets is `m * (1 - (1 - 1/m)^n)`. A hash that ignored a coordinate —
    /// the failure mode that matters, since these keys are dense 3-D positions —
    /// cannot reach it. The negative control below proves the bound discriminates.
    #[test]
    fn dense_3d_positions_spread_across_buckets() {
        const BITS: u32 = 12;
        const M: f64 = (1u64 << BITS) as f64;
        let keys: Vec<(i32, i32, i32)> =
            (0..16).flat_map(|x| (0..16).flat_map(move |y| (0..16).map(move |z| (x, y, z)))).collect();
        let n = keys.len() as f64;
        let expected = M * (1.0 - (1.0 - 1.0 / M).powf(n));

        let occupied = keys
            .iter()
            .map(|k| h(k) & ((1 << BITS) - 1))
            .collect::<std::collections::HashSet<_>>()
            .len() as f64;
        assert!(
            occupied >= expected * 0.95,
            "occupied {occupied} buckets against an expected {expected:.1} for {n} keys"
        );

        // Negative control: the same bound applied to a hasher that drops `z`
        // must FAIL, or the assertion above is measuring nothing. 4096 keys
        // collapse to 256 distinct hashes.
        let degenerate = keys
            .iter()
            .map(|&(x, y, _)| FastBuildHasher::default().hash_one((x, y)) & ((1 << BITS) - 1))
            .collect::<std::collections::HashSet<_>>()
            .len() as f64;
        assert!(
            degenerate < expected * 0.95,
            "the coordinate-dropping control scored {degenerate}, which PASSES the \
             bound {:.1} — the spread assertion above therefore proves nothing",
            expected * 0.95
        );
    }

    /// Seedless, unlike `RandomState`: the same key hashes the same in two
    /// independently constructed builders. Cheap, and the property every
    /// determinism gate downstream leans on.
    #[test]
    fn the_build_hasher_is_seedless() {
        let a = FastBuildHasher::default().hash_one((1i32, -2i32, 3i32));
        let b = FastBuildHasher::default().hash_one((1i32, -2i32, 3i32));
        assert_eq!(a, b);
    }

    /// A `FastMap` is a `HashMap` — same semantics, only the hasher differs.
    #[test]
    fn fast_map_behaves_as_a_hash_map() {
        let mut fast: FastMap<(i32, i32, i32), u16> = FastMap::default();
        let mut std_map: HashMap<(i32, i32, i32), u16> = HashMap::new();
        for x in -8..8 {
            for z in -8..8 {
                fast.insert((x, 0, z), (x * 31 + z) as u16);
                std_map.insert((x, 0, z), (x * 31 + z) as u16);
            }
        }
        assert_eq!(fast.len(), std_map.len());
        for (k, v) in &std_map {
            assert_eq!(fast.get(k), Some(v));
        }
        assert_eq!(fast.remove(&(0, 0, 0)), std_map.remove(&(0, 0, 0)));
        assert_eq!(fast.len(), std_map.len());
    }
}
