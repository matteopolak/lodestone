//! Tests for the draw-time animation seam on [`AtlasSprite`]: resolving an
//! absolute tick to the current/next physical frame and an interpolation blend,
//! faithfully mirroring vanilla's own sprite-contents animation-state class.

use lodestone_assets::AnimationFrame;
use lodestone_assets::{AtlasSprite, ResourceLocation};

fn sprite(frametime: u32, interpolate: bool, frames: Vec<AnimationFrame>) -> AtlasSprite {
    let frame_count = frames.len() as u32;
    AtlasSprite {
        location: ResourceLocation::parse("minecraft:block/x").unwrap(),
        layer: 0,
        x: 0,
        y: 0,
        width: 16,
        height: 16 * frame_count,
        uv_min: [0.0, 0.0],
        uv_max: [1.0, 1.0],
        frame_count,
        frame_height: 16,
        frametime,
        interpolate,
        frames,
        anim_slot: 0,
    }
}

fn f(index: u32, time: Option<u32>) -> AnimationFrame {
    AnimationFrame { index, time }
}

#[test]
fn static_sprite_resolves_to_frame_zero_always() {
    let s = sprite(1, false, vec![f(0, None)]);
    for tick in [0u64, 1, 5, 999] {
        let sample = s.frame_at_tick(tick);
        assert_eq!(sample.current, 0);
        assert_eq!(sample.next, 0);
    }
}

#[test]
fn cycle_length_sums_per_slot_durations() {
    // Two slots defaulting to frametime 2 -> cycle 4.
    let s = sprite(2, false, vec![f(0, None), f(1, None)]);
    assert_eq!(s.cycle_ticks(), 4);
    // Explicit per-slot times override frametime: 1 + 3 = 4.
    let s2 = sprite(10, false, vec![f(0, Some(1)), f(1, Some(3))]);
    assert_eq!(s2.cycle_ticks(), 4);
}

#[test]
fn tick_advances_slots_and_interpolation_blend_tracks_subframe() {
    // 2 frames, frametime 2 -> each slot lasts 2 ticks, cycle 4.
    let s = sprite(2, true, vec![f(0, None), f(1, None)]);

    let t0 = s.frame_at_tick(0);
    assert_eq!((t0.current, t0.next), (0, 1));
    assert!(t0.blend.abs() < 1e-6, "start of slot -> blend 0");

    let t1 = s.frame_at_tick(1);
    assert_eq!((t1.current, t1.next), (0, 1));
    assert!(
        (t1.blend - 0.5).abs() < 1e-6,
        "half through slot 0 -> blend 0.5"
    );

    let t2 = s.frame_at_tick(2);
    assert_eq!(
        (t2.current, t2.next),
        (1, 0),
        "slot 1 wraps next back to frame 0"
    );
    assert!(t2.blend.abs() < 1e-6);

    // Cycle repeats.
    let t4 = s.frame_at_tick(4);
    assert_eq!((t4.current, t4.next), (0, 1));
    assert!(t4.blend.abs() < 1e-6);
}

#[test]
fn per_frame_times_shape_the_blend() {
    // Slot 0 lasts 1 tick, slot 1 lasts 3 ticks. cycle 4.
    let s = sprite(99, true, vec![f(0, Some(1)), f(1, Some(3))]);
    assert_eq!(s.cycle_ticks(), 4);

    let a = s.frame_at_tick(1); // enters slot 1, subframe 0
    assert_eq!((a.current, a.next), (1, 0));
    assert!(a.blend.abs() < 1e-6);

    let b = s.frame_at_tick(2); // slot 1, subframe 1 of 3
    assert!((b.blend - 1.0 / 3.0).abs() < 1e-6, "blend {}", b.blend);

    let c = s.frame_at_tick(3); // slot 1, subframe 2 of 3
    assert!((c.blend - 2.0 / 3.0).abs() < 1e-6, "blend {}", c.blend);
}

#[test]
fn non_default_frame_order_is_honoured() {
    // Explicit playback order 2,0,1 (each frametime 1).
    let s = sprite(1, false, vec![f(2, None), f(0, None), f(1, None)]);
    assert_eq!(s.frame_at_tick(0).current, 2);
    assert_eq!(s.frame_at_tick(1).current, 0);
    assert_eq!(s.frame_at_tick(2).current, 1);
    assert_eq!(s.frame_at_tick(3).current, 2);
}
