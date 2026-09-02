//! Pixel gate: two *other* players' block-crack overlays reach pixels through
//! the real per-frame **gather**, not through a hand-built `Vec<CrackTarget>`.
//!
//! # What this proves that `crack_multi_target_pixels.rs` does not
//!
//! `crack_multi_target_pixels.rs` already proves `CrackPipeline`/
//! `render_with_crack` can draw N targets in one call — it constructs the
//! `[CrackTarget; 2]` slice by hand. That is necessary but not sufficient:
//! before this pass, nothing in the shell ever *produced* more than one
//! `CrackTarget` per frame, because `lodestone_game::mining::
//! BlockDestructionOverlays` exposed only `stage_at`/`len`/`is_empty` — a
//! single-position probe, useless to a per-frame loop that has no position to
//! probe with. `Sim::block_destruction_stage_at` (`91f19db`) was that same
//! probe re-exposed on `Sim` and could not serve the loop either. So a
//! pipeline that draws two hand-built targets fine can still, in the real
//! client, only ever have zero or one target reach it — the exact gap that fix
//! reports.
//!
//! This gate closes that gap end to end, using only production code:
//!
//! 1. **`BlockDestructionOverlays::apply`** (existing, real production fold)
//!    ingests two real `ClientEvent::BlockDestruction` events from two
//!    different breaking entities at two different positions — the same
//!    fold `lodestone_ecs::session::apply_block_destruction` runs on the
//!    live client path.
//! 2. **`BlockDestructionOverlays::iter`** (new, this pass) — the enumeration
//!    accessor that lets a caller walk every active overlay with no position
//!    known in advance, which `stage_at` structurally cannot do.
//! 3. **`gpu::gather_crack_targets`** (new, this pass) — the actual per-frame
//!    gather: local target (`None` here — this gate is about *other*
//!    players) plus one `CrackTarget` per active overlay, resolved through a
//!    `resolve` callback the same way `Sim::crack_target` resolves the local
//!    dig. This is the exact function `Sim`'s per-frame call wires up to
//!    (see the accompanying patch); the only thing this test does not invoke
//!    is the one-line `Sim`/`app.rs` call site, because both files are a
//!    brokered choke point shared with a concurrent `sim.rs` refactor and are
//!    handed over as a patch rather than edited here.
//! 4. **`RenderState::render_with_crack`** draws the gathered slice.
//!
//! So the `CrackTarget`s reaching the pixel buffer below were produced by
//! folding real wire events through the real overlay collection and the real
//! gather function — never typed in as a `CrackTarget` literal.
//!
//! # Scene and controls
//!
//! Same two screen-half positions and diff/bbox methodology as
//! `crack_multi_target_pixels.rs` (measure by location, not frame average):
//! left (`world x=1`) and right (`world x=-2`), three blocks ahead, at
//! `minecraft:stone`. Entities `301` and `402` each break one of them at
//! max stage (9). Controls, all executed:
//!
//! * **empty overlays** (nothing folded in): the gather must produce an empty
//!   slice and render pixel-identical to the sky-only baseline — proves the
//!   gather is not fabricating a target from nothing.
//! * **one overlay only**: gather must produce exactly one `CrackTarget`,
//!   lit only in that target's own screen half.
//! * **both overlays**: gather must produce exactly two `CrackTarget`s, both
//!   reaching pixels, in disjoint bounding boxes that together span both
//!   screen halves — the assertion a single-position `stage_at` probe could
//!   never satisfy, since it cannot enumerate a second entity without
//!   already knowing where to look.
//!
//! Fail-closed like its siblings: a missing GPU or a missing `client.jar` is
//! a failure, never a skip.
//!
//! ```text
//! cargo test -p lodestone-shell --test crack_live_gather_pixels -- --ignored --nocapture
//! ```

use lodestone::gpu::{CrackTarget, RenderState, gather_crack_targets};
use lodestone::resources::BlockResources;
use lodestone_game::mining::BlockDestructionOverlays;
use lodestone_model::ClientEvent;
use lodestone_model::math::BlockPos;
use lodestone_render::{BlockModels, Camera, GpuContext, HeadlessTarget, RenderTarget};

const W: u32 = 320;
const H: u32 = 240;

/// `minecraft:stone` — an ordinary opaque full cube, guaranteed real baked
/// geometry, and nowhere near any block used by another live pixel gate in
/// this crate.
const BLOCK: &str = "minecraft:stone";

/// Maximum real destroy stage. `CrackResolver::mesh_for`'s doc: a stage
/// outside `0..=9` draws nothing (vanilla clears the overlay at `>= 10`), so
/// 9 is both the last real stage and the most visually distinct from "no
/// crack".
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
/// convention as `crack_multi_target_pixels.rs`'s `state_id_of`.
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
fn two_other_players_overlays_reach_pixels_through_the_live_gather() {
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

    // Same two positions `crack_multi_target_pixels.rs` measured directly
    // against this camera: world `x=1` lands at screen x 21..107, `x=-2` at
    // x 212..298, at yaw 0.
    let left_pos = BlockPos::new(1, 0, 3);
    let right_pos = BlockPos::new(-2, 0, 3);

    // The `resolve` callback every entry point below shares: exactly what
    // `Sim::crack_target`'s offline branch does (`self.block_at_world`), but
    // there is no live world here, so it is stubbed to always answer with
    // this gate's one real state id — the resolution *policy* (live vs.
    // demo world) is `Sim`'s brokered one-liner, not part of what this gate
    // is proving. What *is* proved is that `gather_crack_targets` calls
    // `resolve` once per active overlay and turns each answer into a real
    // `CrackTarget`.
    let resolve = |_pos: BlockPos| Some(state_id);

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

    // --- empty: nothing folded into the overlay collection --------------
    let empty_overlays = BlockDestructionOverlays::new();
    let empty_gathered = gather_crack_targets(None, empty_overlays.iter(), resolve);
    assert!(
        empty_gathered.is_empty(),
        "an empty BlockDestructionOverlays must gather nothing"
    );
    let (empty, empty_drawn) = shoot(&mut target, device, queue, &state, &cam, &empty_gathered);

    let plain = {
        let frame = target.acquire().expect("headless acquire");
        state.render(device, queue, frame.view(), &cam, None, &[]);
        target.read_texels(device, queue)
    };
    assert_eq!(
        plain, empty,
        "an empty gathered slice must render pixel-identically to the plain render() path"
    );

    // --- one overlay: entity 301 breaking the left block -----------------
    let mut one_overlay = BlockDestructionOverlays::new();
    one_overlay.apply(&ClientEvent::BlockDestruction {
        entity_id: 301,
        pos: left_pos,
        progress: STAGE,
    });
    let one_gathered = gather_crack_targets(None, one_overlay.iter(), resolve);
    assert_eq!(
        one_gathered.len(),
        1,
        "one active overlay must gather exactly one CrackTarget"
    );
    let (left_only, left_only_drawn) =
        shoot(&mut target, device, queue, &state, &cam, &one_gathered);

    // --- two overlays: entities 301 and 402, two different positions -----
    let mut two_overlays = BlockDestructionOverlays::new();
    two_overlays.apply(&ClientEvent::BlockDestruction {
        entity_id: 301,
        pos: left_pos,
        progress: STAGE,
    });
    two_overlays.apply(&ClientEvent::BlockDestruction {
        entity_id: 402,
        pos: right_pos,
        progress: STAGE,
    });
    let mut enumerated: Vec<(BlockPos, u8)> = two_overlays.iter().collect();
    enumerated.sort_by_key(|(p, _)| (p.x, p.y, p.z));
    assert_eq!(
        enumerated,
        vec![(right_pos, STAGE), (left_pos, STAGE)],
        "iter() must enumerate both breaking entities' positions"
    );
    let two_gathered = gather_crack_targets(None, two_overlays.iter(), resolve);
    assert_eq!(
        two_gathered.len(),
        2,
        "two active overlays from two different entities must gather exactly two CrackTargets \
         — this is the enumeration #410 reports missing"
    );
    let (both, both_drawn) = shoot(&mut target, device, queue, &state, &cam, &two_gathered);

    // Right-only, for isolation, built the same live way.
    let mut right_overlay = BlockDestructionOverlays::new();
    right_overlay.apply(&ClientEvent::BlockDestruction {
        entity_id: 402,
        pos: right_pos,
        progress: STAGE,
    });
    let right_gathered = gather_crack_targets(None, right_overlay.iter(), resolve);
    let (right_only, right_only_drawn) =
        shoot(&mut target, device, queue, &state, &cam, &right_gathered);

    let d_left_alone = diff(&left_only, &empty);
    let d_right_alone = diff(&right_only, &empty);
    let d_both = diff(&both, &empty);
    let d_left_alone_vs_right_region = diff(&left_only, &right_only);
    let d_both_vs_left_alone = diff(&both, &left_only);
    let d_both_vs_right_alone = diff(&both, &right_only);

    eprintln!("=== crack live-gather pixel gate (issue #410) ===");
    eprintln!("empty overlays               cracks_drawn={empty_drawn}");
    eprintln!(
        "entity 301 only (left)  lit={} bbox x {}..{} y {}..{} cracks_drawn={left_only_drawn}",
        d_left_alone.count, d_left_alone.min_x, d_left_alone.max_x, d_left_alone.min_y, d_left_alone.max_y
    );
    eprintln!(
        "entity 402 only (right) lit={} bbox x {}..{} y {}..{} cracks_drawn={right_only_drawn}",
        d_right_alone.count, d_right_alone.min_x, d_right_alone.max_x, d_right_alone.min_y, d_right_alone.max_y
    );
    eprintln!(
        "both entities            lit={} bbox x {}..{} y {}..{} cracks_drawn={both_drawn}",
        d_both.count, d_both.min_x, d_both.max_x, d_both.min_y, d_both.max_y
    );
    eprintln!(
        "both vs left-only diff (entity 402's own contribution) = {}",
        d_both_vs_left_alone.count
    );
    eprintln!(
        "both vs right-only diff (entity 301's own contribution) = {}",
        d_both_vs_right_alone.count
    );

    // --- the load-bearing positives -------------------------------------
    assert_eq!(empty_drawn, 0, "an empty gathered slice must draw nothing");
    assert_eq!(left_only_drawn, 1, "one overlay must draw exactly one crack");
    assert_eq!(right_only_drawn, 1, "one overlay must draw exactly one crack");
    assert_eq!(
        both_drawn, 2,
        "two overlays from two different breaking entities, gathered through the live \
         BlockDestructionOverlays -> iter() -> gather_crack_targets path, must both draw; \
         {both_drawn} means the enumeration is still silently dropping one of them"
    );

    assert!(
        d_left_alone.count > 50,
        "entity 301's overlay must produce a real, substantial cluster of darkened pixels; \
         got {}",
        d_left_alone.count
    );
    assert!(
        d_right_alone.count > 50,
        "entity 402's overlay must produce a real, substantial cluster of darkened pixels; \
         got {}",
        d_right_alone.count
    );

    assert!(
        d_left_alone.max_x < W / 2,
        "entity 301's cluster (x {}..{}) must sit left of screen centre",
        d_left_alone.min_x,
        d_left_alone.max_x
    );
    assert!(
        d_right_alone.min_x > W / 2,
        "entity 402's cluster (x {}..{}) must sit right of screen centre",
        d_right_alone.min_x,
        d_right_alone.max_x
    );

    assert!(
        d_both_vs_left_alone.count > 50,
        "gathering both overlays must add real pixels beyond the entity-301-only render \
         (entity 402's own contribution); got {}",
        d_both_vs_left_alone.count
    );
    assert!(
        d_both_vs_right_alone.count > 50,
        "gathering both overlays must add real pixels beyond the entity-402-only render \
         (entity 301's own contribution); got {}",
        d_both_vs_right_alone.count
    );

    assert!(
        d_both.min_x < W / 2 && d_both.max_x > W / 2,
        "the combined render's bounding box (x {}..{}) must span both entities' clusters, \
         not just one — a single-target-capable gather would instead produce a bounding box \
         confined to one half",
        d_both.min_x,
        d_both.max_x
    );

    assert!(
        d_left_alone_vs_right_region.count > 50,
        "the two overlays' solo renders must differ from each other; {} differing px would \
         mean the gathered position has no effect",
        d_left_alone_vs_right_region.count
    );
}
