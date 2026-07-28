//! Acceptance gate for the **crack-overlay atlas producer** (step 1 of the
//! progressive block-breaking overlay).
//!
//! The mining state machine already computes a destroy stage `0..=9`
//! (`Mining::destroy_stage`, `BlockDestructionOverlays::stage_at`), but nothing
//! draws it — the seventh "island". The crack pass will re-texture a block's
//! *model geometry* with the `destroy_stage_N` sprite, so those ten sprites must
//! be resident in the **same** complete atlas the model pipeline binds. They are
//! referenced by no block model, so [`BlockModels::build`] must stitch them
//! explicitly (like fluids).
//!
//! This gate proves the producer half: all ten stages resolve to distinct,
//! non-empty atlas rectangles. The **negative control** — observed by
//! temporarily removing the explicit load in `build_complete_atlas` — is that
//! every stage then collapses to the empty `[0,0,0,0]` rect (the sprite is
//! absent), failing both the non-empty and the distinctness assertions below.

use lodestone_assets::{ResourceManager, ZipSource};
use lodestone_render::{BlockModels, CRACK_STAGE_COUNT, blocks_json_registry};

mod gate_harness;
use gate_harness::{require_blocks_report, require_client_jar};

fn build_models() -> BlockModels {
    let jar = require_client_jar();
    let report = require_blocks_report(&jar);
    let source = ZipSource::open(&jar).expect("open client.jar");
    let manager = ResourceManager::new(vec![Box::new(source)]);
    let registry = blocks_json_registry(&report).expect("parse blocks.json into a registry");
    BlockModels::build(&manager, &registry).expect("bake block models")
}

fn is_empty_rect(uv: [f32; 4]) -> bool {
    // A missing sprite resolves to a zero-area rect (min == max).
    (uv[2] - uv[0]).abs() < f32::EPSILON || (uv[3] - uv[1]).abs() < f32::EPSILON
}

#[test]
#[ignore = "requires a fetched vanilla client.jar and generated/reports/blocks.json"]
fn all_ten_crack_stages_are_distinct_resident_sprites() {
    let models = build_models();

    let rects: Vec<[f32; 4]> = (0..CRACK_STAGE_COUNT)
        .map(|s| {
            let uv = models
                .crack_stage_uv(s as u8)
                .unwrap_or_else(|| panic!("stage {s} must be in range"));
            assert!(
                !is_empty_rect(uv),
                "destroy_stage_{s} must be a real (non-empty) atlas sprite, got {uv:?} — the \
                 producer failed to stitch it (this is the observed negative-control failure)"
            );
            uv
        })
        .collect();

    // Ten *distinct* sprites: if the explicit load were missing they would all
    // collapse to the same empty rect, so distinctness is what proves each of
    // the ten physical crack frames is independently resident.
    for i in 0..rects.len() {
        for j in (i + 1)..rects.len() {
            assert_ne!(
                rects[i], rects[j],
                "destroy_stage_{i} and destroy_stage_{j} must be distinct atlas sprites"
            );
        }
    }
}

#[test]
#[ignore = "requires a fetched vanilla client.jar and generated/reports/blocks.json"]
fn out_of_range_stage_has_no_sprite() {
    let models = build_models();
    // Vanilla clears the overlay for any stage outside 0..=9; the accessor must
    // guard the range rather than index out of bounds.
    assert_eq!(models.crack_stage_uv(CRACK_STAGE_COUNT as u8), None);
    assert_eq!(models.crack_stage_uv(255), None);
}
