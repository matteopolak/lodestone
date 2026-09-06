//! End-city template-piece assembly.
//!
//! A city starts with three house floors and then grows tower, bridge, house and
//! fat-tower branches. Pieces carry template placement settings; the dimension
//! stage owns writing them into its finished chunk grid.

use lodestone_worldgen_core::rng::RandomSource;

use super::processor::Processor;
use super::template::{transform, Mirror, PlaceSettings, Rotation};
use super::{template_piece, StructurePiece, TemplateStore};

const MAX_DEPTH: i32 = 8;

#[derive(Clone, Copy)]
struct Bridge {
    rotation: Rotation,
    offset: [i32; 3],
}

const TOWER_BRIDGES: [Bridge; 4] = [
    Bridge { rotation: Rotation::None, offset: [1, -1, 0] },
    Bridge { rotation: Rotation::Cw90, offset: [6, -1, 1] },
    Bridge { rotation: Rotation::Ccw90, offset: [0, -1, 5] },
    Bridge { rotation: Rotation::Cw180, offset: [5, -1, 6] },
];
const FAT_TOWER_BRIDGES: [Bridge; 4] = [
    Bridge { rotation: Rotation::None, offset: [4, -1, 0] },
    Bridge { rotation: Rotation::Cw90, offset: [12, -1, 4] },
    Bridge { rotation: Rotation::Ccw90, offset: [0, -1, 8] },
    Bridge { rotation: Rotation::Cw180, offset: [8, -1, 12] },
];

#[derive(Clone, Copy)]
enum Branch {
    HouseTower,
    Tower,
    TowerBridge { ship_created: bool },
    FatTower,
}

/// Builds every template piece of one city. `origin` is its computed 5×5
/// ground sample, not the chunk's north-west corner.
pub(super) fn generate<R: RandomSource>(
    origin: [i32; 3],
    rotation: Rotation,
    templates: &TemplateStore,
    random: &mut R,
) -> Vec<StructurePiece> {
    let mut pieces = Vec::new();
    let Some(mut last) = add_root(&mut pieces, templates, "base_floor", origin, rotation, true) else {
        return pieces;
    };
    for (offset, name, overwrite) in [
        ([-1, 0, -1], "second_floor_1", false),
        ([-1, 4, -1], "third_floor_1", false),
        ([-1, 8, -1], "third_roof", true),
    ] {
        let Some(piece) = add(&mut pieces, templates, &last, offset, name, rotation, overwrite) else {
            return pieces;
        };
        last = piece;
    }
    let _ = recursive(templates, Branch::Tower, 1, &last, [0, 0, 0], &mut pieces, random);
    pieces
}

fn rotation_add(a: Rotation, b: Rotation) -> Rotation {
    match (a.turns() + b.turns()) % 4 {
        1 => Rotation::Cw90,
        2 => Rotation::Cw180,
        3 => Rotation::Ccw90,
        _ => Rotation::None,
    }
}

fn settings(rotation: Rotation, overwrite: bool) -> PlaceSettings {
    PlaceSettings {
        rotation,
        mirror: Mirror::None,
        pivot: [0, 0, 0],
        processors: vec![if overwrite { Processor::structure_block() } else { Processor::structure_and_air() }],
        waterlogging: false,
    }
}

fn add_root(
    out: &mut Vec<StructurePiece>, templates: &TemplateStore, name: &str, position: [i32; 3], rotation: Rotation, overwrite: bool,
) -> Option<StructurePiece> {
    let template = templates.get(&format!("minecraft:end_city/{name}"))?;
    let piece = template_piece("minecraft:ecp", &format!("minecraft:end_city/{name}"), template, position, settings(rotation, overwrite));
    out.push(piece.clone());
    Some(piece)
}

fn add(
    out: &mut Vec<StructurePiece>, templates: &TemplateStore, parent: &StructurePiece, offset: [i32; 3], name: &str, rotation: Rotation, overwrite: bool,
) -> Option<StructurePiece> {
    let parent_place = parent.placement.as_ref()?;
    let delta = transform(offset, Mirror::None, parent_place.settings.rotation, [0, 0, 0]);
    let position = [parent_place.position[0] + delta[0], parent_place.position[1] + delta[1], parent_place.position[2] + delta[2]];
    add_root(out, templates, name, position, rotation, overwrite)
}

fn recursive<R: RandomSource>(
    templates: &TemplateStore, branch: Branch, depth: i32, parent: &StructurePiece, offset: [i32; 3], existing: &mut Vec<StructurePiece>, random: &mut R,
) -> bool {
    if depth > MAX_DEPTH { return false; }
    let mut children = Vec::new();
    let generated = generate_branch(templates, branch, depth, parent, offset, &mut children, random);
    if !generated { return false; }
    let tag = random.next_int();
    for child in &mut children { child.gen_depth = tag; }
    if children.iter().any(|child| existing.iter().any(|old| child.bounding_box.intersects(old.bounding_box) && old.gen_depth != parent.gen_depth)) {
        return false;
    }
    existing.extend(children);
    true
}

fn add_child(
    out: &mut Vec<StructurePiece>, templates: &TemplateStore, parent: &StructurePiece, offset: [i32; 3], name: &str, rotation: Rotation, overwrite: bool,
) -> Option<StructurePiece> { add(out, templates, parent, offset, name, rotation, overwrite) }

fn generate_branch<R: RandomSource>(
    templates: &TemplateStore, branch: Branch, depth: i32, parent: &StructurePiece, offset: [i32; 3], out: &mut Vec<StructurePiece>, random: &mut R,
) -> bool {
    let rotation = parent.placement.as_ref().map(|p| p.settings.rotation).unwrap_or(Rotation::None);
    match branch {
        Branch::HouseTower => {
            let Some(mut last) = add_child(out, templates, parent, offset, "base_floor", rotation, true) else { return false; };
            match random.next_int_bounded(3) {
                0 => { let Some(_) = add_child(out, templates, &last, [-1, 4, -1], "base_roof", rotation, true) else { return false; }; }
                1 => {
                    let Some(piece) = add_child(out, templates, &last, [-1, 0, -1], "second_floor_2", rotation, false) else { return false; }; last = piece;
                    let Some(piece) = add_child(out, templates, &last, [-1, 8, -1], "second_roof", rotation, false) else { return false; }; last = piece;
                    let _ = recursive(templates, Branch::Tower, depth + 1, &last, [0, 0, 0], out, random);
                }
                _ => {
                    let Some(piece) = add_child(out, templates, &last, [-1, 0, -1], "second_floor_2", rotation, false) else { return false; }; last = piece;
                    let Some(piece) = add_child(out, templates, &last, [-1, 4, -1], "third_floor_2", rotation, false) else { return false; }; last = piece;
                    let Some(piece) = add_child(out, templates, &last, [-1, 8, -1], "third_roof", rotation, true) else { return false; }; last = piece;
                    let _ = recursive(templates, Branch::Tower, depth + 1, &last, [0, 0, 0], out, random);
                }
            }
            true
        }
        Branch::Tower => {
            let Some(mut last) = add_child(out, templates, parent, [3 + random.next_int_bounded(2), -3, 3 + random.next_int_bounded(2)], "tower_base", rotation, true) else { return false; };
            let Some(piece) = add_child(out, templates, &last, [0, 7, 0], "tower_piece", rotation, true) else { return false; }; last = piece;
            let mut bridge = (random.next_int_bounded(3) == 0).then(|| last.clone());
            let height = 1 + random.next_int_bounded(3);
            for i in 0..height {
                let Some(piece) = add_child(out, templates, &last, [0, 4, 0], "tower_piece", rotation, true) else { return false; }; last = piece;
                if i < height - 1 && random.next_bool() { bridge = Some(last.clone()); }
            }
            if let Some(bridge) = bridge {
                for entry in TOWER_BRIDGES {
                    if random.next_bool() {
                        let Some(start) = add_child(out, templates, &bridge, entry.offset, "bridge_end", rotation_add(rotation, entry.rotation), true) else { return false; };
                        let _ = recursive(templates, Branch::TowerBridge { ship_created: false }, depth + 1, &start, [0, 0, 0], out, random);
                    }
                }
                add_child(out, templates, &last, [-1, 4, -1], "tower_top", rotation, true).is_some()
            } else if depth != 7 {
                recursive(templates, Branch::FatTower, depth + 1, &last, [0, 0, 0], out, random)
            } else {
                add_child(out, templates, &last, [-1, 4, -1], "tower_top", rotation, true).is_some()
            }
        }
        Branch::TowerBridge { mut ship_created } => {
            let length = random.next_int_bounded(4) + 1;
            let Some(mut last) = add_child(out, templates, parent, [0, 0, -4], "bridge_piece", rotation, true) else { return false; };
            last.gen_depth = -1;
            let mut next_y = 0;
            for _ in 0..length {
                let (name, offset) = if random.next_bool() { ("bridge_piece", [0, next_y, -4]) } else if random.next_bool() { ("bridge_steep_stairs", [0, next_y, -4]) } else { ("bridge_gentle_stairs", [0, next_y, -8]) };
                let Some(piece) = add_child(out, templates, &last, offset, name, rotation, true) else { return false; }; last = piece;
                next_y = if name == "bridge_piece" { 0 } else { 4 };
            }
            if !ship_created && random.next_int_bounded(10 - depth) == 0 {
                let Some(_) = add_child(out, templates, &last, [-8 + random.next_int_bounded(8), next_y, -70 + random.next_int_bounded(10)], "ship", rotation, true) else { return false; };
                ship_created = true;
            } else if !recursive(templates, Branch::HouseTower, depth + 1, &last, [-3, next_y + 1, -11], out, random) { return false; }
            let Some(mut end) = add_child(out, templates, &last, [4, next_y, 0], "bridge_end", rotation_add(rotation, Rotation::Cw180), true) else { return false; };
            end.gen_depth = -1;
            let _ = ship_created;
            true
        }
        Branch::FatTower => {
            let Some(mut last) = add_child(out, templates, parent, [-3, 4, -3], "fat_tower_base", rotation, true) else { return false; };
            let Some(piece) = add_child(out, templates, &last, [0, 4, 0], "fat_tower_middle", rotation, true) else { return false; }; last = piece;
            for _ in 0..2 {
                if random.next_int_bounded(3) == 0 { break; }
                let Some(piece) = add_child(out, templates, &last, [0, 8, 0], "fat_tower_middle", rotation, true) else { return false; }; last = piece;
                for entry in FAT_TOWER_BRIDGES {
                    if random.next_bool() {
                        let Some(start) = add_child(out, templates, &last, entry.offset, "bridge_end", rotation_add(rotation, entry.rotation), true) else { return false; };
                        let _ = recursive(templates, Branch::TowerBridge { ship_created: false }, depth + 1, &start, [0, 0, 0], out, random);
                    }
                }
            }
            add_child(out, templates, &last, [-2, 8, -2], "fat_tower_top", rotation, true).is_some()
        }
    }
}
