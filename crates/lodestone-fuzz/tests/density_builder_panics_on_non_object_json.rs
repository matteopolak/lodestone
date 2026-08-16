//! Bug found by `fuzz/fuzz_targets/density_function_json.rs` (widened Track
//! A scope, issue #549's "obvious first targets" list naming "the density
//! compiler"): `lodestone_worldgen_core::density::Builder::build` aborts the
//! process on the very first fuzz iteration, on the two-byte input `[]`.
//!
//! ## Where the bug lives
//!
//! `crates/lodestone-worldgen-core/src/density/mod.rs`'s `Builder::build`:
//!
//! ```text
//! pub fn build(&self, node: &Value) -> Density {
//!     match node {
//!         Value::Number(n) => Density::Const(n.as_f64().unwrap()),
//!         Value::String(id) => { ... }
//!         Value::Object(_) => self.build_object(node),
//!         other => panic!("unexpected density-function json: {other:?}"),
//!     }
//! }
//! ```
//!
//! Every JSON value that is not a number, string or object — an array, bool,
//! or null — hits the final `panic!` arm. `build_object` has the same shape
//! one level down: `.expect("density function object missing type")` on a
//! missing `type` field, and `.unwrap()` on `Value::Number::as_f64()`
//! wherever a field is read with this file's own `f()` helper. None of this
//! is a decode bug in the sense the packet/NBT fuzz targets check — it is a
//! deliberate "this is trusted embedded data" design, since every call site
//! today feeds `Builder::build` our own bundled `worldgen/density_function/
//! *.json` documents or the checked-in oracle fixtures, never anything
//! attacker-supplied.
//!
//! ## Why this is worth recording without fixing it
//!
//! A `dimension_type`/noise-settings registry entry — the exact shape of
//! document this builder parses — is sent to a **joining client** by
//! whichever server it connects to (`registry_data` in the Configuration
//! phase). `Builder::build` is not wired to that path today (this repo's own
//! worldgen only ever runs server-side, generating from our own bundled
//! data), so this is not a live remote-crash vulnerability right now. But it
//! is exactly the kind of panic surface that becomes one silently, the
//! moment a future change starts handing this builder anything that
//! originated over the wire — see `CLAUDE.md`'s island-detection rule about
//! producers and consumers being wired long after either side alone looks
//! finished. Hardening `build`/`build_object` into a `Result`-returning API
//! is a real (if sizeable — `Density` and every call site across
//! `lodestone-worldgen` would need to follow) piece of work, out of scope
//! for the session that widened this fuzz corpus; this test exists so the
//! panic is a tracked, named finding rather than something the next person
//! rediscovers from a cryptic in-fuzzer abort.
//!
//! ## Reproduction
//!
//! The fuzzer's actual crashing input was the raw bytes `[0x20, 0x20, 0x5b,
//! 0x5d]` (`"  []"`, i.e. a JSON array with whitespace) — this test uses the
//! minimal `"[]"` instead, which is the same failing shape without the
//! incidental leading whitespace libFuzzer's minimizer had not yet stripped.

use lodestone_worldgen_core::density::{Builder, NoiseParams, Resolver};
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

/// **Currently panics** — documents the live finding. If this test starts
/// failing because `build` no longer panics (it now returns a `Result`, or
/// treats a non-object/string/number node as some default `Density`),
/// replace `#[should_panic]` with an assertion on the `Result`/`Density`
/// this call now produces — that is the signal the hardening described in
/// this file's module doc has landed, not a spurious regression.
#[test]
#[should_panic(expected = "unexpected density-function json")]
fn build_panics_on_a_json_array_instead_of_returning_a_decode_error() {
    let node: Value = serde_json::from_str("[]").expect("valid JSON");
    let resolver = StubResolver;
    let builder = Builder::new(0, &resolver);
    let _ = builder.build(&node);
}
