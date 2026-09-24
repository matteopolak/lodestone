//! Structural control for the dense Java benchmark scene.
//!
//! This does not claim Java accepts every command; the live runner owns that
//! check. It prevents a scene edit from silently dropping one of the render
//! paths the benchmark exists to exercise.

#[test]
fn showcase_exercises_every_requested_render_path() {
    let scene = include_str!("../../../../scripts/benchmark-scenes/showcase.txt");
    for required in [
        "_sign[",
        "player_head",
        "_banner[",
        "item_frame",
        "map_id",
        "summon armor_stand",
        "summon sheep",
        "summon text_display",
        "summon item_display",
        "summon block_display",
        "particle ",
    ] {
        assert!(scene.contains(required), "showcase misses {required}");
    }
    assert!(scene.matches("item_frame").count() >= 16);
    assert!(scene.matches("_sign[").count() >= 24);
    assert!(scene.matches("_banner[").count() >= 16);
    assert!(scene.matches("player_head").count() >= 16);
    assert!(scene.matches("summon armor_stand").count() >= 12);
    assert!(scene.matches("summon sheep").count() >= 24);
}
