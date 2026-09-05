//! End-to-end gate for the reported water bug: **a pond whose banks are real
//! `grass_block` states must not draw the flowing-water sprite at its edges.**
//!
//! The user's report, on a live 26.2 server: water "shows the 'flowing down'
//! effect on the edges that touch non-water blocks which is weird and shouldnt
//! happen". It was a **culling** bug, not a texture-choice bug — vanilla's
//! fluid mesher does use the `*_flow` sprite on every fluid side
//! face, but it culls that face when the neighbour occludes it, and the
//! neighbour on a shoreline is `grass_block`.
//!
//! # Why this gate and not just the unit test
//!
//! `models.rs` already unit-tests `mesh_fluids` against a synthetic view whose
//! banks are told to occlude. That is a closed loop: it proves the mesher honours
//! the flag, and would stay green while the flag itself was wrong — which is
//! exactly what happened. The load-bearing fact lives one layer down, in what
//! `BlockModels` says about the real `grass_block` model baked from the real jar.
//! So this gate builds `BlockModels` from `client.jar`, populates a
//! [`FluidSectionView`] with genuine vanilla state ids, and runs the same
//! `mesh_fluids` the live terrain path runs.
//!
//! # Negative control (executed here)
//!
//! The identical scene is meshed a second time through a view that answers
//! occlusion with the **pre-fix** expression (`is_full_cube(quads) && layer ==
//! Solid`) over those same real state ids. It must fail the same assertion — and
//! it does, loudly, with hundreds of side faces. Without that half, a gate that
//! only ever saw the fixed path could not tell a real fix from a scene that
//! happens to contain no shoreline (the "world" species of vacuous test).
//!
//! `#[ignore]`d and fail-closed: a missing jar is an environment failure, never a
//! silent skip. Run with:
//! `cargo test -p lodestone-render --test fluid_shoreline_gate -- --ignored --nocapture`

use std::collections::BTreeMap;

use lodestone_assets::{ResourceManager, ZipSource};
use lodestone_data::block_states::StateId;
use lodestone_model::{BlockStateRegistry, Identifier};
use lodestone_render::block_models::{FluidCell, FluidKind, FluidSprites};
use lodestone_render::models::{FluidSectionView, mesh_fluids};
use lodestone_render::{BlockModels, RenderLayer, blocks_json_registry, is_full_cube};

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

/// Which occlusion rule the view answers with.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Rule {
    /// What `BlockModels` reports today: per-face opaque coverage.
    PerFace,
    /// The pre-fix whole-block rule, reproduced here as the negative control.
    PreFixWholeBlock,
}

fn state_id(raw: u32) -> StateId {
    StateId::new(raw).expect("state id from the canonical blocks report")
}

/// A 16³ pond of vanilla `water` walled and floored with vanilla `grass_block`,
/// answering neighbourhood queries out of a real [`BlockModels`].
struct Pond<'a> {
    models: &'a BlockModels,
    water: u32,
    bank: u32,
    rule: Rule,
}

impl Pond<'_> {
    /// Water fills `x, z ∈ 4..12`, `y ∈ 0..8`; `grass_block` fills the rest below
    /// `y = 8` and the floor at `y = -1`; air above. Every water cell on the rim
    /// therefore has a bank as its horizontal neighbour — the geometry the bug
    /// lives at, which a lone water block cannot exercise.
    fn state_at(&self, x: i32, y: i32, z: i32) -> u32 {
        if y >= 8 {
            return 0; // air (state 0 in vanilla's registry)
        }
        if y < 0 {
            return self.bank;
        }
        if (4..12).contains(&x) && (4..12).contains(&z) {
            self.water
        } else {
            self.bank
        }
    }
}

impl FluidSectionView for Pond<'_> {
    fn fluid_at(&self, x: i32, y: i32, z: i32) -> Option<FluidCell> {
        self.models.fluid(state_id(self.state_at(x, y, z)))
    }

    fn occludes_at(&self, x: i32, y: i32, z: i32) -> bool {
        let id = self.state_at(x, y, z);
        match self.rule {
            Rule::PerFace => self.models.occludes(state_id(id)),
            Rule::PreFixWholeBlock => {
                let sm = self.models.state(state_id(id));
                is_full_cube(&sm.quads) && sm.layer == RenderLayer::Solid
            }
        }
    }

    fn fluid_sprites(&self, kind: FluidKind) -> FluidSprites {
        self.models.fluid_sprites(kind)
    }
}

/// Side faces (vertical quads) and level top surfaces in a fluid mesh.
fn count_faces(mesh: &lodestone_render::ModelMesh) -> (usize, usize) {
    let mut vertical = 0;
    let mut level = 0;
    for q in mesh.vertices.chunks(4) {
        let (lo, hi) = q
            .iter()
            .map(|v| v.position[1])
            .fold((f32::MAX, f32::MIN), |(l, h), y| (l.min(y), h.max(y)));
        if hi - lo > 0.5 {
            vertical += 1;
        } else if (hi - lo).abs() < 1e-6 {
            level += 1;
        }
    }
    (vertical, level)
}

#[test]
#[ignore = "requires a fetched vanilla client.jar and generated/reports/blocks.json"]
fn a_grass_banked_pond_draws_no_flowing_side_faces() {
    let jar = require_client_jar();
    let report = require_blocks_report(&jar);
    let source = ZipSource::open(&jar).expect("open client.jar");
    let manager = ResourceManager::new(vec![Box::new(source)]);
    let registry = blocks_json_registry(&report).expect("parse blocks.json into a registry");
    let models = BlockModels::build(&manager, &registry).expect("bake block models");

    let water = find_state(&registry, "minecraft:water", &[("level", "0")])
        .expect("water[level=0] in registry");
    let bank = find_state(&registry, "minecraft:grass_block", &[("snowy", "false")])
        .expect("grass_block[snowy=false] in registry");

    let fixed = mesh_fluids(&Pond {
        models: &models,
        water,
        bank,
        rule: Rule::PerFace,
    });
    let (vertical, level) = count_faces(&fixed.water);
    println!("per-face occlusion:  side faces = {vertical}, level top faces = {level}");

    // Negative control, executed and printed so its failure is visible in the log.
    let control = mesh_fluids(&Pond {
        models: &models,
        water,
        bank,
        rule: Rule::PreFixWholeBlock,
    });
    let (bad_vertical, bad_level) = count_faces(&control.water);
    println!(
        "pre-fix whole-block: side faces = {bad_vertical}, level top faces = {bad_level}  \
         <-- every side face draws the animated water_flow sprite over the bank"
    );

    assert!(
        bad_vertical > 0,
        "the negative control must reproduce the bug (it produced {bad_vertical} side \
         faces); if it does not, this gate cannot see the defect it exists to catch"
    );
    assert_eq!(
        vertical, 0,
        "a pond walled in real grass_block must emit no fluid side faces — each one \
         would draw the flowing-water sprite at the shoreline (the reported bug). The \
         pre-fix control on this same scene emits {bad_vertical}"
    );
    // The whole sky above the pond is open air, so every top-surface cell's
    // `should_render_backward_up_face_in` ring is all-air:
    // each of the 64 level quads gets a back copy, matching vanilla's own
    // open-water behaviour rather than a single-sided sheet.
    assert_eq!(
        level, 128,
        "the pond must still render its 8x8 top surface, flat, front+back: {level} level quads"
    );
}
