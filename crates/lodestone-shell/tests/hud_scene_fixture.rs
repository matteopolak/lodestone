//! Hermetic invariants for the live `05-hud` screenshot scene.
//!
//! The live capture remains the pixel oracle, but these assertions catch two
//! scene-construction mistakes without requiring RCON, a GPU, or a vanilla jar:
//! outward-facing double-chest halves and transparent lantern models replacing
//! the only opaque layer of their wall.

const HUD_SCENE: &str = include_str!("../../../scripts/screenshot-scenes/05-hud.txt");

fn has_command(command: &str) -> bool {
    HUD_SCENE
        .lines()
        .map(str::trim)
        .any(|line| line == command)
}

#[test]
fn south_facing_chest_halves_connect_inward() {
    assert!(has_command(
        "setblock -1 64 18 minecraft:chest[facing=south,type=right]"
    ));
    assert!(has_command(
        "setblock 0 64 18 minecraft:chest[facing=south,type=left]"
    ));
}

#[test]
fn lanterns_sit_in_backed_roofed_alcoves() {
    for command in [
        "setblock -8 64 21 minecraft:lantern[hanging=false]",
        "setblock 8 64 21 minecraft:lantern[hanging=false]",
        "fill -9 64 21 -7 65 22 minecraft:stone_bricks hollow",
        "fill 7 64 21 9 65 22 minecraft:stone_bricks hollow",
    ] {
        assert!(
            has_command(command),
            "missing HUD scene command: {command}"
        );
    }
}
