//! `WorldgenRandom.Algorithm` — the runtime choice between the two families.
//!
//! Every dimension's `noise_settings` carries `legacy_random_source`, and
//! vanilla switches the **whole** noise stack on it
//! (`NoiseGeneratorSettings.getRandomSource()` →
//! `RandomState`'s `settings.getRandomSource().newInstance(seed).forkPositional()`).
//! The Overworld leaves the flag out (xoroshiro); `nether.json` and `end.json`
//! both set it true, so both dimensions seed *every* named noise, the surface
//! system's `vertical_gradient` factories and the aquifer/ore factories from the
//! `java.util.Random` LCG instead.
//!
//! ## Why an enum and not a trait object
//!
//! [`PositionalRandomFactory`] is not object-safe: it has an associated
//! `Source` type and returns `Self::Source` by value. Making
//! [`crate::density::Builder`] generic over the factory would have made the
//! parameter viral through `SurfaceSystem`, `AquiferSystem` and every stage
//! struct that stores one. So the polymorphism is a two-variant enum that is
//! itself a [`PositionalRandomFactory`], and — because both concrete factories
//! are `Copy` — it stays `Copy`, which is what lets the existing by-value fields
//! (`SurfaceSystem::master`, `Cond::VerticalGradient::factory`,
//! `AquiferSystem::positional`) keep their shape.
//!
//! ## Gotcha
//!
//! The two families' `from_seed` are **not** the same operation and must not be
//! unified: `LegacyPositionalRandomFactory.fromSeed(seed)` discards its own seed
//! and returns `new LegacyRandomSource(seed)`, while the xoroshiro one XORs the
//! seed into both halves of its 128-bit state. Each arm delegates to its own
//! concrete impl for exactly that reason.

use super::{
    LegacyPositionalFactory, LegacyRandomSource, PositionalRandomFactory, RandomSource,
    XoroshiroPositionalFactory, XoroshiroRandomSource,
};

/// `WorldgenRandom.Algorithm` — which family a dimension's noise stack uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Algorithm {
    /// `WorldgenRandom.Algorithm.LEGACY` — the `java.util.Random` LCG. Selected
    /// by `legacy_random_source: true` (the Nether and the End).
    Legacy,
    /// `WorldgenRandom.Algorithm.XOROSHIRO` — the 1.18+ default (the Overworld).
    Xoroshiro,
}

impl Algorithm {
    /// `NoiseGeneratorSettings.getRandomSource()` — reads the flag the way
    /// vanilla does, defaulting to xoroshiro when the key is absent.
    #[must_use]
    pub fn from_legacy_flag(legacy_random_source: bool) -> Self {
        if legacy_random_source {
            Self::Legacy
        } else {
            Self::Xoroshiro
        }
    }

    /// Reads `legacy_random_source` out of a `noise_settings` document.
    /// A missing key is xoroshiro, matching the codec's `false` default.
    #[must_use]
    pub fn from_settings(settings: &serde_json::Value) -> Self {
        Self::from_legacy_flag(
            settings["legacy_random_source"]
                .as_bool()
                .unwrap_or(false),
        )
    }

    /// Whether this is the legacy family — the question `RandomState`'s
    /// `useLegacyInit` asks when it decides how to seed `BlendedNoise`.
    #[must_use]
    pub fn is_legacy(self) -> bool {
        matches!(self, Self::Legacy)
    }

    /// `Algorithm.newInstance(seed)`.
    #[must_use]
    pub fn new_instance(self, seed: i64) -> AnyRandomSource {
        match self {
            Self::Legacy => AnyRandomSource::Legacy(LegacyRandomSource::new(seed)),
            Self::Xoroshiro => AnyRandomSource::Xoroshiro(XoroshiroRandomSource::new(seed)),
        }
    }

    /// `newInstance(seed).forkPositional()` — `RandomState.random`.
    #[must_use]
    pub fn root_positional(self, seed: i64) -> AnyPositionalFactory {
        self.new_instance(seed).fork_positional()
    }
}

/// Either family's generator, dispatched at runtime.
///
/// Every method delegates; nothing is reimplemented here, so the parity the two
/// concrete impls already have against `rng_java.txt` carries over unchanged.
#[derive(Debug, Clone)]
pub enum AnyRandomSource {
    /// The `java.util.Random` LCG.
    Legacy(LegacyRandomSource),
    /// xoroshiro128++.
    Xoroshiro(XoroshiroRandomSource),
}

/// Either family's positional factory. `Copy`, like both of its variants.
#[derive(Debug, Clone, Copy)]
pub enum AnyPositionalFactory {
    /// `LegacyPositionalRandomFactory`.
    Legacy(LegacyPositionalFactory),
    /// `XoroshiroPositionalRandomFactory`.
    Xoroshiro(XoroshiroPositionalFactory),
}

impl From<XoroshiroPositionalFactory> for AnyPositionalFactory {
    fn from(value: XoroshiroPositionalFactory) -> Self {
        Self::Xoroshiro(value)
    }
}

impl From<LegacyPositionalFactory> for AnyPositionalFactory {
    fn from(value: LegacyPositionalFactory) -> Self {
        Self::Legacy(value)
    }
}

/// Dispatches a `&mut self` method over both arms.
macro_rules! dispatch_mut {
    ($self:ident, $method:ident $(, $arg:ident)*) => {
        match $self {
            Self::Legacy(inner) => inner.$method($($arg),*),
            Self::Xoroshiro(inner) => inner.$method($($arg),*),
        }
    };
}

impl RandomSource for AnyRandomSource {
    type Positional = AnyPositionalFactory;

    fn fork_positional(&mut self) -> AnyPositionalFactory {
        match self {
            Self::Legacy(inner) => AnyPositionalFactory::Legacy(inner.fork_positional()),
            Self::Xoroshiro(inner) => AnyPositionalFactory::Xoroshiro(inner.fork_positional()),
        }
    }

    fn set_seed(&mut self, seed: i64) {
        dispatch_mut!(self, set_seed, seed)
    }

    fn next_bits(&mut self, bits: u32) -> i32 {
        dispatch_mut!(self, next_bits, bits)
    }

    fn next_int(&mut self) -> i32 {
        dispatch_mut!(self, next_int)
    }

    fn next_int_bounded(&mut self, bound: i32) -> i32 {
        dispatch_mut!(self, next_int_bounded, bound)
    }

    fn next_long(&mut self) -> i64 {
        dispatch_mut!(self, next_long)
    }

    fn next_bool(&mut self) -> bool {
        dispatch_mut!(self, next_bool)
    }

    fn next_float(&mut self) -> f32 {
        dispatch_mut!(self, next_float)
    }

    fn next_double(&mut self) -> f64 {
        dispatch_mut!(self, next_double)
    }

    fn next_gaussian(&mut self) -> f64 {
        dispatch_mut!(self, next_gaussian)
    }

    fn consume_count(&mut self, rounds: u32) {
        dispatch_mut!(self, consume_count, rounds)
    }
}

impl PositionalRandomFactory for AnyPositionalFactory {
    type Source = AnyRandomSource;

    fn at(&self, x: i32, y: i32, z: i32) -> AnyRandomSource {
        match self {
            Self::Legacy(inner) => AnyRandomSource::Legacy(inner.at(x, y, z)),
            Self::Xoroshiro(inner) => AnyRandomSource::Xoroshiro(inner.at(x, y, z)),
        }
    }

    fn from_hash_of(&self, name: &str) -> AnyRandomSource {
        match self {
            Self::Legacy(inner) => AnyRandomSource::Legacy(inner.from_hash_of(name)),
            Self::Xoroshiro(inner) => AnyRandomSource::Xoroshiro(inner.from_hash_of(name)),
        }
    }

    fn from_seed(&self, seed: i64) -> AnyRandomSource {
        match self {
            Self::Legacy(inner) => AnyRandomSource::Legacy(inner.from_seed(seed)),
            Self::Xoroshiro(inner) => AnyRandomSource::Xoroshiro(inner.from_seed(seed)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The dispatch must be transparent: the enum's draws are the concrete
    /// family's draws, in the same order. Checked against both arms, because a
    /// wrapper that silently routed everything to one family would still look
    /// plausible from either side alone.
    #[test]
    fn dispatch_is_transparent_for_both_families() {
        for seed in [0i64, 42, -8_823_894_646, 1_234_567_890_123] {
            let mut concrete = LegacyRandomSource::new(seed);
            let mut wrapped = Algorithm::Legacy.new_instance(seed);
            for i in 0..8 {
                assert_eq!(
                    concrete.next_long(),
                    wrapped.next_long(),
                    "legacy draw {i} at seed {seed}"
                );
            }

            let mut concrete = XoroshiroRandomSource::new(seed);
            let mut wrapped = Algorithm::Xoroshiro.new_instance(seed);
            for i in 0..8 {
                assert_eq!(
                    concrete.next_long(),
                    wrapped.next_long(),
                    "xoroshiro draw {i} at seed {seed}"
                );
            }
        }
    }

    /// The control for the test above, and the reason it is not vacuous: the two
    /// families must actually disagree on this seed, or a wrapper hardwired to
    /// one of them would pass. Observed, not described.
    #[test]
    fn the_two_families_are_separated_at_this_seed() {
        let mut legacy = Algorithm::Legacy.new_instance(42);
        let mut xoro = Algorithm::Xoroshiro.new_instance(42);
        assert_ne!(legacy.next_long(), xoro.next_long());
    }

    /// `from_seed` differs by family and unifying it would be a silent
    /// divergence: the legacy factory discards its own seed, the xoroshiro one
    /// mixes it in. So a legacy factory's `from_seed(s)` equals a bare
    /// `LegacyRandomSource::new(s)` regardless of the fork it came from, and the
    /// xoroshiro one does not.
    #[test]
    fn from_seed_keeps_each_family_s_own_meaning() {
        let legacy = Algorithm::Legacy.root_positional(42);
        let mut from_factory = legacy.from_seed(7);
        let mut bare = LegacyRandomSource::new(7);
        assert_eq!(from_factory.next_long(), bare.next_long());

        let xoro = Algorithm::Xoroshiro.root_positional(42);
        let mut from_factory = xoro.from_seed(7);
        let mut bare = XoroshiroRandomSource::new(7);
        assert_ne!(
            from_factory.next_long(),
            bare.next_long(),
            "xoroshiro's from_seed must mix the factory's own seed in"
        );
    }

    /// A missing `legacy_random_source` key is xoroshiro (the Overworld), and
    /// `true` is legacy (the Nether and the End). Read from the real bundled
    /// documents' own spelling rather than a hand-made literal.
    #[test]
    fn the_flag_selects_the_family() {
        let overworld: serde_json::Value = serde_json::json!({ "sea_level": 63 });
        let nether: serde_json::Value = serde_json::json!({ "legacy_random_source": true });
        assert_eq!(Algorithm::from_settings(&overworld), Algorithm::Xoroshiro);
        assert_eq!(Algorithm::from_settings(&nether), Algorithm::Legacy);
    }
}
