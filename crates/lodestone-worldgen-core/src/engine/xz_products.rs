//! Request-scoped sharing for the exact X/Z-only terrain products.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::density::Density;

/// One recognized terrain product.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XzProductKind {
    Factor,
    Offset,
}

/// A bounded rectangle in quart-grid coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct XzRect {
    pub min_qx: i32,
    pub min_qz: i32,
    pub width: usize,
    pub depth: usize,
}

impl XzRect {
    #[must_use]
    pub fn new(min_qx: i32, min_qz: i32, width: usize, depth: usize) -> Self {
        assert!(
            width != 0 && depth != 0,
            "X/Z product rectangle cannot be empty"
        );
        assert!(
            width.checked_mul(depth).is_some(),
            "X/Z product rectangle is too large"
        );
        Self {
            min_qx,
            min_qz,
            width,
            depth,
        }
    }

    #[inline]
    pub(crate) fn index(self, qx: i32, qz: i32) -> Option<usize> {
        let x = usize::try_from(qx.checked_sub(self.min_qx)?).ok()?;
        let z = usize::try_from(qz.checked_sub(self.min_qz)?).ok()?;
        (x < self.width && z < self.depth).then_some(z * self.width + x)
    }
}

/// Versioned structural identity for the factor/offset pair shared by the
/// evaluators.
///
/// The identity is deliberately retained in full. A compact numeric token is
/// useful in diagnostics, but it is not sufficient to admit a lattice: two
/// route shapes are allowed to share a token without ever sharing this value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XzProductIdentity {
    version: u32,
    seed: i64,
    factor_signature: Arc<[u64]>,
    offset_signature: Arc<[u64]>,
    preliminary_signature: Arc<[u64]>,
    final_signature: Arc<[u64]>,
}

static IDENTITY_REGISTRY: OnceLock<Mutex<BTreeMap<u64, Vec<XzProductIdentity>>>> = OnceLock::new();

fn register_identity(identity: &XzProductIdentity) {
    let token = identity.diagnostic_token();
    let registry = IDENTITY_REGISTRY.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut registry = registry.lock().expect("X/Z product identity registry poisoned");
    let candidates = registry.entry(token).or_default();
    if !candidates.iter().any(|candidate| candidate == identity) {
        candidates.push(identity.clone());
    }
}

fn identity_for_token(token: u64) -> Option<XzProductIdentity> {
    IDENTITY_REGISTRY
        .get()
        .and_then(|registry| registry.lock().ok())
        .and_then(|registry| {
            registry
                .get(&token)
                .filter(|candidates| candidates.len() == 1)
                .cloned()
        })
        .and_then(|mut candidates| candidates.pop())
}

impl XzProductIdentity {
    const VERSION: u32 = 1;

    fn new(
        seed: i64,
        factor_signature: Vec<u64>,
        offset_signature: Vec<u64>,
        preliminary_signature: Vec<u64>,
        final_signature: Vec<u64>,
    ) -> Self {
        Self {
            version: Self::VERSION,
            seed,
            factor_signature: factor_signature.into_boxed_slice().into(),
            offset_signature: offset_signature.into_boxed_slice().into(),
            preliminary_signature: preliminary_signature.into_boxed_slice().into(),
            final_signature: final_signature.into_boxed_slice().into(),
        }
    }

    /// Stable numeric token retained for diagnostics and compatibility with
    /// older callers. Runtime reuse checks compare [`Self`] directly.
    #[must_use]
    pub fn diagnostic_token(&self) -> u64 {
        // FNV-1a is intentionally only a display/cache-stat token here. It is
        // never consulted by lattice admission; the complete typed identity
        // is compared there.
        let mut token = 0xcbf2_9ce4_8422_2325_u64;
        for value in [
            u64::from(self.version),
            self.seed as u64,
            self.factor_signature.len() as u64,
        ] {
            token ^= value;
            token = token.wrapping_mul(0x1000_0000_01b3);
        }
        for signature in [
            &self.factor_signature,
            &self.offset_signature,
            &self.preliminary_signature,
            &self.final_signature,
        ] {
            for &value in signature.iter() {
                token ^= value;
                token = token.wrapping_mul(0x1000_0000_01b3);
            }
            token ^= 0xff;
            token = token.wrapping_mul(0x1000_0000_01b3);
        }
        token
    }
}

/// Structural identity for the factor/offset pair shared by the evaluators.
///
/// The preliminary route produces both values, while a final-density route may
/// consume only the factor.  The manifest is therefore the union of the
/// admitted products; each compiled program records the subset it actually
/// contains.
#[derive(Clone, Debug)]
pub struct XzProductManifest {
    factor_signature: Vec<u64>,
    offset_signature: Vec<u64>,
    identity: XzProductIdentity,
}

impl XzProductManifest {
    /// Recognizes pure products when the preliminary route contains the pair
    /// and the final route contains at least one member of it. Wrapper kinds
    /// are intentionally omitted from density signatures: point `cache_2d` and
    /// field `flat_cache` therefore match the same function.
    #[must_use]
    pub fn from_routes(
        seed: i64,
        preliminary: &Density,
        final_density: &Density,
        factor: &Density,
        offset: &Density,
    ) -> Option<Self> {
        if !factor.is_xz_pure() || !offset.is_xz_pure() {
            return None;
        }
        let factor_signature = product_signature(factor);
        let offset_signature = product_signature(offset);
        let preliminary_factor = has_wrapped_signature(preliminary, &factor_signature);
        let preliminary_offset = has_wrapped_signature(preliminary, &offset_signature);
        let final_factor = has_wrapped_signature(final_density, &factor_signature);
        let final_offset = has_wrapped_signature(final_density, &offset_signature);
        if factor_signature == offset_signature
            || !preliminary_factor
            || !preliminary_offset
            || (!final_factor && !final_offset)
        {
            return None;
        }
        let preliminary_signature = signature(preliminary);
        let final_signature = signature(final_density);
        let identity = XzProductIdentity::new(
            seed,
            factor_signature.clone(),
            offset_signature.clone(),
            preliminary_signature,
            final_signature,
        );
        register_identity(&identity);
        Some(Self {
            identity,
            factor_signature,
            offset_signature,
        })
    }

    /// The complete route identity used to admit request-scoped products.
    #[must_use]
    pub fn identity(&self) -> XzProductIdentity {
        self.identity.clone()
    }

    /// A stable numeric token for diagnostics and compatibility only.
    #[must_use]
    pub fn fingerprint(&self) -> u64 {
        // Kept as a method rather than a field so the structural identity is
        // the sole source of truth. This value is never used for admission.
        self.identity.diagnostic_token()
    }

    pub(crate) fn kind_for(&self, inner: &Density) -> Option<XzProductKind> {
        if !inner.is_xz_pure() {
            return None;
        }
        let sig = signature(inner);
        if sig == self.factor_signature {
            Some(XzProductKind::Factor)
        } else if sig == self.offset_signature {
            Some(XzProductKind::Offset)
        } else {
            None
        }
    }
}

fn signature(density: &Density) -> Vec<u64> {
    let mut out = Vec::new();
    density.write_signature(&mut out);
    out
}

/// The product wrapper itself is an admission boundary, not part of the
/// product definition. Keep nested wrappers in the signature because they are
/// part of the exact point walk, but ignore the outer wrapper that identifies
/// where a route exposes the value.
fn product_signature(density: &Density) -> Vec<u64> {
    match density {
        Density::FlatCache { inner, .. } | Density::Cache2D { inner, .. } => signature(inner),
        _ => signature(density),
    }
}

fn has_wrapped_signature(density: &Density, target: &[u64]) -> bool {
    match density {
        Density::FlatCache { inner, .. } | Density::Cache2D { inner, .. } => {
            let mut sig = Vec::new();
            inner.write_signature(&mut sig);
            (inner.is_xz_pure() && sig == target) || has_wrapped_signature(inner, target)
        }
        Density::Const(_)
        | Density::BlendAlpha
        | Density::BlendOffset
        | Density::Beardifier
        | Density::YClampedGradient { .. }
        | Density::Noise { .. }
        | Density::ShiftA(_)
        | Density::ShiftB(_)
        | Density::Shift(_)
        | Density::Spline(_)
        | Density::Blended(_)
        | Density::EndIslands(_) => false,
        Density::Add(a, b)
        | Density::Mul(a, b)
        | Density::Min(a, b)
        | Density::Max(a, b) => has_wrapped_signature(a, target) || has_wrapped_signature(b, target),
        Density::Abs(a)
        | Density::Square(a)
        | Density::Cube(a)
        | Density::HalfNegative(a)
        | Density::QuarterNegative(a)
        | Density::Squeeze(a)
        | Density::Invert(a)
        | Density::Marker(a) => has_wrapped_signature(a, target),
        Density::Clamp { input, .. } | Density::Interpolated { inner: input, .. } => {
            has_wrapped_signature(input, target)
        }
        Density::ShiftedNoise {
            shift_x,
            shift_y,
            shift_z,
            ..
        } => {
            has_wrapped_signature(shift_x, target)
                || has_wrapped_signature(shift_y, target)
                || has_wrapped_signature(shift_z, target)
        }
        Density::RangeChoice {
            input,
            when_in_range,
            when_out_of_range,
            ..
        } => {
            has_wrapped_signature(input, target)
                || has_wrapped_signature(when_in_range, target)
                || has_wrapped_signature(when_out_of_range, target)
        }
        Density::IntervalSelect {
            input, functions, ..
        } => {
            has_wrapped_signature(input, target)
                || functions.iter().any(|f| has_wrapped_signature(f, target))
        }
        Density::FindTopSurface {
            density,
            upper_bound,
            ..
        } => {
            has_wrapped_signature(density, target)
                || has_wrapped_signature(upper_bound, target)
        }
    }
}

/// Dense, request-scoped factor/offset values. A missing cell is intentional:
/// sparse admitted positions and aquifer-only tails fall back to the old route.
#[derive(Debug)]
pub struct XzProductLattice {
    rect: XzRect,
    identity: Option<XzProductIdentity>,
    diagnostic_token: u64,
    factor: Vec<f64>,
    offset: Vec<f64>,
    present: Vec<bool>,
    computes: AtomicU64,
    hits: AtomicU64,
    misses: AtomicU64,
}

impl XzProductLattice {
    #[must_use]
    pub fn new(rect: XzRect, fingerprint: u64) -> Self {
        Self::new_legacy(rect, fingerprint)
    }

    /// Constructs a lattice carrying the complete product identity.
    #[must_use]
    pub fn new_with_identity(rect: XzRect, identity: XzProductIdentity) -> Self {
        let diagnostic_token = identity.diagnostic_token();
        Self::with_identity(rect, identity, diagnostic_token)
    }

    fn new_legacy(rect: XzRect, diagnostic_token: u64) -> Self {
        // Kept for source compatibility with older diagnostic/test callers.
        // Resolve the token only when the process has one unambiguous full
        // identity for it. A collision deliberately disables reuse.
        Self::with_identity_token(
            rect,
            identity_for_token(diagnostic_token),
            diagnostic_token,
        )
    }

    fn with_identity(
        rect: XzRect,
        identity: XzProductIdentity,
        diagnostic_token: u64,
    ) -> Self {
        Self::with_identity_token(rect, Some(identity), diagnostic_token)
    }

    fn with_identity_token(
        rect: XzRect,
        identity: Option<XzProductIdentity>,
        diagnostic_token: u64,
    ) -> Self {
        let len = rect.width * rect.depth;
        Self {
            rect,
            identity,
            diagnostic_token,
            factor: vec![0.0; len],
            offset: vec![0.0; len],
            present: vec![false; len],
            computes: AtomicU64::new(0),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    #[must_use]
    pub const fn rect(&self) -> XzRect {
        self.rect
    }

    #[must_use]
    pub fn fingerprint(&self) -> u64 {
        self.diagnostic_token
    }

    /// Whether this lattice was built for exactly this route identity.
    #[must_use]
    pub fn identity_matches(&self, identity: &XzProductIdentity) -> bool {
        let Some(candidate) = self.identity.as_ref() else {
            return false;
        };
        if candidate.version == identity.version
            && candidate.seed == identity.seed
            && Arc::ptr_eq(&candidate.factor_signature, &identity.factor_signature)
            && Arc::ptr_eq(&candidate.offset_signature, &identity.offset_signature)
            && Arc::ptr_eq(&candidate.preliminary_signature, &identity.preliminary_signature)
            && Arc::ptr_eq(&candidate.final_signature, &identity.final_signature)
        {
            return true;
        }
        candidate == identity
    }

    /// Inserts one pair at a block coordinate. Re-inserting the same key is a
    /// bitwise parity assertion rather than a silent overwrite.
    pub fn insert_pair(&mut self, x: i32, z: i32, factor: f64, offset: f64) {
        let (qx, qz) = aligned_quart(x, z);
        let index = self
            .rect
            .index(qx, qz)
            .expect("X/Z product insertion escaped its bounded lattice");
        if self.present[index] {
            assert_eq!(self.factor[index].to_bits(), factor.to_bits());
            assert_eq!(self.offset[index].to_bits(), offset.to_bits());
            return;
        }
        self.factor[index] = factor;
        self.offset[index] = offset;
        self.present[index] = true;
        self.computes.fetch_add(1, Ordering::Relaxed);
    }

    #[must_use]
    pub fn get(&self, kind: XzProductKind, x: i32, z: i32) -> Option<f64> {
        let Some((qx, qz)) = aligned_quart_checked(x, z) else {
            self.misses.fetch_add(1, Ordering::Relaxed);
            return None;
        };
        let Some(index) = self.rect.index(qx, qz) else {
            self.misses.fetch_add(1, Ordering::Relaxed);
            return None;
        };
        if !self.present[index] {
            self.misses.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        self.hits.fetch_add(1, Ordering::Relaxed);
        Some(match kind {
            XzProductKind::Factor => self.factor[index],
            XzProductKind::Offset => self.offset[index],
        })
    }

    #[must_use]
    pub fn get_pair(&self, x: i32, z: i32) -> Option<(f64, f64)> {
        let Some((qx, qz)) = aligned_quart_checked(x, z) else {
            self.misses.fetch_add(1, Ordering::Relaxed);
            return None;
        };
        let Some(index) = self.rect.index(qx, qz) else {
            self.misses.fetch_add(1, Ordering::Relaxed);
            return None;
        };
        if !self.present[index] {
            self.misses.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        self.hits.fetch_add(1, Ordering::Relaxed);
        Some((self.factor[index], self.offset[index]))
    }

    #[must_use]
    pub fn computes(&self) -> u64 {
        self.computes.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn hits(&self) -> u64 {
        self.hits.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn misses(&self) -> u64 {
        self.misses.load(Ordering::Relaxed)
    }
}

fn aligned_quart(x: i32, z: i32) -> (i32, i32) {
    aligned_quart_checked(x, z).expect("X/Z product coordinates must be four-block aligned")
}

fn aligned_quart_checked(x: i32, z: i32) -> Option<(i32, i32)> {
    (x.rem_euclid(4) == 0 && z.rem_euclid(4) == 0).then_some((x / 4, z / 4))
}

#[cfg(test)]
mod tests {
    use super::{XzProductKind, XzProductLattice, XzProductManifest, XzRect};
    use crate::density::{Density, NoiseChunkSampler, XzMemoId};
    use crate::engine::{Bounds, Program};
    use crate::noise::NormalNoise;
    use crate::rng::{Algorithm, PositionalRandomFactory};

    fn cached(inner: Density, _slot: usize) -> Density {
        Density::Cache2D {
            inner: Box::new(inner),
            memo: XzMemoId::allocate(),
        }
    }

    fn flat(inner: Density, slot: usize) -> Density {
        Density::FlatCache {
            inner: Box::new(inner),
            slot,
            memo: XzMemoId::NONE,
        }
    }

    fn xz_noise(seed: i64, name: &str) -> Density {
        let mut source = Algorithm::Xoroshiro
            .root_positional(seed)
            .from_hash_of(name);
        Density::ShiftA(NormalNoise::create(&mut source, -1, &[1.0, 1.0]))
    }

    #[test]
    fn lattice_counters_and_bitwise_values_are_exact() {
        let mut lattice = XzProductLattice::new(XzRect::new(-1, -1, 3, 3), 7);
        let factor = f64::from_bits(0x3ff0_0000_0000_0001);
        let offset = f64::from_bits(0x8000_0000_0000_0001);
        lattice.insert_pair(0, 0, factor, offset);
        lattice.insert_pair(0, 0, factor, offset);
        assert_eq!(lattice.computes(), 1);
        assert_eq!(
            lattice
                .get(XzProductKind::Factor, 0, 0)
                .unwrap()
                .to_bits(),
            factor.to_bits()
        );
        assert_eq!(
            lattice
                .get(XzProductKind::Offset, 0, 0)
                .unwrap()
                .to_bits(),
            offset.to_bits()
        );
        assert!(lattice.get_pair(2, 0).is_none());
        assert!(lattice.get_pair(40, 0).is_none());
        assert_eq!(lattice.hits(), 2);
        assert_eq!(lattice.misses(), 2);
    }

    #[test]
    fn manifest_admits_route_subsets_but_rejects_mismatched_products() {
        let factor = Density::Const(1.0);
        let offset = Density::Const(2.0);
        let preliminary = Density::FindTopSurface {
            density: Box::new(cached(factor.clone(), 0)),
            upper_bound: Box::new(cached(offset.clone(), 1)),
            lower_bound: 0,
            cell_height: 8,
        };
        let final_density = Density::Add(
            Box::new(cached(factor.clone(), 2)),
            Box::new(cached(offset.clone(), 3)),
        );
        let manifest = XzProductManifest::from_routes(
            42,
            &preliminary,
            &final_density,
            &factor,
            &offset,
        );
        assert!(manifest.is_some());

        let y_sensitive = Density::YClampedGradient {
            from_y: 0.0,
            to_y: 8.0,
            from_value: 0.0,
            to_value: 1.0,
        };
        assert!(XzProductManifest::from_routes(
            42,
            &preliminary,
            &final_density,
            &y_sensitive,
            &offset,
        )
        .is_none());
        let factor_only = Density::Add(
            Box::new(cached(factor.clone(), 4)),
            Box::new(Density::Const(0.0)),
        );
        assert!(XzProductManifest::from_routes(
            42,
            &preliminary,
            &factor_only,
            &factor,
            &offset,
        )
        .is_some());

        let no_product_route = Density::Const(0.0);
        assert!(XzProductManifest::from_routes(
            42,
            &preliminary,
            &no_product_route,
            &factor,
            &offset,
        )
        .is_none());
    }

    #[test]
    fn field_lattice_hit_is_bit_exact_and_mismatch_falls_back() {
        let factor = xz_noise(42, "product-factor");
        let offset = xz_noise(42, "product-offset");
        let preliminary = Density::Add(
            Box::new(cached(factor.clone(), 0)),
            Box::new(cached(offset.clone(), 1)),
        );
        let final_density = Density::Add(
            Box::new(flat(factor.clone(), 2)),
            Box::new(flat(offset.clone(), 3)),
        );
        let manifest = XzProductManifest::from_routes(
            42,
            &preliminary,
            &final_density,
            &factor,
            &offset,
        )
        .expect("test routes must admit the pure product pair");
        let program = Program::compile_with_xz_products(&final_density, manifest);
        let fingerprint = program
            .xz_product_fingerprint()
            .expect("compiled field route must retain its product identity");
        let mut lattice = XzProductLattice::new(XzRect::new(0, 0, 1, 1), fingerprint);
        let factor_bits = factor.compute(crate::density::Context::new(0, 0, 0)).to_bits();
        let offset_bits = offset.compute(crate::density::Context::new(0, 0, 0)).to_bits();
        assert_ne!(
            factor.compute(crate::density::Context::new(4, 0, 0)).to_bits(),
            factor_bits,
            "the coordinate-sensitive control did not cross a product tile"
        );
        lattice.insert_pair(0, 0, f64::from_bits(factor_bits), f64::from_bits(offset_bits));
        let lattice = std::sync::Arc::new(lattice);
        let optimized = NoiseChunkSampler::from_program_with_xz_products(
            program,
            4,
            4,
            8,
            Some(Bounds {
                x: (0, 3),
                y: (0, 7),
                z: (0, 3),
            }),
            Some(std::sync::Arc::clone(&lattice)),
        );
        let baseline = NoiseChunkSampler::from_program(
            Program::compile(&final_density),
            4,
            4,
            8,
            Some(Bounds {
                x: (0, 3),
                y: (0, 7),
                z: (0, 3),
            }),
        );
        for &(x, y, z) in &[(0, 0, 0), (3, 7, 3)] {
            assert_eq!(
                optimized.final_density(x, y, z).to_bits(),
                baseline.final_density(x, y, z).to_bits(),
                "product hit changed field output at ({x},{y},{z})",
            );
        }

        let mismatched = std::sync::Arc::new(XzProductLattice::new(
            XzRect::new(0, 0, 1, 1),
            fingerprint.wrapping_add(1),
        ));
        let mismatch_sampler = NoiseChunkSampler::from_program_with_xz_products(
            Program::compile_with_xz_products(
                &final_density,
                XzProductManifest::from_routes(
                    42,
                    &preliminary,
                    &final_density,
                    &factor,
                    &offset,
                )
                .expect("test routes must admit the pure product pair"),
            ),
            4,
            4,
            8,
            Some(Bounds {
                x: (0, 3),
                y: (0, 7),
                z: (0, 3),
            }),
            Some(mismatched),
        );
        assert_eq!(
            mismatch_sampler.final_density(0, 0, 0).to_bits(),
            baseline.final_density(0, 0, 0).to_bits(),
            "fingerprint mismatch must retain the exact fallback"
        );

        #[cfg(feature = "gen-counters")]
        {
            let bounds = Bounds {
                x: (0, 3),
                y: (0, 7),
                z: (0, 3),
            };
            let baseline_control = NoiseChunkSampler::from_program(
                Program::compile(&final_density),
                4,
                4,
                8,
                Some(bounds),
            );
            let optimized_control = NoiseChunkSampler::from_program_with_xz_products(
                Program::compile_with_xz_products(
                    &final_density,
                    XzProductManifest::from_routes(
                        42,
                        &preliminary,
                        &final_density,
                        &factor,
                        &offset,
                    )
                    .expect("test routes must admit the pure product pair"),
                ),
                4,
                4,
                8,
                Some(bounds),
                Some(std::sync::Arc::clone(&lattice)),
            );
            let mismatch_control = NoiseChunkSampler::from_program_with_xz_products(
                Program::compile_with_xz_products(
                    &final_density,
                    XzProductManifest::from_routes(
                        42,
                        &preliminary,
                        &final_density,
                        &factor,
                        &offset,
                    )
                    .expect("test routes must admit the pure product pair"),
                ),
                4,
                4,
                8,
                Some(bounds),
                Some(std::sync::Arc::new(XzProductLattice::new(
                    XzRect::new(0, 0, 1, 1),
                    fingerprint.wrapping_add(1),
                ))),
            );
            crate::counters::reset();
            let _ = baseline_control.final_density(0, 0, 0);
            let baseline_visits = crate::counters::snapshot().density_evals_total();
            crate::counters::reset();
            let _ = optimized_control.final_density(0, 0, 0);
            let optimized_visits = crate::counters::snapshot().density_evals_total();
            assert_eq!(baseline_visits, 5, "baseline shape visit control changed");
            assert_eq!(optimized_visits, 3, "product hit did not bypass both children");
            assert!(optimized_visits < baseline_visits);

            crate::counters::reset();
            let _ = mismatch_control.final_density(0, 0, 0);
            assert_eq!(
                crate::counters::snapshot().density_evals_total(),
                baseline_visits,
                "negative fingerprint control unexpectedly used the lattice"
            );
        }
    }

    /// Darwin-only before/after control for the admitted two-wrapper shape.
    /// Run in release with `--ignored --nocapture`: the reported instruction
    /// and cycle counts are process-local measurements, while the visit counts
    /// above are the portable acceptance criterion.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "local instruction/cycle control for the X/Z product lattice"]
    #[allow(unsafe_code)]
    fn field_lattice_instruction_cycle_control() {
        use std::hint::black_box;
        use std::mem::size_of;

        #[repr(C)]
        #[derive(Default, Clone, Copy)]
        #[allow(non_snake_case, dead_code)]
        struct RusageInfoV4 {
            ri_uuid: [u8; 16],
            ri_user_time: u64,
            ri_system_time: u64,
            ri_pkg_idle_wkups: u64,
            ri_interrupt_wkups: u64,
            ri_pageins: u64,
            ri_wired_size: u64,
            ri_resident_size: u64,
            ri_phys_footprint: u64,
            ri_proc_start_abstime: u64,
            ri_proc_exit_abstime: u64,
            ri_child_user_time: u64,
            ri_child_system_time: u64,
            ri_child_pkg_idle_wkups: u64,
            ri_child_interrupt_wkups: u64,
            ri_child_pageins: u64,
            ri_child_elapsed_abstime: u64,
            ri_diskio_bytesread: u64,
            ri_diskio_byteswritten: u64,
            ri_cpu_time_qos_default: u64,
            ri_cpu_time_qos_maintenance: u64,
            ri_cpu_time_qos_background: u64,
            ri_cpu_time_qos_utility: u64,
            ri_cpu_time_qos_legacy: u64,
            ri_cpu_time_qos_user_initiated: u64,
            ri_cpu_time_qos_user_interactive: u64,
            ri_billed_system_time: u64,
            ri_serviced_system_time: u64,
            ri_logical_writes: u64,
            ri_lifetime_max_phys_footprint: u64,
            ri_instructions: u64,
            ri_cycles: u64,
            ri_billed_energy: u64,
            ri_serviced_energy: u64,
            ri_interval_max_phys_footprint: u64,
            ri_runnable_time: u64,
            ri_flags: u64,
        }

        const RUSAGE_INFO_V4: i32 = 4;
        const RUSAGE_INFO_V4_SIZE: usize = 16 + 36 * 8;

        unsafe extern "C" {
            fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut core::ffi::c_void) -> i32;
        }

        fn usage() -> (u64, u64) {
            assert_eq!(size_of::<RusageInfoV4>(), RUSAGE_INFO_V4_SIZE);
            let mut info = RusageInfoV4::default();
            let result = unsafe {
                proc_pid_rusage(
                    i32::try_from(std::process::id()).expect("pid fits in i32"),
                    RUSAGE_INFO_V4,
                    (&raw mut info).cast::<core::ffi::c_void>(),
                )
            };
            assert_eq!(result, 0, "proc_pid_rusage failed with {result}");
            (info.ri_instructions, info.ri_cycles)
        }

        const SIDE: usize = 128;
        let factor = xz_noise(42, "measured-product-factor");
        let offset = xz_noise(42, "measured-product-offset");
        let preliminary = Density::Add(
            Box::new(cached(factor.clone(), 0)),
            Box::new(cached(offset.clone(), 1)),
        );
        let final_density = Density::Add(
            Box::new(flat(factor.clone(), 2)),
            Box::new(flat(offset.clone(), 3)),
        );
        let manifest = XzProductManifest::from_routes(
            42,
            &preliminary,
            &final_density,
            &factor,
            &offset,
        )
        .expect("test routes must admit the pure product pair");
        let optimized_program = Program::compile_with_xz_products(&final_density, manifest);
        let fingerprint = optimized_program
            .xz_product_fingerprint()
            .expect("compiled route must retain its product identity");
        let mut lattice = XzProductLattice::new(XzRect::new(0, 0, SIDE, SIDE), fingerprint);
        let mut coords = Vec::with_capacity(SIDE * SIDE);
        for z in 0..SIDE {
            for x in 0..SIDE {
                let x = i32::try_from(x * 4).expect("measurement coordinate fits");
                let z = i32::try_from(z * 4).expect("measurement coordinate fits");
                lattice.insert_pair(
                    x,
                    z,
                    factor.compute(crate::density::Context::new(x, 0, z)),
                    offset.compute(crate::density::Context::new(x, 0, z)),
                );
                coords.push((x, z));
            }
        }
        let products = std::sync::Arc::new(lattice);
        let bounds = Bounds {
            x: (0, i32::try_from(SIDE * 4 - 1).expect("bound fits")),
            y: (0, 7),
            z: (0, i32::try_from(SIDE * 4 - 1).expect("bound fits")),
        };

        let make_baseline = || {
            NoiseChunkSampler::from_program(
                Program::compile(&final_density),
                4,
                4,
                8,
                Some(bounds),
            )
        };
        let make_optimized = || {
            NoiseChunkSampler::from_program_with_xz_products(
                Program::compile_with_xz_products(
                    &final_density,
                    XzProductManifest::from_routes(
                        42,
                        &preliminary,
                        &final_density,
                        &factor,
                        &offset,
                    )
                    .expect("test routes must admit the pure product pair"),
                ),
                4,
                4,
                8,
                Some(bounds),
                Some(std::sync::Arc::clone(&products)),
            )
        };
        let baseline_check = make_baseline();
        let optimized_check = make_optimized();
        let mut baseline_digest = 0_u64;
        let mut optimized_digest = 0_u64;
        for &(x, z) in &coords {
            baseline_digest ^= baseline_check.final_density(x, 0, z).to_bits();
            optimized_digest ^= optimized_check.final_density(x, 0, z).to_bits();
        }
        assert_eq!(baseline_digest, optimized_digest, "measured shape changed output bits");
        drop((baseline_check, optimized_check));

        let baseline = make_baseline();
        let before_baseline = usage();
        let mut baseline_digest = 0_u64;
        for &(x, z) in &coords {
            baseline_digest ^= black_box(baseline.final_density(x, 0, z).to_bits());
        }
        let after_baseline = usage();

        let optimized = make_optimized();
        let before_optimized = usage();
        let mut optimized_digest = 0_u64;
        for &(x, z) in &coords {
            optimized_digest ^= black_box(optimized.final_density(x, 0, z).to_bits());
        }
        let after_optimized = usage();
        assert_eq!(baseline_digest, optimized_digest);
        println!(
            "XZ_PRODUCT_CONTROL cells={} baseline_instructions={} optimized_instructions={} \
             baseline_cycles={} optimized_cycles={} baseline_digest={baseline_digest:016x}",
            coords.len(),
            after_baseline.0.saturating_sub(before_baseline.0),
            after_optimized.0.saturating_sub(before_optimized.0),
            after_baseline.1.saturating_sub(before_baseline.1),
            after_optimized.1.saturating_sub(before_optimized.1),
        );
    }
}
