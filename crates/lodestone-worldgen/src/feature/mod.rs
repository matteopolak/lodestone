//! The feature placers a jigsaw feature-pool element runs.
//!
//! A pool element names a placed feature; its document (placement modifiers
//! and a configured feature) is parsed into [`vegetation::PlacedRef`] and run
//! against a [`vegetation::VegGrid`] over the structure's own world. Terrain
//! decoration itself is not done here: the 26.3 generator decorates with
//! `lodestone-worldgen-feature-26-3`.
//!
//! Placement modifiers compose as a depth-first flat-map: modifier 0 emits its
//! positions (drawing as it goes), then for *each* of those, modifier 1 runs
//! fully (including the eventual place call), and so on. The draw order of a
//! placed feature depends on that nesting, so [`vegetation`] reproduces it
//! with a recursion rather than collecting each modifier's positions first.

use serde_json::Value;

use crate::math;
use crate::rng::RandomSource;

pub(crate) mod vegetation;
pub(crate) mod overlay;
pub(crate) mod state_predicate;

/// A block position with `i32` components (vanilla's own block-position record).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BlockPos {
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) z: i32,
}

/// An integer provider: the subset feature and placement documents use.
#[derive(Clone, Debug)]
pub(crate) enum IntProvider {
    Constant(i32),
    Uniform { min: i32, max: i32 },
    /// An inclusive clamp around another integer provider. The nested
    /// provider is sampled first, then restricted to `[min, max]`.
    Clamped {
        source: Box<IntProvider>,
        min: i32,
        max: i32,
    },
    /// A weighted choice among nested integer providers, `(provider, weight)`
    /// in declaration order: the draw walks the list in that order.
    WeightedProviders(Vec<(Box<IntProvider>, i32)>),
    /// Vanilla's own biased-to-bottom int provider — used by
    /// `cactus`/`sugar_cane`'s block-column-feature layer heights (the
    /// cacti/sugar-cane increment). Additive: nothing before that
    /// increment constructs this variant.
    BiasedToBottom { min: i32, max: i32 },
    /// Vanilla's own trapezoid int provider, the REAL
    /// (two-draw, triangular) sample — not the `Uniform` approximation
    /// `crate::feature::vegetation::try_parse_int_provider` used to fold
    /// this into. That approximation preserved mean and support but not
    /// **draw count**: vanilla's symmetric case
    /// (`min == -max, plateau == 0`, which is every `random_offset` this
    /// crate's vegetation engine actually uses) draws `nextInt` TWICE and
    /// subtracts, while `Uniform` draws once — every RNG call after the
    /// first desyncs completely from vanilla's own stream, because
    /// `random_offset`'s `xz_spread`/`y_spread` are exactly this symmetric
    /// trapezoid shape. See
    /// [`IntProvider::sample`] for the exact vanilla formula, ported.
    Trapezoid { min: i32, max: i32, plateau: i32 },
    /// A Gaussian sample converted to a float, clamped to the inclusive bounds,
    /// then truncated toward zero. Used by cave speleothem random offsets.
    ClampedNormal {
        mean: f32,
        deviation: f32,
        min: i32,
        max: i32,
    },
}

impl IntProvider {
    pub(crate) fn sample<R: RandomSource>(&self, random: &mut R) -> i32 {
        match self {
            IntProvider::Constant(v) => *v,
            IntProvider::Uniform { min, max } => {
                math::random_between_inclusive(random, *min, *max)
            }
            IntProvider::Clamped { source, min, max } => source.sample(random).clamp(*min, *max),
            // Walk in declared order, subtracting one `nextInt(totalWeight)`
            // draw until it goes negative: the entry it goes negative on is
            // the pick, and only that entry samples.
            IntProvider::WeightedProviders(entries) => {
                let total: i32 = entries.iter().map(|(_, weight)| *weight).sum();
                let mut roll = random.next_int_bounded(total.max(1));
                for (provider, weight) in entries {
                    roll -= *weight;
                    if roll < 0 {
                        return provider.sample(random);
                    }
                }
                entries.last().map_or(0, |(provider, _)| provider.sample(random))
            }
            // Vanilla's own biased-to-bottom int provider's sample: `min + next_int(next_int(max
            // - min + 1) + 1)` — two nested, dependent draws, not one.
            IntProvider::BiasedToBottom { min, max } => {
                let n = *max - *min + 1;
                let inner = random.next_int_bounded(n);
                min + random.next_int_bounded(inner + 1)
            }
            // Vanilla's own trapezoid int provider's sample, ported exactly (see this variant's own
            // doc comment for why the draw COUNT matters, not just the
            // resulting distribution's shape).
            IntProvider::Trapezoid { min, max, plateau } => {
                if *plateau == 0 && *max == -*min {
                    random.next_int_bounded(max + 1) - random.next_int_bounded(max + 1)
                } else {
                    let range = max - min;
                    if *plateau == range {
                        math::random_between_inclusive(random, *min, *max)
                    } else {
                        let plateau_start = (range - plateau) / 2;
                        let plateau_end = range - plateau_start;
                        min + math::random_between_inclusive(random, 0, plateau_end)
                            + math::random_between_inclusive(random, 0, plateau_start)
                    }
                }
            }
            IntProvider::ClampedNormal {
                mean,
                deviation,
                min,
                max,
            } => (*mean + random.next_gaussian() as f32 * *deviation)
                .clamp(*min as f32, *max as f32) as i32,
        }
    }
}

/// Canonicalise an ore JSON `state` object (`{Name, Properties?}`) the same way
/// the oracle canonicalises a `BlockState`: name plus alphabetically-sorted
/// `key=value` properties.
#[must_use]
pub(crate) fn canonical_text(state: &Value) -> String {
    let name = state["Name"].as_str().expect("state Name");
    let mut out = name.to_string();
    if let Some(props) = state.get("Properties").and_then(Value::as_object) {
        let mut kv: Vec<(&String, String)> = props
            .iter()
            .map(|(k, v)| (k, v.as_str().unwrap_or_default().to_string()))
            .collect();
        kv.sort_by(|a, b| a.0.cmp(b.0));
        out.push('[');
        for (i, (k, v)) in kv.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(k);
            out.push('=');
            out.push_str(v);
        }
        out.push(']');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::rng::XoroshiroRandomSource;

    #[test]
    fn clamped_int_provider_clamps_nested_values_without_sampling_when_constant() {
        let mut random = XoroshiroRandomSource::new(0);
        let lower = IntProvider::Clamped {
            source: Box::new(IntProvider::Constant(-9)),
            min: 0,
            max: 4,
        };
        let middle = IntProvider::Clamped {
            source: Box::new(IntProvider::Constant(2)),
            min: 0,
            max: 4,
        };
        let upper = IntProvider::Clamped {
            source: Box::new(IntProvider::Constant(9)),
            min: 0,
            max: 4,
        };

        assert_eq!(lower.sample(&mut random), 0);
        assert_eq!(middle.sample(&mut random), 2);
        assert_eq!(upper.sample(&mut random), 4);
    }
}
