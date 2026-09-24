//! libFuzzer target: `lodestone_worldgen_core::density::Builder::build` must
//! never panic on arbitrary (but JSON-shaped) bytes.
//!
//! Unlike this fuzz crate's other targets, `Builder::build` is **not**
//! written to return `Result` on a structurally-malformed density-function
//! document — it `panic!`s on an unrecognised `type`, and `.unwrap()`/
//! `.expect()` on a handful of required fields (see
//! `lodestone_worldgen_core::density::mod`'s `build`/`build_object`). That is
//! a deliberate "this is trusted embedded data" assumption *today*, but
//! density-function documents are exactly the shape of thing a `dimension_type`
//! / custom noise-settings registry entry carries, and those are sent to a
//! joining **client** by whichever server it connects to
//! (`registry_data` in the Configuration phase) — so this is worth fuzzing
//! now to establish whether the panic surface is reachable from that path
//! before it is ever wired to it, not because it is reachable today.
//!
//! A panic here is therefore not necessarily a bug to fix on sight — it may
//! simply confirm the "trusted data" assumption is still accurate — but it
//! **is** a finding worth recording the moment this builder gains a
//! network-reachable caller, and coverage-guided fuzzing is the cheapest way
//! to keep that inventory current as the code evolves.
//!
//! The stub [`Resolver`] below returns inert defaults for every reference
//! query (`density_function`/`noise`) — those affect *tree shape* on a
//! resolved reference, not this target's panic property, since a self-string
//! reference just recurses into another `build` call over the stub's fixed
//! answer.

#![no_main]

use libfuzzer_sys::fuzz_target;
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

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let Ok(node) = serde_json::from_str::<Value>(text) else {
        return;
    };
    let resolver = StubResolver;
    let builder = Builder::new(0, &resolver);
    let _ = builder.build(&node);
});
