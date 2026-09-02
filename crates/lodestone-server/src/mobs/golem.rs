//! Golem construction from a placed pumpkin — the snow-golem and iron-golem
//! block-pattern match and spawn. Moved out of `mobs/mod.rs` verbatim as part
//! of the `mobs.rs` file split (see `docs/plans/crate-and-file-splits.md`).

use lodestone_model::{BlockPos, ResourceKey, Vec3};

use super::{ChunkWorld, MobSim};

/// One cell of a golem-construction block pattern, in the pattern's own
/// local `(right, down, forward)` axes — vanilla's own block-pattern frame,
/// where `(0, 0, 0)` is the
/// anchor cell `find` requires to land exactly on the placed pumpkin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GolemCell {
    Iron,
    Snow,
    Pumpkin,
    Air,
}

impl GolemCell {
    /// Vanilla's own block predicates: `BlockStatePredicate.forBlock` for
    /// iron/snow, the carved-pumpkin-or-jack-o'-lantern `Predicate` literal
    /// (`CarvedPumpkinBlock.PUMPKINS_PREDICATE`), and `BlockStateBase::isAir`
    /// for the required-clear cells.
    fn matches(self, block: &str) -> bool {
        // Strip any `[prop=value]` state suffix so this compares block
        // identity only, matching `BlockStatePredicate.forBlock`.
        let path = block.split('[').next().unwrap_or(block);
        match self {
            GolemCell::Iron => path == "minecraft:iron_block",
            GolemCell::Snow => path == "minecraft:snow_block",
            GolemCell::Pumpkin => {
                path == "minecraft:carved_pumpkin" || path == "minecraft:jack_o_lantern"
            }
            GolemCell::Air => path == "minecraft:air",
        }
    }
}

/// The snow golem's pattern (`CarvedPumpkinBlock.getOrCreateSnowGolemFull`):
/// one column, a pumpkin over two snow blocks. Indexed `[down][right]`, a
/// single `forward` layer (depth 1, `.aisle(...)` called once).
pub(super) const SNOW_GOLEM_PATTERN: &[&[GolemCell]] =
    &[&[GolemCell::Pumpkin], &[GolemCell::Snow], &[GolemCell::Snow]];

/// The iron golem's pattern (`getOrCreateIronGolemFull`): a T of iron blocks
/// topped with a pumpkin, air filling the two corners of the top row and the
/// two corners of the bottom row (`~^~` / `###` / `~#~` in the source's own
/// aisle notation, `~` meaning "must be air").
pub(super) const IRON_GOLEM_PATTERN: &[&[GolemCell]] = &[
    &[GolemCell::Air, GolemCell::Pumpkin, GolemCell::Air],
    &[GolemCell::Iron, GolemCell::Iron, GolemCell::Iron],
    &[GolemCell::Air, GolemCell::Iron, GolemCell::Air],
];

/// The six axis-aligned unit vectors `BlockPattern.find` rotates a pattern
/// through (vanilla `Direction.values()`), as `(dx, dy, dz)`.
const GOLEM_PATTERN_DIRECTIONS: [(i32, i32, i32); 6] = [
    (0, -1, 0),
    (0, 1, 0),
    (0, 0, -1),
    (0, 0, 1),
    (-1, 0, 0),
    (1, 0, 0),
];

fn vec3i_cross(a: (i32, i32, i32), b: (i32, i32, i32)) -> (i32, i32, i32) {
    (
        a.1 * b.2 - a.2 * b.1,
        a.2 * b.0 - a.0 * b.2,
        a.0 * b.1 - a.1 * b.0,
    )
}

fn vec3i_neg(v: (i32, i32, i32)) -> (i32, i32, i32) {
    (-v.0, -v.1, -v.2)
}

/// A matched golem pattern's orientation — vanilla `BlockPattern.
/// BlockPatternMatch`'s `(frontTopLeft, forwards, up)` triple, sufficient to
/// re-derive any cell's world position.
#[derive(Debug, Clone, Copy)]
pub(super) struct GolemPatternMatch {
    origin: (i32, i32, i32),
    forwards: (i32, i32, i32),
    up: (i32, i32, i32),
}

impl GolemPatternMatch {
    /// Vanilla `BlockPattern.translateAndRotate`: the world cell at local
    /// `(right, down, forward)`.
    pub(super) fn translate(&self, right: i32, down: i32, forward: i32) -> (i32, i32, i32) {
        let r = vec3i_cross(self.forwards, self.up);
        (
            self.origin.0 + self.up.0 * -down + r.0 * right + self.forwards.0 * forward,
            self.origin.1 + self.up.1 * -down + r.1 * right + self.forwards.1 * forward,
            self.origin.2 + self.up.2 * -down + r.2 * right + self.forwards.2 * forward,
        )
    }

    /// Every non-air cell's world position — what vanilla
    /// `CarvedPumpkinBlock.clearPatternBlocks` iterates to clear (it walks
    /// every cell including the air ones, but clearing air to air is a
    /// no-op, so only the real blocks are worth reporting to a caller).
    pub(super) fn consumed(&self, pattern: &[&[GolemCell]]) -> Vec<BlockPos> {
        let mut out = Vec::new();
        for (down, row) in pattern.iter().enumerate() {
            for (right, &cell) in row.iter().enumerate() {
                if cell != GolemCell::Air {
                    let (x, y, z) = self.translate(right as i32, down as i32, 0);
                    out.push(BlockPos::new(x, y, z));
                }
            }
        }
        out
    }
}

fn golem_pattern_matches(
    block_at: &dyn Fn(i32, i32, i32) -> String,
    pattern: &[&[GolemCell]],
    candidate: &GolemPatternMatch,
) -> bool {
    for (down, row) in pattern.iter().enumerate() {
        for (right, &cell) in row.iter().enumerate() {
            let (x, y, z) = candidate.translate(right as i32, down as i32, 0);
            if !cell.matches(&block_at(x, y, z)) {
                return false;
            }
        }
    }
    true
}

/// Vanilla `BlockPattern.find`: brute-forces every position in the
/// `dist × dist × dist` cube starting at `placed` (inclusive of `placed`
/// itself), and every valid `(forwards, up)` axis pair (24 orientations —
/// `up` excludes only parallel-to-`forwards`), until one fully matches.
///
/// **This is why a golem can be built lying on its side or upside down, not
/// only standing upright** — real vanilla behaviour (the search tries all 24
/// orientations, not "up"), not a generalisation invented for this port. A
/// matcher that only tried `up = +Y` would silently reject a legally-built
/// sideways golem.
pub(super) fn find_golem_pattern(
    block_at: &dyn Fn(i32, i32, i32) -> String,
    pattern: &[&[GolemCell]],
    placed: (i32, i32, i32),
) -> Option<GolemPatternMatch> {
    let height = pattern.len() as i32;
    let width = pattern.iter().map(|row| row.len()).max().unwrap_or(0) as i32;
    // vanilla's `dist = max(width, height, depth)`; `depth` is always 1 for
    // both patterns here (a single `.aisle(...)` call each).
    let dist = height.max(width).max(1);
    for dx in 0..dist {
        for dy in 0..dist {
            for dz in 0..dist {
                let origin = (placed.0 + dx, placed.1 + dy, placed.2 + dz);
                for &forwards in &GOLEM_PATTERN_DIRECTIONS {
                    for &up in &GOLEM_PATTERN_DIRECTIONS {
                        if up == forwards || up == vec3i_neg(forwards) {
                            continue;
                        }
                        let candidate = GolemPatternMatch {
                            origin,
                            forwards,
                            up,
                        };
                        if golem_pattern_matches(block_at, pattern, &candidate) {
                            return Some(candidate);
                        }
                    }
                }
            }
        }
    }
    None
}

/// A pattern-block cell's world position → the golem's spawn position
/// (vanilla `golem.snapTo(spawnPos.getX() + 0.5, spawnPos.getY() + 0.05,
/// spawnPos.getZ() + 0.5, 0.0F, 0.0F)`).
pub(super) fn golem_feet_to_spawn_pos((x, y, z): (i32, i32, i32)) -> Vec3 {
    Vec3::new(f64::from(x) + 0.5, f64::from(y) + 0.05, f64::from(z) + 0.5)
}

/// Which golem [`MobSim::try_construct_golem`] spawned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GolemSpecies {
    Snow,
    Iron,
}

/// The result of a successful golem-pattern match: which golem spawned, its
/// entity id, and the world cells the caller must clear (vanilla
/// `CarvedPumpkinBlock.clearPatternBlocks` — air them and fire the level
/// event, both the block-placement owner's job, not this crate's).
#[derive(Debug, Clone)]
pub struct GolemConstruction {
    pub species: GolemSpecies,
    /// The spawned golem's entity id, as returned by [`SimMob::id`].
    pub id: i32,
    pub consumed: Vec<BlockPos>,
}

/// Issue #239: block-pattern detection and spawn for the snow and iron
/// golem. Every world in this module is a bare `HashMap`-backed closure —
/// `try_construct_golem` takes a pure block oracle, not a [`ChunkWorld`], so
/// there is nothing else to build.
#[cfg(test)]
mod golem_tests {
    use super::*;

    fn world_from(blocks: &[((i32, i32, i32), &str)]) -> impl Fn(i32, i32, i32) -> String {
        let map: std::collections::HashMap<(i32, i32, i32), String> = blocks
            .iter()
            .map(|(pos, name)| (*pos, (*name).to_owned()))
            .collect();
        move |x, y, z| {
            map.get(&(x, y, z))
                .cloned()
                .unwrap_or_else(|| "minecraft:air".to_owned())
        }
    }

    /// `try_construct_golem` reads only its own `block_at` closure — never
    /// `self.world` — so an empty `ChunkWorld` is a fine stand-in; it exists
    /// only because `MobSim::new` requires one.
    fn empty_world() -> ChunkWorld {
        ChunkWorld::new(-64, 384)
    }

    /// **The real iron golem shape, standing upright.** Four iron blocks in
    /// a T (three across at `y=4`, one more below centre at `y=3`) topped
    /// with a carved pumpkin — the exact geometry `CarvedPumpkinBlock`'s
    /// `getOrCreateIronGolemFull` declares, worked out independently against
    /// its `translateAndRotate` formula rather than assumed.
    #[test]
    fn an_upright_iron_golem_pattern_spawns_at_the_predicted_position() {
        let world = world_from(&[
            ((10, 5, 10), "minecraft:carved_pumpkin"),
            ((9, 4, 10), "minecraft:iron_block"),
            ((10, 4, 10), "minecraft:iron_block"),
            ((11, 4, 10), "minecraft:iron_block"),
            ((10, 3, 10), "minecraft:iron_block"),
        ]);
        let chunk_world = empty_world();
        let mut mobs = MobSim::new(&chunk_world);
        let result = mobs
            .try_construct_golem(&world, (10, 5, 10))
            .expect("a complete iron golem pattern must match");
        assert_eq!(result.species, GolemSpecies::Iron);

        let mut consumed: Vec<(i32, i32, i32)> =
            result.consumed.iter().map(|p| (p.x, p.y, p.z)).collect();
        consumed.sort_unstable();
        let mut expected = vec![
            (10, 5, 10),
            (9, 4, 10),
            (10, 4, 10),
            (11, 4, 10),
            (10, 3, 10),
        ];
        expected.sort_unstable();
        assert_eq!(
            consumed, expected,
            "every pattern block (pumpkin included) must be reported, and nothing else"
        );

        let spawned = mobs.get(result.id).expect("the golem was spawned");
        assert_eq!(
            spawned.position(),
            Vec3::new(10.5, 3.05, 10.5),
            "spawn position is the bottom-centre iron block's cell, offset (0.5, 0.05, 0.5) \
             per vanilla's own `snapTo` call — not the pumpkin's position"
        );
    }

    /// **Control: the same four-iron-plus-pumpkin count is not enough on its
    /// own** — remove the single load-bearing centre-bottom block and the
    /// otherwise-identical structure must not match. Without this, a
    /// permissive matcher (e.g. "at least 4 iron blocks nearby") would pass
    /// the test above for the wrong reason.
    #[test]
    fn control_a_pattern_missing_its_bottom_iron_block_does_not_match() {
        let world = world_from(&[
            ((10, 5, 10), "minecraft:carved_pumpkin"),
            ((9, 4, 10), "minecraft:iron_block"),
            ((10, 4, 10), "minecraft:iron_block"),
            ((11, 4, 10), "minecraft:iron_block"),
            // (10, 3, 10) missing.
        ]);
        let chunk_world = empty_world();
        let mut mobs = MobSim::new(&chunk_world);
        assert!(
            mobs.try_construct_golem(&world, (10, 5, 10)).is_none(),
            "an incomplete pattern must never spawn a golem"
        );
    }

    /// **The 24-orientation search actually engages** — the same T-shape
    /// built lying on its side against an imaginary wall (`up = +X` instead
    /// of `up = +Y`) must still match. A matcher that only tried
    /// `up = (0, 1, 0)` would silently reject this, which is exactly the
    /// "a pattern can be built in more than one orientation" trap this
    /// issue's brief calls out by name.
    #[test]
    fn an_iron_golem_built_sideways_against_a_wall_still_matches() {
        let world = world_from(&[
            ((0, 4, 0), "minecraft:carved_pumpkin"),
            ((-1, 3, 0), "minecraft:iron_block"),
            ((-1, 4, 0), "minecraft:iron_block"),
            ((-1, 5, 0), "minecraft:iron_block"),
            ((-2, 4, 0), "minecraft:iron_block"),
        ]);
        let chunk_world = empty_world();
        let mut mobs = MobSim::new(&chunk_world);
        let result = mobs
            .try_construct_golem(&world, (0, 4, 0))
            .expect("a sideways-built iron golem pattern must still match");
        assert_eq!(result.species, GolemSpecies::Iron);
    }

    /// The snow golem's pattern: pumpkin over two snow blocks, one column.
    #[test]
    fn a_snow_golem_pattern_spawns_at_the_predicted_position() {
        let world = world_from(&[
            ((2, 10, 2), "minecraft:carved_pumpkin"),
            ((2, 9, 2), "minecraft:snow_block"),
            ((2, 8, 2), "minecraft:snow_block"),
        ]);
        let chunk_world = empty_world();
        let mut mobs = MobSim::new(&chunk_world);
        let result = mobs
            .try_construct_golem(&world, (2, 10, 2))
            .expect("a complete snow golem pattern must match");
        assert_eq!(result.species, GolemSpecies::Snow);
        assert_eq!(result.consumed.len(), 3, "pumpkin plus two snow blocks");

        let spawned = mobs.get(result.id).expect("the golem was spawned");
        assert_eq!(
            spawned.position(),
            Vec3::new(2.5, 8.05, 2.5),
            "spawn position is the bottom snow block's cell"
        );
    }

    /// A jack o'lantern completes the pattern exactly as a plain carved
    /// pumpkin does — `PUMPKINS_PREDICATE` accepts both, and a matcher keyed
    /// on only one block name would miss half of vanilla's valid triggers.
    #[test]
    fn a_jack_o_lantern_completes_the_pattern_too() {
        let world = world_from(&[
            ((2, 10, 2), "minecraft:jack_o_lantern"),
            ((2, 9, 2), "minecraft:snow_block"),
            ((2, 8, 2), "minecraft:snow_block"),
        ]);
        let chunk_world = empty_world();
        let mut mobs = MobSim::new(&chunk_world);
        assert!(
            mobs.try_construct_golem(&world, (2, 10, 2)).is_some(),
            "a jack o'lantern must trigger the pattern exactly like a carved pumpkin"
        );
    }
}
