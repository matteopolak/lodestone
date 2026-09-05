//! Discriminating gate for `is_packed_cube`'s [`RenderLayer`] requirement.
//!
//! # Background: the claim this gate was written to check, and what verifying
//! it against the tree actually found
//!
//! The **model** path (`mesh_models_layers`) draws
//! translucent blocks through their own blended pass. A second, narrower gap
//! sits next to it: `is_packed_cube` (`crates/lodestone-render/src/
//! models.rs`) excluded only *tinted* full cubes, so — read naively — an
//! untinted translucent full cube (stained glass, ice, tinted glass, slime,
//! honey) looked like it would still take a fast "opaque" path and render
//! solid.
//!
//! Tracing the actual call graph before building on that claim
//! (`crates/lodestone-shell/src/mesher.rs`'s `mesh_one`,
//! `crates/lodestone-shell/src/blocks.rs`'s `ShellClassifier::models`,
//! `crates/lodestone-shell/src/resources.rs`'s `BlockResources::try_vanilla`)
//! found `is_packed_cube` has **zero production callers** anywhere in the
//! workspace outside tests. Every live (`ShellClassifier::Vanilla`) block —
//! full cube or not, tinted or not — is meshed through the wide per-quad model
//! path (`mesh_models_layers`), which already reads the real
//! [`lodestone_render::BlockModels::layer`] and puts anything `Translucent` on
//! its own pass. `SectionGeometry::Packed` (the thing `is_packed_cube` would
//! route into) is reachable only through `ShellClassifier::Demo`, an offline
//! 10-block sandbox palette with no translucent block in it at all (see
//! `crates/lodestone-shell/src/blocks.rs`'s own doc: water there "renders
//! opaque in this demo"). So **the bug as originally stated does not
//! reproduce in the live game today** — see `crates/lodestone-render/src/
//! models.rs`'s module doc for the fuller trace.
//!
//! What *is* real: `is_packed_cube` is public API, exercised by
//! `model_census.rs` and `live_gate.rs`, and its own module doc frames it as
//! "our policy" for routing full cubes — exactly the function a future patch
//! restoring the packed path for live full cubes (a real, measured VRAM win:
//! 72 vs 152 bytes/quad, see `models.rs`'s D1 section) would reach for. Before
//! this gate, that function would have silently reproduced the exact bug
//! described above, the moment someone wired it up: it took no layer at all, so an
//! untinted translucent full cube passed it unconditionally. `is_packed_cube`
//! now requires the caller to supply the block's real `RenderLayer` and only
//! returns `true` for `RenderLayer::Solid` — this gate proves that against
//! real vanilla data, not synthetic fixtures.
//!
//! # Three classes, three arms, one collection
//!
//! Per `CLAUDE.md`'s "collect mismatches, don't assert! in a loop" rule, every
//! candidate is checked and any failure is pushed to a `Vec`; the test panics
//! once at the end with everything that went wrong, so a single run can never
//! hide a second failure behind the first `assert!`.
//!
//! Expected values are **not** hardcoded from memory or copied off
//! `RenderLayer::from_sprite_alpha`'s own logic — they are derived here from
//! the real PNG bytes in the fetched `client.jar`
//! ([`lodestone_assets::Image::decode_png`]), independently reproducing the
//! alpha classification rule (`Solid` = all texels 255, `Cutout` = only 0/255,
//! `Translucent` = any texel strictly between). If `BlockModels::layer` or
//! `is_packed_cube` ever drifted from that rule, this gate — not just the
//! predicate's own unit tests — would catch it.
//!
//! `#[ignore]`d and fail-closed like its sibling `block_models_gate.rs`. Run
//! with:
//! `cargo test -p lodestone-render --test packed_cube_layer_gate -- --ignored --nocapture`

use std::collections::BTreeMap;

use lodestone_assets::{Image, ResourceManager, ZipSource};
use lodestone_data::block_states::StateId;
use lodestone_model::{BlockStateRegistry, Identifier};
use lodestone_render::{BlockModels, RenderLayer, blocks_json_registry, is_full_cube,
    is_packed_cube};

#[path = "../gate_harness/mod.rs"]
mod gate_harness;
use gate_harness::{require_blocks_report, require_client_jar};

/// The first state id whose block matches `block` and whose properties are a
/// superset of `want`.
fn find_state(reg: &dyn BlockStateRegistry, block: &str, want: &[(&str, &str)]) -> Option<u32> {
    let ident: Identifier = block.parse().ok()?;
    let wanted: BTreeMap<&str, &str> = want.iter().copied().collect();
    (0..reg.state_count()).find(|&id| {
        let Some(state) = reg.resolve(id) else {
            return false;
        };
        if *state.block != ident {
            return false;
        }
        wanted
            .iter()
            .all(|(k, v)| state.properties.get(*k).map(String::as_str) == Some(*v))
    })
}

fn state_id(raw: u32) -> StateId {
    StateId::new(raw).expect("state id from the canonical blocks report")
}

/// The real per-texel-alpha `RenderLayer` of one or more raw PNGs
/// (`block_layer`'s "most transparent sprite wins" rule, reproduced here from
/// the actual file bytes rather than trusting the crate's own atlas
/// machinery), so this gate's expected values originate outside the code under
/// test.
fn measured_layer_of_textures(manager: &ResourceManager, paths: &[&str]) -> RenderLayer {
    let mut layer = RenderLayer::Solid;
    for path in paths {
        let bytes = manager
            .read(path)
            .unwrap_or_else(|| panic!("texture missing from jar: {path}"));
        let img = Image::decode_png(&bytes).unwrap_or_else(|e| panic!("{path}: {e}"));
        let alpha: Vec<u8> = img.rgba.chunks_exact(4).map(|p| p[3]).collect();
        layer = layer.max(RenderLayer::from_sprite_alpha(&alpha));
    }
    layer
}

/// One candidate block: its state, and the raw texture(s) whose alpha decides
/// its expected layer independently of `BlockModels`.
struct Candidate {
    block: &'static str,
    props: &'static [(&'static str, &'static str)],
    /// Whether we expect `is_full_cube` to hold. Measured, not assumed: baking
    /// the real jar found `slime_block`/`honey_block` are **not** six-quad
    /// cubes (12 quads each — vanilla's models nest a second, inset element for
    /// the "sticky edge" look), so they are already excluded by
    /// [`is_full_cube`] before the layer check this gate targets ever runs.
    /// They stay in the corpus (both are still real untinted `Translucent`
    /// full-*block-space* geometry) but do not, on their own,
    /// discriminate the layer fix — `white_stained_glass`/`ice`/`tinted_glass`
    /// do that, and `glass` (plain) is the `Cutout` control.
    expect_full_cube: bool,
}

const T: &str = "assets/minecraft/textures/block";

fn candidates() -> Vec<Candidate> {
    vec![
        // Control: fully opaque full cube. Must stay packed-eligible.
        Candidate {
            block: "minecraft:stone",
            props: &[],
            expect_full_cube: true,
        },
        // Control: untinted, binary-alpha (Cutout) full cube. Must NOT be
        // packed-eligible — the plausible wrong hypothesis this gate exists to
        // catch (collapsing Cutout into "not Translucent, so fine"). Plain
        // glass, not a stained variant: stained glass panes/blocks are the
        // Translucent case below, and leaves are tinted (which would already
        // exclude them via the tint check, not the layer check this gate
        // targets).
        Candidate {
            block: "minecraft:glass",
            props: &[],
            expect_full_cube: true,
        },
        // The five untinted translucent full cubes.
        Candidate {
            block: "minecraft:white_stained_glass",
            props: &[],
            expect_full_cube: true,
        },
        Candidate {
            block: "minecraft:ice",
            props: &[],
            expect_full_cube: true,
        },
        Candidate {
            block: "minecraft:tinted_glass",
            props: &[],
            expect_full_cube: true,
        },
        Candidate {
            block: "minecraft:slime_block",
            props: &[],
            expect_full_cube: false,
        },
        Candidate {
            block: "minecraft:honey_block",
            props: &[],
            expect_full_cube: false,
        },
    ]
}

/// Texture paths per block, keyed separately from `candidates()` so the
/// `const` list above stays declarative; multi-texture blocks (honey_block)
/// need every face's sprite measured, or a uniformly-Translucent side face
/// could hide behind a not-yet-measured top.
fn textures_for(block: &str) -> Vec<String> {
    let name = block.strip_prefix("minecraft:").unwrap_or(block);
    match name {
        "honey_block" => vec![
            format!("{T}/honey_block_top.png"),
            format!("{T}/honey_block_side.png"),
            format!("{T}/honey_block_bottom.png"),
        ],
        other => vec![format!("{T}/{other}.png")],
    }
}

#[test]
#[ignore = "requires a fetched vanilla client.jar and generated/reports/blocks.json"]
fn packed_cube_requires_solid_layer_on_real_vanilla_data() {
    let jar = require_client_jar();
    let report = require_blocks_report(&jar);
    let source = ZipSource::open(&jar).expect("open client.jar");
    let manager = ResourceManager::new(vec![Box::new(source)]);
    let registry = blocks_json_registry(&report).expect("parse blocks.json into a registry");
    let models = BlockModels::build(&manager, &registry).expect("bake block models");

    let mut failures: Vec<String> = Vec::new();
    let mut measured: Vec<(&'static str, RenderLayer, RenderLayer, bool)> = Vec::new();

    for c in candidates() {
        let Some(id) = find_state(&registry, c.block, c.props) else {
            failures.push(format!("{}: state not found in registry", c.block));
            continue;
        };
        let sm = models.state(state_id(id));

        if c.expect_full_cube && !is_full_cube(&sm.quads) {
            failures.push(format!(
                "{}: expected is_full_cube (geometry can't tell these classes apart, which \
                 is the point), got {} quads",
                c.block,
                sm.quads.len()
            ));
        }
        if sm.quads.iter().any(|q| q.tint_index.is_some()) {
            failures.push(format!(
                "{}: has a tinted quad — the tint exclusion alone would already route it off \
                 the packed path, so this candidate can't discriminate the layer check",
                c.block
            ));
        }

        let tex_paths = textures_for(c.block);
        let tex_refs: Vec<&str> = tex_paths.iter().map(String::as_str).collect();
        let expected_layer = measured_layer_of_textures(&manager, &tex_refs);

        if sm.layer != expected_layer {
            failures.push(format!(
                "{}: BlockModels::layer = {:?}, but the real PNG alpha (measured \
                 independently from {:?}) says {:?}",
                c.block, sm.layer, tex_paths, expected_layer
            ));
        }

        let packed = is_packed_cube(&sm.quads, sm.layer);
        let expect_packed = expected_layer == RenderLayer::Solid;
        if packed != expect_packed {
            failures.push(format!(
                "{}: is_packed_cube(quads, {:?}) = {packed}, expected {expect_packed} \
                 (packed-eligible iff the real texture alpha says Solid)",
                c.block, sm.layer
            ));
        }
        measured.push((c.block, expected_layer, sm.layer, packed));
    }

    eprintln!("=== packed-cube / layer gate (real vanilla data) ===");
    for (block, expected, got, packed) in &measured {
        eprintln!("  {block:32} texture-measured={expected:?}  BlockModels::layer={got:?}  is_packed_cube={packed}");
    }

    assert!(
        failures.is_empty(),
        "packed-cube layer gate found {} mismatch(es):\n{}",
        failures.len(),
        failures.join("\n")
    );

    // Discriminating check, stated explicitly rather than left implicit in the
    // loop above: stone (Solid) must be the only candidate that is
    // packed-eligible. A gate that let every full cube through, or excluded
    // every full cube, would still pass the per-candidate loop above if its
    // predicate were a constant — this is the "must disagree in both
    // directions" control.
    let packed_blocks: Vec<&str> = measured
        .iter()
        .filter(|(_, _, _, packed)| *packed)
        .map(|(b, _, _, _)| *b)
        .collect();
    assert_eq!(
        packed_blocks,
        vec!["minecraft:stone"],
        "exactly one candidate (stone) should be packed-eligible; got {packed_blocks:?}"
    );
}
