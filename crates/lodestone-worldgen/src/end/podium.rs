//! The exit portal / end podium — the small bedrock structure at the
//! world's End origin that the dragon egg sits on, ringed by the portal
//! blocks that appear once the dragon is defeated. A port of
//! `EndPodiumFeature.place`, kept pure: it takes the origin the caller
//! already resolved (vanilla samples the heightmap at world XZ `(0, 0)` for
//! it — this crate has no `ChunkSource` to do that sampling itself, so the
//! caller supplies the resolved `y`) and returns the list of block writes,
//! never touching a world.
//!
//! `crates/lodestone-worldgen/src/end/mod.rs`'s own module doc names this
//! structure (along with the obsidian pillars in `spikes.rs`) as "not here,
//! and it is not terrain" — this is that piece.

/// One block this feature writes, in absolute coordinates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PodiumBlock {
    pub x: i32,
    pub y: i32,
    pub z: i32,
    /// A full block-state string, e.g. `"minecraft:bedrock"` or
    /// `"minecraft:wall_torch[facing=north]"`.
    pub state: String,
}

/// The four horizontal directions a wall torch can face, matching vanilla's
/// own `Direction.Plane.HORIZONTAL` membership (not its iteration order,
/// which this port does not need to reproduce: all four torches are placed
/// regardless of the order they are generated in).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Facing {
    North,
    South,
    West,
    East,
}

impl Facing {
    const ALL: [Facing; 4] = [Facing::North, Facing::South, Facing::West, Facing::East];

    /// The unit offset a wall torch on this face sits at, relative to the
    /// block it is attached to.
    fn offset(self) -> (i32, i32) {
        match self {
            Facing::North => (0, -1),
            Facing::South => (0, 1),
            Facing::West => (-1, 0),
            Facing::East => (1, 0),
        }
    }

    fn state_name(self) -> &'static str {
        match self {
            Facing::North => "north",
            Facing::South => "south",
            Facing::West => "west",
            Facing::East => "east",
        }
    }
}

/// `Vec3i.closerThan`: 3D Euclidean distance, strictly less than.
fn closer_than(dx: i32, dy: i32, dz: i32, distance: f64) -> bool {
    let dist_sq = f64::from(dx * dx + dy * dy + dz * dz);
    dist_sq < distance * distance
}

/// `EndPodiumFeature.place`, at `(origin_x, origin_y, origin_z)`. `active`
/// selects the exit-portal appearance (`minecraft:end_portal` core, air
/// above) vs. the initial, unlit podium (solid end-stone core, no portal
/// block) — matching vanilla's own `boolean active` constructor parameter.
///
/// **Not ported**: `dropPreviousAndSetBlock`'s item-drop side effect when
/// re-placing an already-different block during the active transition —
/// this function only describes the final block, not the drop; a caller
/// with real block-entity/inventory access can add that itself, the same
/// disclosed gap `mobs::wither`'s own module doc names for its own
/// non-worldgen block writes.
#[must_use]
pub fn end_podium(origin_x: i32, origin_y: i32, origin_z: i32, active: bool) -> Vec<PodiumBlock> {
    let mut writes = Vec::new();
    let push = |writes: &mut Vec<PodiumBlock>, x: i32, y: i32, z: i32, state: &str| {
        writes.push(PodiumBlock { x, y, z, state: state.to_string() });
    };

    for dx in -4..=4 {
        for dy in -1..=32 {
            for dz in -4..=4 {
                let inside_rim = closer_than(dx, dy, dz, 2.5);
                if !inside_rim && !closer_than(dx, dy, dz, 3.5) {
                    continue;
                }
                let (x, y, z) = (origin_x + dx, origin_y + dy, origin_z + dz);
                if dy < 0 {
                    if inside_rim {
                        push(&mut writes, x, y, z, "minecraft:bedrock");
                    } else {
                        push(&mut writes, x, y, z, "minecraft:end_stone");
                    }
                } else if dy > 0 {
                    push(&mut writes, x, y, z, "minecraft:air");
                } else if !inside_rim {
                    push(&mut writes, x, y, z, "minecraft:bedrock");
                } else if active {
                    push(&mut writes, x, y, z, "minecraft:end_portal");
                } else {
                    push(&mut writes, x, y, z, "minecraft:air");
                }
            }
        }
    }

    // The central bedrock pillar — always solid regardless of `active`, and
    // written *after* the ring above so it overwrites whatever the ring
    // pass wrote at its own column (including the portal-core block at
    // `dy == 0`): the pillar occupies the ring's exact centre, and vanilla
    // writes it unconditionally over the top for the same reason.
    for dy in 0..4 {
        push(&mut writes, origin_x, origin_y + dy, origin_z, "minecraft:bedrock");
    }

    let center_y = origin_y + 2;
    for facing in Facing::ALL {
        let (dx, dz) = facing.offset();
        push(
            &mut writes,
            origin_x + dx,
            center_y,
            origin_z + dz,
            &format!("minecraft:wall_torch[facing={}]", facing.state_name()),
        );
    }

    writes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find<'a>(writes: &'a [PodiumBlock], x: i32, y: i32, z: i32) -> Option<&'a PodiumBlock> {
        // Last write wins — mirrors vanilla's own overwrite-in-place
        // semantics (`setBlock` called twice at the same position keeps the
        // second), and matches the pillar's own deliberate overwrite noted
        // above.
        writes.iter().rev().find(|w| w.x == x && w.y == y && w.z == z)
    }

    /// The discriminating gate this whole port exists for: the exact centre
    /// column must be solid bedrock, never the end-portal block, in *both*
    /// states — the pillar write must win over the ring write at every
    /// height it occupies.
    #[test]
    fn the_centre_column_is_always_bedrock_not_portal() {
        for active in [false, true] {
            let writes = end_podium(0, 64, 0, active);
            for dy in 0..4 {
                let block = find(&writes, 0, 64 + dy, 0).expect("centre column must be written");
                assert_eq!(block.state, "minecraft:bedrock", "active={active} dy={dy}");
            }
        }
    }

    /// One block off-centre, at the ring's own plane, must actually be the
    /// portal core when active and air when not — the ring's own contract,
    /// distinct from the always-bedrock centre above.
    #[test]
    fn the_ring_at_the_portal_plane_is_portal_when_active_and_air_when_not() {
        // (1, 0) is inside the 2.5 rim (dist = 1.0) but not the centre
        // column, so the pillar overwrite above does not reach it.
        let active = end_podium(0, 64, 0, true);
        let inactive = end_podium(0, 64, 0, false);
        assert_eq!(find(&active, 1, 64, 0).unwrap().state, "minecraft:end_portal");
        assert_eq!(find(&inactive, 1, 64, 0).unwrap().state, "minecraft:air");
    }

    /// The outer bedrock rim (outside 2.5, inside 3.5, at the portal plane)
    /// is unconditional — present regardless of `active`.
    #[test]
    fn the_outer_rim_at_the_portal_plane_is_always_bedrock() {
        // (0, 3): dist = 3.0, outside 2.5 (not inside_rim) but inside 3.5.
        for active in [false, true] {
            let writes = end_podium(0, 64, 0, active);
            let block = find(&writes, 0, 64, 3).expect("outer rim must be written");
            assert_eq!(block.state, "minecraft:bedrock", "active={active}");
        }
    }

    /// All four wall torches are present, each facing outward from the
    /// pillar, at height `origin_y + 2`.
    #[test]
    fn all_four_wall_torches_are_placed_facing_outward() {
        let writes = end_podium(10, 70, -5, true);
        for (dx, dz, facing) in [(0, -1, "north"), (0, 1, "south"), (-1, 0, "west"), (1, 0, "east")] {
            let block = find(&writes, 10 + dx, 72, -5 + dz).unwrap_or_else(|| panic!("missing {facing} torch"));
            assert_eq!(block.state, format!("minecraft:wall_torch[facing={facing}]"));
        }
    }

    /// **Control**: an origin offset must translate every write, proving
    /// the function is not silently hardcoded to `(0, *, 0)` — every gate
    /// above uses `origin_x == origin_z == 0`, which alone could not catch
    /// a transposed or dropped offset.
    #[test]
    fn every_write_translates_with_the_origin() {
        let base = end_podium(0, 64, 0, true);
        let shifted = end_podium(100, 64, -200, true);
        assert_eq!(base.len(), shifted.len(), "translating the origin must not change how many blocks are written");
        for (b, s) in base.iter().zip(shifted.iter()) {
            assert_eq!(s.x, b.x + 100, "x did not translate");
            assert_eq!(s.z, b.z - 200, "z did not translate");
            assert_eq!(s.y, b.y, "y must be unaffected by an xz-only shift");
            assert_eq!(s.state, b.state, "the block placed at the translated position must match");
        }
    }
}
