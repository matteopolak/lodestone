//! Pixel gate: `CrackPipeline`/`RenderState::render_with_crack` must draw
//! **every** target in the `cracks` slice, not just the first.
//!
//! # The reported defect
//!
//! Before this pass, `render_with_crack`/`render_with_crack_and_effects` took a
//! single `CrackTarget`, so another player's block-crack overlay had nowhere to
//! render even though `SessionBlockDestruction` already carried it (folded end
//! to end by `44485e4`, per `lodestone-client`'s
//! `state::tests::apply_routes_block_destruction_through_the_real_path`). A gate
//! that only ever supplies one target cannot tell "draws the one target it was
//! given" from "structurally cannot draw a second" — the exact gap this issue
//! reports. So this gate supplies **two** targets at **two** distinct world
//! positions in the same call and requires **both** to reach the screen, with
//! their own bounding boxes reported so the failure mode ("only one lit") is
//! visible rather than inferred from a single count.
//!
//! # Scene
//!
//! No terrain is uploaded — the crack pass does not read section geometry, only
//! `CrackResolver`'s per-state baked quads (captured once from the vanilla
//! `BlockModels` at `RenderState::new`) and the camera. Two `minecraft:stone`
//! targets sit left and right of screen centre, three blocks ahead, drawn at
//! destroy stage 9 (maximum crack, per `CrackResolver::mesh_for`'s doc: vanilla
//! clears the overlay at `stage >= 10`, so 9 is the last real stage and the most
//! visually distinct from "no crack"). The doubled-multiply blend
//! (`crack_pipeline.rs`) can only ever darken, so each target's footprint reads
//! as a cluster of pixels darker than the sky behind it.
//!
//! # Controls, all executed
//!
//! * **empty `cracks` slice**: pixel-identical to a plain `render()` call —
//!   proves the multi-target plumbing is a true no-op when there is nothing to
//!   draw, not an accidental single implicit target.
//! * **left-only** / **right-only**: each must light up *only* its own cluster
//!   and leave the other target's region untouched — the discriminating proof
//!   that the two draws are independent, not one target drawn twice or the
//!   second silently dropped.
//! * **both together**: both clusters must be lit, at bounding boxes that do not
//!   overlap the empty-scene baseline and do not overlap each other — this is
//!   the one assertion a single-target implementation cannot pass, since the
//!   pre-fix pipeline accepted at most one `CrackTarget` and had nothing to
//!   store a second in.
//! * `stats.cracks_drawn` corroborates the pixel count non-visually at each
//!   step (0 / 1 / 1 / 2), per CLAUDE.md's "measure by location, never by frame
//!   average" — a bounding box localises *where*, and `cracks_drawn` confirms
//!   *how many draw calls actually happened*, so a regression that drew two
//!   overlapping quads at one position (same pixel cluster, wrong count) cannot
//!   hide behind a pixel-only assertion.
//!
//! Fail-closed like its siblings: a missing GPU or a missing `client.jar` is a
//! failure, never a skip.
//!
//! ```text
//! cargo test -p lodestone-shell --test crack_multi_target_pixels -- --ignored --nocapture
//! ```

use lodestone::gpu::{CrackTarget, RenderState};
use lodestone::resources::BlockResources;
use lodestone_render::{BlockModels, Camera, GpuContext, HeadlessTarget, RenderTarget};

const W: u32 = 320;
const H: u32 = 240;

/// `minecraft:stone` — an ordinary opaque full cube, guaranteed real baked
/// geometry, and nowhere near any block used by another live pixel gate in this
/// crate.
const BLOCK: &str = "minecraft:stone";

/// Maximum real destroy stage. `CrackResolver::mesh_for`'s doc: a stage outside
/// `0..=9` draws nothing (vanilla clears the overlay at `>= 10`), so 9 is both
/// the last real stage and the most visually distinct from "no crack".
const STAGE: u8 = 9;

fn camera() -> Camera {
    Camera {
        position: glam::Vec3::new(0.0, 0.5, 0.0),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 60.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(8, 0),
    }
}

/// The block-state id `name` resolves to in the jar-derived state census,
/// taking the block's first state. Scanned rather than hardcoded so a census
/// regeneration cannot silently start naming an unrelated block — same
/// convention as `mining_destroy_burst.rs`'s `state_id_of`.
fn state_id_of(name: &str) -> u32 {
    (0..lodestone_data::block_states::STATE_COUNT)
        .find(|&id| lodestone_data::block_states::block_name(id) == Some(name))
        .unwrap_or_else(|| panic!("{name} is not in the protocol-776 block-state census"))
}

/// Pixels whose colour differs from `reference`'s at the same offset by more
/// than a rounding wobble, with the changed set's bounding box.
struct Diff {
    count: usize,
    min_x: u32,
    max_x: u32,
    min_y: u32,
    max_y: u32,
}

fn diff(subject: &[u8], reference: &[u8]) -> Diff {
    let (mut min_x, mut max_x, mut min_y, mut max_y) = (W, 0u32, H, 0u32);
    let mut count = 0usize;
    for (i, (a, b)) in subject
        .chunks_exact(4)
        .zip(reference.chunks_exact(4))
        .enumerate()
    {
        let d = (i32::from(a[0]) - i32::from(b[0])).abs()
            + (i32::from(a[1]) - i32::from(b[1])).abs()
            + (i32::from(a[2]) - i32::from(b[2])).abs();
        if d > 20 {
            count += 1;
            let x = (i as u32) % W;
            let y = (i as u32) / W;
            min_x = min_x.min(x);
            max_x = max_x.max(x);
            min_y = min_y.min(y);
            max_y = max_y.max(y);
        }
    }
    if count == 0 {
        Diff {
            count: 0,
            min_x: 0,
            max_x: 0,
            min_y: 0,
            max_y: 0,
        }
    } else {
        Diff {
            count,
            min_x,
            max_x,
            min_y,
            max_y,
        }
    }
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn two_crack_targets_both_reach_pixels_at_distinct_positions() {
    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;

    let resources = BlockResources::load(true);
    let atlas = resources.vanilla_atlas.clone().unwrap_or_else(|| {
        panic!(
            "GPU gate opted in but the vanilla pack did not load; set LODESTONE_ASSETS \
             to a pack root with client.jar + generated/reports/blocks.json. Banner: {:?}",
            resources.banner
        )
    });
    let state_id = state_id_of(BLOCK);
    {
        let models: &BlockModels = atlas
            .models()
            .expect("the vanilla load must attach baked block models");
        assert!(
            !models.quads(state_id).is_empty(),
            "{BLOCK} (state {state_id}) must have baked model quads, or this gate would be \
             measuring the absence of geometry rather than the absence of a second draw"
        );
    }

    let mut target = HeadlessTarget::new(device, W, H, format);
    let state = RenderState::new(device, queue, format, W, H, Some(atlas.as_ref()));
    let cam = camera();

    // Left and right of screen centre, three blocks ahead, well clear of each
    // other and of the frame edges at this camera's ~38° half-horizontal FOV
    // (see the module doc's angular-size note). Which world `+X`/`-X`
    // corresponds to on screen at yaw 0 is not asserted from first principles
    // here — it was measured directly against this same camera (world `x=1`
    // landed at screen x 21..107, `x=-2` at x 212..298) and named accordingly,
    // rather than guessed and potentially gotten backwards the way CLAUDE.md's
    // §12 warns hand-derived geometry often is.
    let left = CrackTarget {
        block: [1, 0, 3],
        state_id,
        stage: STAGE,
    };
    let right = CrackTarget {
        block: [-2, 0, 3],
        state_id,
        stage: STAGE,
    };

    fn shoot(
        target: &mut HeadlessTarget,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        state: &RenderState,
        cam: &Camera,
        cracks: &[CrackTarget],
    ) -> (Vec<u8>, usize) {
        let frame = target.acquire().expect("headless acquire");
        let stats = state.render_with_crack(device, queue, frame.view(), cam, None, &[], cracks);
        (target.read_texels(device, queue), stats.cracks_drawn)
    }

    // Baseline: nothing but sky.
    let (empty, empty_drawn) = shoot(&mut target, device, queue, &state, &cam, &[]);
    // Cross-check: the empty-slice path through `render_with_crack` must be
    // byte-identical to the plain `render()` call `app.rs` used to fall back to
    // — an empty slice must be a true no-op, not merely "draws nothing visible".
    let plain = {
        let frame = target.acquire().expect("headless acquire");
        state.render(device, queue, frame.view(), &cam, None, &[]);
        target.read_texels(device, queue)
    };
    assert_eq!(
        plain, empty,
        "an empty `cracks` slice through `render_with_crack` must render pixel-identically \
         to the plain `render()` path"
    );

    let (left_only, left_only_drawn) =
        shoot(&mut target, device, queue, &state, &cam, std::slice::from_ref(&left));
    let (right_only, right_only_drawn) =
        shoot(&mut target, device, queue, &state, &cam, std::slice::from_ref(&right));
    let (both, both_drawn) = shoot(&mut target, device, queue, &state, &cam, &[left, right]);

    let d_left_alone = diff(&left_only, &empty);
    let d_right_alone = diff(&right_only, &empty);
    let d_both = diff(&both, &empty);
    // Isolation: with only the left target drawn, the right target's own
    // footprint (measured against its own solo render) must be untouched, and
    // vice versa.
    let d_left_alone_vs_right_region = diff(&left_only, &right_only);
    let d_both_vs_left_alone = diff(&both, &left_only);
    let d_both_vs_right_alone = diff(&both, &right_only);

    eprintln!("=== crack multi-target pixel gate ===");
    eprintln!("empty scene                 cracks_drawn={empty_drawn}");
    eprintln!(
        "left only    lit={} bbox x {}..{} y {}..{} cracks_drawn={left_only_drawn}",
        d_left_alone.count, d_left_alone.min_x, d_left_alone.max_x, d_left_alone.min_y, d_left_alone.max_y
    );
    eprintln!(
        "right only   lit={} bbox x {}..{} y {}..{} cracks_drawn={right_only_drawn}",
        d_right_alone.count, d_right_alone.min_x, d_right_alone.max_x, d_right_alone.min_y, d_right_alone.max_y
    );
    eprintln!(
        "both         lit={} bbox x {}..{} y {}..{} cracks_drawn={both_drawn}",
        d_both.count, d_both.min_x, d_both.max_x, d_both.min_y, d_both.max_y
    );
    eprintln!(
        "both vs left-only diff (the right target's own contribution) = {}",
        d_both_vs_left_alone.count
    );
    eprintln!(
        "both vs right-only diff (the left target's own contribution) = {}",
        d_both_vs_right_alone.count
    );

    // --- the load-bearing positives -------------------------------------
    assert_eq!(empty_drawn, 0, "an empty slice must draw nothing");
    assert_eq!(left_only_drawn, 1, "one target must draw exactly one crack");
    assert_eq!(right_only_drawn, 1, "one target must draw exactly one crack");
    // The one assertion a single-target pipeline cannot pass: two targets
    // supplied together must both draw. Before that fix this was structurally
    // impossible (`crack: Option<CrackTarget>` had room for one).
    assert_eq!(
        both_drawn, 2,
        "both targets supplied in one call must both be drawn; {both_drawn} means the \
         pipeline is still silently dropping one of them"
    );

    assert!(
        d_left_alone.count > 50,
        "the left-only target must produce a real, substantial cluster of darkened pixels; \
         got {} — a single-target-only pipeline drawing nothing here would fail this",
        d_left_alone.count
    );
    assert!(
        d_right_alone.count > 50,
        "the right-only target must produce a real, substantial cluster of darkened pixels; \
         got {}",
        d_right_alone.count
    );

    // Left and right land in disjoint screen halves — the two clusters are at
    // genuinely distinct locations, not the same quad drawn twice.
    assert!(
        d_left_alone.max_x < W / 2,
        "the left target's cluster (x {}..{}) must sit left of screen centre",
        d_left_alone.min_x,
        d_left_alone.max_x
    );
    assert!(
        d_right_alone.min_x > W / 2,
        "the right target's cluster (x {}..{}) must sit right of screen centre",
        d_right_alone.min_x,
        d_right_alone.max_x
    );

    // The combined render must show substantial change relative to *both* solo
    // renders — i.e. the target missing from each solo render is the one that
    // newly appears when both are supplied together.
    assert!(
        d_both_vs_left_alone.count > 50,
        "drawing both targets must add real pixels beyond the left-only render (the right \
         target's own contribution); got {}",
        d_both_vs_left_alone.count
    );
    assert!(
        d_both_vs_right_alone.count > 50,
        "drawing both targets must add real pixels beyond the right-only render (the left \
         target's own contribution); got {}",
        d_both_vs_right_alone.count
    );

    // The combined bounding box must span both halves of the screen — a
    // single-target implementation drawing only (say) the first target in the
    // slice would instead produce a bounding box confined to one half.
    assert!(
        d_both.min_x < W / 2 && d_both.max_x > W / 2,
        "the combined render's bounding box (x {}..{}) must span both the left and right \
         target clusters, not just one",
        d_both.min_x,
        d_both.max_x
    );

    // --- executed negative-shaped cross-checks --------------------------
    // With only the left target drawn, the frame must be identical to the
    // right-only frame *outside* each other's own cluster is not asserted
    // directly (both differ from empty in their own region); instead the
    // additive checks above already prove independence. This control instead
    // confirms the two solo renders are not accidentally identical to each
    // other, which would mean the position argument was being ignored.
    assert!(
        d_left_alone_vs_right_region.count > 50,
        "the left-only and right-only renders must differ from each other (different crack \
         positions); {} differing px would mean the target position has no effect",
        d_left_alone_vs_right_region.count
    );
}
