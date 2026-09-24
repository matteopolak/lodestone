//! Bug found by `fuzz/fuzz_targets/density_function_json.rs` (widened Track
//! A scope, from the "obvious first targets" list naming "the density
//! compiler"): `lodestone_worldgen_core::density::Builder::build` used to
//! abort the process on the very first fuzz iteration, on the two-byte input
//! `[]`.
//!
//! ## Where the bug lived
//!
//! `crates/lodestone-worldgen-core/src/density/mod.rs`'s `Builder::build`
//! matched on the node kind and hit a `panic!` arm for anything that was not
//! a number, string or object — an array, bool, or null. `build_object` had
//! the same shape one level down: an `.expect(...)` on a missing `type`
//! field, and `.unwrap()`s wherever a field was read.
//!
//! `build`/`build_object`/`build_spline` (and the small field-reading helpers
//! they use) now return `Result<_, DensityBuildError>` instead: every one of
//! those panicking spots is a typed error variant there now, so no JSON
//! shape can bring down the process. This test pins the fix by asserting the
//! specific error the two-byte input `[]` now produces, rather than a panic.
//!
//! ## Why this mattered even though every call site today is trusted
//!
//! Every call site feeds `Builder::build` our own bundled
//! `worldgen/density_function/*.json` documents or a checked-in oracle
//! fixture, never anything attacker-supplied — so this was not a live
//! remote-crash vulnerability. But a `dimension_type`/noise-settings registry
//! entry is exactly the shape of document this builder parses, and those are
//! sent to a **joining client** by whichever server it connects to
//! (`registry_data` in the Configuration phase). `Builder::build` is not
//! wired to that path today, but it is exactly the kind of panic surface that
//! becomes one silently the moment a future change starts handing this
//! builder anything that originated over the wire.
//!
//! ## Reproduction
//!
//! The fuzzer's actual crashing input was the raw bytes `[0x20, 0x20, 0x5b,
//! 0x5d]` (`"  []"`, i.e. a JSON array with whitespace) — this test uses the
//! minimal `"[]"` instead, which is the same failing shape without the
//! incidental leading whitespace libFuzzer's minimizer had not yet stripped.

use lodestone_worldgen_core::density::{Builder, DensityBuildError, NoiseParams, Resolver};
use serde_json::Value;

struct StubResolver;

impl Resolver for StubResolver {
    fn density_function(&self, _id: &str) -> Value {
        Value::Number(0.into())
    }

    fn noise(&self, _id: &str) -> NoiseParams {
        NoiseParams {
            first_octave: 0,
            amplitudes: vec![1.0],
        }
    }
}

/// **Used to panic** — now returns the specific decode error instead, which
/// is the signal the hardening described in this file's module doc has
/// landed rather than a spurious regression.
#[test]
fn build_returns_a_decode_error_on_a_json_array_instead_of_panicking() {
    let node: Value = serde_json::from_str("[]").expect("valid JSON");
    let resolver = StubResolver;
    let builder = Builder::new(0, &resolver);
    match builder.build(&node) {
        Err(DensityBuildError::NotNumberStringOrObject) => {}
        other => panic!(
            "a JSON array is not a number, string or object — `build` must report \
             `Err(DensityBuildError::NotNumberStringOrObject)` rather than panicking or \
             silently accepting it; got {other:?}"
        ),
    }
}
