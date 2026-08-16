//! The wither's summon-structure block-pattern match — vanilla
//! `WitherSkullBlock.checkSpawn`/`getOrCreateWitherFull`
//! (`.cache/mc/26.2/src/net/minecraft/world/level/block/WitherSkullBlock.java`).
//!
//! Deliberately **not** built by generalising `golem::GolemCell` (a closed,
//! four-variant enum keyed to the two golem patterns) — a fifth/sixth variant
//! for soul-sand-or-soil and wither-skull-or-wall-skull would widen a type
//! this crate does not otherwise touch, for a shape ([`WitherCell`]) that
//! needs its own predicates anyway. The *engine* underneath (brute-force
//! search over a `dist × dist × dist` cube, all 24 `(forwards, up)`
//! orientations, `translate`/`consumed` in the pattern's own local frame) is
//! copied from `golem::find_golem_pattern`/`GolemPatternMatch` rather than
//! shared through a generic, because `BlockPatternBuilder`'s real engine
//! (`.../level/block/state/pattern/BlockPattern.java`) is itself duplicated
//! per call site in vanilla too — there is no shared vanilla abstraction this
//! port would be dropping by not factoring one out here.

use lodestone_model::{BlockPos, Vec3};

/// One cell of the wither's summon pattern, in the pattern's own local
/// `(right, down, forward)` axes — see `golem::GolemCell`'s own doc for what
/// this frame means; `find_wither_pattern` uses it identically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WitherCell {
    /// `BlockTags.WITHER_SUMMON_BASE_BLOCKS` — soul sand or soul soil.
    Base,
    /// `Blocks.WITHER_SKELETON_SKULL` or `Blocks.WITHER_SKELETON_WALL_SKULL`.
    Skull,
    Air,
}

impl WitherCell {
    fn matches(self, block: &str) -> bool {
        let path = block.split('[').next().unwrap_or(block);
        match self {
            WitherCell::Base => path == "minecraft:soul_sand" || path == "minecraft:soul_soil",
            WitherCell::Skull => {
                path == "minecraft:wither_skeleton_skull" || path == "minecraft:wither_skeleton_wall_skull"
            }
            WitherCell::Air => path == "minecraft:air",
        }
    }
}

/// `WitherSkullBlock.getOrCreateWitherFull`'s `.aisle("^^^", "###", "~#~")`:
/// three skulls over three base blocks over `[air, base, air]`. One aisle
/// (depth 1), rows top-to-bottom exactly as `golem::IRON_GOLEM_PATTERN`
/// already establishes the convention for (`down = 0` is the row nearest
/// vanilla's own aisle-string order, `down = 2` is `match.getBlock(1, 2, 0)`
/// — the base block vanilla's own `checkSpawn` reads as the spawn anchor).
pub(super) const WITHER_PATTERN: &[&[WitherCell]] = &[
    &[WitherCell::Skull, WitherCell::Skull, WitherCell::Skull],
    &[WitherCell::Base, WitherCell::Base, WitherCell::Base],
    &[WitherCell::Air, WitherCell::Base, WitherCell::Air],
];

/// `golem::GOLEM_PATTERN_DIRECTIONS` — vanilla `Direction.values()`, the six
/// axis-aligned unit vectors `BlockPattern.find` rotates a pattern through.
const WITHER_PATTERN_DIRECTIONS: [(i32, i32, i32); 6] =
    [(0, -1, 0), (0, 1, 0), (0, 0, -1), (0, 0, 1), (-1, 0, 0), (1, 0, 0)];

fn vec3i_cross(a: (i32, i32, i32), b: (i32, i32, i32)) -> (i32, i32, i32) {
    (a.1 * b.2 - a.2 * b.1, a.2 * b.0 - a.0 * b.2, a.0 * b.1 - a.1 * b.0)
}

fn vec3i_neg(v: (i32, i32, i32)) -> (i32, i32, i32) {
    (-v.0, -v.1, -v.2)
}

/// A matched wither pattern's orientation — see `golem::GolemPatternMatch`'s
/// own doc, identical shape.
#[derive(Debug, Clone, Copy)]
pub(super) struct WitherPatternMatch {
    origin: (i32, i32, i32),
    forwards: (i32, i32, i32),
    up: (i32, i32, i32),
}

impl WitherPatternMatch {
    pub(super) fn translate(&self, right: i32, down: i32, forward: i32) -> (i32, i32, i32) {
        let r = vec3i_cross(self.forwards, self.up);
        (
            self.origin.0 + self.up.0 * -down + r.0 * right + self.forwards.0 * forward,
            self.origin.1 + self.up.1 * -down + r.1 * right + self.forwards.1 * forward,
            self.origin.2 + self.up.2 * -down + r.2 * right + self.forwards.2 * forward,
        )
    }

    /// Every non-air cell's world position — `CarvedPumpkinBlock.
    /// clearPatternBlocks`'s wither-specific caller
    /// (`WitherSkullBlock.checkSpawn`) walks the same match to air the three
    /// skulls and three base blocks once the wither is spawned.
    pub(super) fn consumed(&self) -> Vec<BlockPos> {
        let mut out = Vec::new();
        for (down, row) in WITHER_PATTERN.iter().enumerate() {
            for (right, &cell) in row.iter().enumerate() {
                if cell != WitherCell::Air {
                    let (x, y, z) = self.translate(right as i32, down as i32, 0);
                    out.push(BlockPos::new(x, y, z));
                }
            }
        }
        out
    }

    /// `match.getBlock(1, 2, 0)` — the bottom-centre base block's cell, the
    /// spawn anchor `WitherSkullBlock.checkSpawn` reads.
    pub(super) fn spawn_anchor(&self) -> (i32, i32, i32) {
        self.translate(1, 2, 0)
    }
}

fn wither_pattern_matches(
    block_at: &dyn Fn(i32, i32, i32) -> String,
    candidate: &WitherPatternMatch,
) -> bool {
    for (down, row) in WITHER_PATTERN.iter().enumerate() {
        for (right, &cell) in row.iter().enumerate() {
            let (x, y, z) = candidate.translate(right as i32, down as i32, 0);
            if !cell.matches(&block_at(x, y, z)) {
                return false;
            }
        }
    }
    true
}

/// `WitherSkullBlock.getOrCreateWitherFull().find(level, pos)` — brute-forces
/// every position in the `dist × dist × dist` cube starting at `placed`, and
/// every one of the 24 `(forwards, up)` orientations, exactly as
/// `golem::find_golem_pattern` does (see that function's own doc for why 24
/// orientations, not just "upright", is real vanilla behaviour: a wither can
/// be summoned lying on its side against a wall too).
pub(super) fn find_wither_pattern(
    block_at: &dyn Fn(i32, i32, i32) -> String,
    placed: (i32, i32, i32),
) -> Option<WitherPatternMatch> {
    let height = WITHER_PATTERN.len() as i32;
    let width = WITHER_PATTERN.iter().map(|row| row.len()).max().unwrap_or(0) as i32;
    let dist = height.max(width).max(1);
    for dx in 0..dist {
        for dy in 0..dist {
            for dz in 0..dist {
                let origin = (placed.0 + dx, placed.1 + dy, placed.2 + dz);
                for &forwards in &WITHER_PATTERN_DIRECTIONS {
                    for &up in &WITHER_PATTERN_DIRECTIONS {
                        if up == forwards || up == vec3i_neg(forwards) {
                            continue;
                        }
                        let candidate = WitherPatternMatch { origin, forwards, up };
                        if wither_pattern_matches(block_at, &candidate) {
                            return Some(candidate);
                        }
                    }
                }
            }
        }
    }
    None
}

/// A pattern-block cell's world position → the wither's spawn position —
/// `witherBoss.snapTo(spawnPos.getX() + 0.5, spawnPos.getY() + 0.55,
/// spawnPos.getZ() + 0.5, ...)`. The `0.55` (not the golem's `0.05`) is
/// transcribed exactly as vanilla's own literal, not assumed to match.
pub(super) fn wither_anchor_to_spawn_pos((x, y, z): (i32, i32, i32)) -> Vec3 {
    Vec3::new(f64::from(x) + 0.5, f64::from(y) + 0.55, f64::from(z) + 0.5)
}

/// `match.getForwards().getAxis() == Direction.Axis.X ? 0.0F : 90.0F` — the
/// wither's spawn yaw, from the matched orientation's forward axis.
pub(super) fn wither_spawn_yaw(forwards: (i32, i32, i32)) -> f32 {
    if forwards.0 != 0 { 0.0 } else { 90.0 }
}

impl WitherPatternMatch {
    pub(super) fn forwards(&self) -> (i32, i32, i32) {
        self.forwards
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world_from(blocks: &[((i32, i32, i32), &str)]) -> impl Fn(i32, i32, i32) -> String {
        let map: std::collections::HashMap<(i32, i32, i32), String> =
            blocks.iter().map(|(pos, name)| (*pos, (*name).to_owned())).collect();
        move |x, y, z| map.get(&(x, y, z)).cloned().unwrap_or_else(|| "minecraft:air".to_owned())
    }

    /// The real wither shape, standing upright: three skulls over three soul
    /// sand over `[air, soul sand, air]`, worked out independently against
    /// `translate`'s formula the same way `golem_tests` does for the iron
    /// golem, not copied from vanilla's own test suite (there is none in the
    /// decompile).
    #[test]
    fn an_upright_wither_pattern_matches_and_reports_the_right_anchor() {
        let world = world_from(&[
            ((9, 6, 10), "minecraft:wither_skeleton_skull"),
            ((10, 6, 10), "minecraft:wither_skeleton_skull"),
            ((11, 6, 10), "minecraft:wither_skeleton_skull"),
            ((9, 5, 10), "minecraft:soul_sand"),
            ((10, 5, 10), "minecraft:soul_sand"),
            ((11, 5, 10), "minecraft:soul_sand"),
            ((10, 4, 10), "minecraft:soul_soil"),
        ]);
        let found = find_wither_pattern(&world, (10, 6, 10)).expect("a complete wither pattern must match");
        let anchor = found.spawn_anchor();
        assert_eq!(anchor, (10, 4, 10), "the anchor is the bottom-centre base block");

        let mut consumed: Vec<(i32, i32, i32)> = found.consumed().iter().map(|p| (p.x, p.y, p.z)).collect();
        consumed.sort_unstable();
        let mut expected = vec![
            (9, 6, 10),
            (10, 6, 10),
            (11, 6, 10),
            (9, 5, 10),
            (10, 5, 10),
            (11, 5, 10),
            (10, 4, 10),
        ];
        expected.sort_unstable();
        assert_eq!(consumed, expected, "every skull and base block must be reported, and nothing else");
    }

    /// **Control**: the same seven-block count is not enough on its own —
    /// remove the load-bearing centre-bottom soul-soil block and the
    /// otherwise-identical structure must not match. Without this, a
    /// permissive matcher (e.g. "3 skulls plus enough soul sand nearby")
    /// would pass the test above for the wrong reason.
    #[test]
    fn control_a_pattern_missing_its_bottom_base_block_does_not_match() {
        let world = world_from(&[
            ((9, 6, 10), "minecraft:wither_skeleton_skull"),
            ((10, 6, 10), "minecraft:wither_skeleton_skull"),
            ((11, 6, 10), "minecraft:wither_skeleton_skull"),
            ((9, 5, 10), "minecraft:soul_sand"),
            ((10, 5, 10), "minecraft:soul_sand"),
            ((11, 5, 10), "minecraft:soul_sand"),
            // (10, 4, 10) missing.
        ]);
        assert!(
            find_wither_pattern(&world, (10, 6, 10)).is_none(),
            "an incomplete pattern must never match"
        );
    }

    /// Soul soil completes the pattern exactly as soul sand does —
    /// `WITHER_SUMMON_BASE_BLOCKS` accepts both — and a wall/floor skull
    /// mix (skull + two wall skulls) is legal too. Both mixed at once, to
    /// discriminate a matcher keyed on only one literal block name.
    #[test]
    fn soul_soil_and_wall_skulls_complete_the_pattern_too() {
        let world = world_from(&[
            ((9, 6, 10), "minecraft:wither_skeleton_wall_skull"),
            ((10, 6, 10), "minecraft:wither_skeleton_skull"),
            ((11, 6, 10), "minecraft:wither_skeleton_wall_skull"),
            ((9, 5, 10), "minecraft:soul_soil"),
            ((10, 5, 10), "minecraft:soul_sand"),
            ((11, 5, 10), "minecraft:soul_soil"),
            ((10, 4, 10), "minecraft:soul_sand"),
        ]);
        assert!(find_wither_pattern(&world, (10, 6, 10)).is_some());
    }

    /// The 24-orientation search actually engages: the same T built sideways
    /// against a wall (`up = +X`) must still match — the same trap
    /// `golem_tests::an_iron_golem_built_sideways_against_a_wall_still_matches`
    /// exists to catch.
    #[test]
    fn a_wither_built_sideways_against_a_wall_still_matches() {
        // The upright fixture above with X and Y swapped (world `+X` stands
        // in for the vertical axis instead of world `+Y`) — a mechanical
        // transform of a known-good fixture rather than a hand-derived
        // orientation, so there is no risk of an inconsistent geometry that
        // no orientation of the search could ever match.
        let world = world_from(&[
            ((6, 9, 10), "minecraft:wither_skeleton_skull"),
            ((6, 10, 10), "minecraft:wither_skeleton_skull"),
            ((6, 11, 10), "minecraft:wither_skeleton_skull"),
            ((5, 9, 10), "minecraft:soul_sand"),
            ((5, 10, 10), "minecraft:soul_sand"),
            ((5, 11, 10), "minecraft:soul_sand"),
            ((4, 10, 10), "minecraft:soul_soil"),
        ]);
        assert!(
            find_wither_pattern(&world, (6, 10, 10)).is_some(),
            "a sideways-built wither pattern must still match"
        );
    }

    #[test]
    fn spawn_yaw_is_zero_on_the_x_axis_and_ninety_otherwise() {
        assert_eq!(wither_spawn_yaw((1, 0, 0)), 0.0);
        assert_eq!(wither_spawn_yaw((-1, 0, 0)), 0.0);
        assert_eq!(wither_spawn_yaw((0, 0, 1)), 90.0);
        assert_eq!(wither_spawn_yaw((0, 1, 0)), 90.0, "up/down forwards is not the X axis either");
    }

    #[test]
    fn spawn_pos_offsets_are_the_wither_specific_zero_point_five_five_not_the_golem_zero_point_zero_five() {
        let pos = wither_anchor_to_spawn_pos((10, 4, 10));
        assert_eq!(pos, Vec3::new(10.5, 4.55, 10.5));
    }
}
