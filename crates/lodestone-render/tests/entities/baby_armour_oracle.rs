//! Baby humanoid armour against the real client: geometry and pose.
//!
//! `tests/support/baby_armour_jvm.txt` is written by `oracle-java/BabyArmourOracle.java`
//! (`just oracle-baby-armour`). For each baby wearer and slot it bakes the client's
//! own baby armour layer into the wearer's baby model class, runs that model's pose
//! setup for a scenario, and prints every part's local pose and every face's extents.
//! This test bakes [`BabyArmourMesh`] for the same wearer and slot and checks both:
//! the skeleton's local pose of every part we keep, and the faces of every part as a
//! multiset of position and texture extents.

use lodestone_assets::equipment::{ArmourSlot, BabyArmourKind};
use lodestone_render::entity::{ArmourMesh, BabyArmourMesh, EntityModelSet};
use lodestone_render::entity_anim::{AnimInput, Skeleton};
use lodestone_render::PartRange;
use lodestone_render::models::ModelVertex;

const DUMP: &str = include_str!("../support/baby_armour_jvm.txt");
const TOLERANCE: f32 = 1.0e-4;

struct Scenario {
    name: String,
    rig: String,
    slot: ArmourSlot,
    input: AnimInput,
    /// `(part, [x, y, z, xRot, yRot, zRot])`.
    parts: Vec<(String, [f32; 6])>,
    /// `(part, [minX, minY, minZ, maxX, maxY, maxZ, minU, minV, maxU, maxV])`.
    faces: Vec<(String, [f32; 10])>,
}

fn slot(name: &str) -> ArmourSlot {
    match name {
        "head" => ArmourSlot::Head,
        "chest" => ArmourSlot::Chest,
        "legs" => ArmourSlot::Legs,
        "feet" => ArmourSlot::Feet,
        other => panic!("unknown slot {other}"),
    }
}

fn numbers<const N: usize>(cols: &[&str]) -> [f32; N] {
    let mut out = [0.0; N];
    for (slot, text) in out.iter_mut().zip(cols) {
        *slot = text.parse().expect("number");
    }
    out
}

fn parse() -> Vec<Scenario> {
    let mut out: Vec<Scenario> = Vec::new();
    for line in DUMP.lines().filter(|l| !l.starts_with('#') && !l.is_empty()) {
        let cols: Vec<&str> = line.split_whitespace().collect();
        match cols[0] {
            "input" => {
                let mut input = AnimInput::REST;
                for kv in &cols[4..] {
                    let (key, value) = kv.split_once('=').expect("key=value");
                    let v: f32 = value.parse().expect("number");
                    match key {
                        "pitch" => input.head_pitch_deg = v,
                        "yaw" => input.head_yaw_deg = v,
                        "pos" => input.limb_swing = v,
                        "speed" => input.limb_swing_amount = v,
                        "age" => input.age_ticks = v,
                        "crouching" => input.crouching = v != 0.0,
                        "aggressive" => input.aggressive = v != 0.0,
                        other => panic!("unknown scenario key {other}"),
                    }
                }
                out.push(Scenario {
                    name: cols[1].to_owned(),
                    rig: cols[2].to_owned(),
                    slot: slot(cols[3]),
                    input,
                    parts: Vec::new(),
                    faces: Vec::new(),
                });
            }
            kind @ ("part" | "face") => {
                let scenario = out.last_mut().expect("a part line before any input line");
                assert_eq!(cols[1], scenario.name, "{kind} line out of order");
                if kind == "part" {
                    scenario.parts.push((cols[2].to_owned(), numbers(&cols[3..9])));
                } else {
                    scenario.faces.push((cols[2].to_owned(), numbers(&cols[3..13])));
                }
            }
            other => panic!("unknown line kind {other}"),
        }
    }
    out
}

/// Every mismatch between `skeleton`'s local poses and the client's, for the parts
/// the skeleton has. A client part the skeleton lacks is checked by the face
/// comparison (it must carry no geometry).
fn pose_mismatches(skeleton: &Skeleton, scenario: &Scenario) -> Vec<String> {
    let poses = skeleton.local_poses(&scenario.input);
    let mut out = Vec::new();
    for (part, want) in &scenario.parts {
        let Some((_, p)) = poses.iter().find(|(name, _)| name == part) else {
            continue;
        };
        let got = [p.x, p.y, p.z, p.x_rot, p.y_rot, p.z_rot];
        for (axis, (g, w)) in ["x", "y", "z", "xRot", "yRot", "zRot"].iter().zip(got.iter().zip(want)) {
            if (g - w).abs() > TOLERANCE {
                out.push(format!("{part}.{axis}: got {g}, client {w}"));
            }
        }
    }
    for (name, _) in &poses {
        // The unnamed root is the model itself, which the client does not list.
        if !name.is_empty() && *name != "root" && !scenario.parts.iter().any(|(part, _)| part == name) {
            out.push(format!("{name}: a part the client's mesh does not have"));
        }
    }
    out
}

/// The extents of each quad of a part, in the dump's column order.
fn quads(vertices: &[ModelVertex], range: PartRange) -> Vec<[f32; 10]> {
    let start = range.vertex_start as usize;
    vertices[start..start + range.vertex_count as usize]
        .chunks_exact(4)
        .map(|quad| {
            let mut lo = [f32::INFINITY; 5];
            let mut hi = [f32::NEG_INFINITY; 5];
            for v in quad {
                let p = [v.position[0], v.position[1], v.position[2], v.uv[0], v.uv[1]];
                for i in 0..5 {
                    lo[i] = lo[i].min(p[i]);
                    hi[i] = hi[i].max(p[i]);
                }
            }
            [lo[0], lo[1], lo[2], hi[0], hi[1], hi[2], lo[3], lo[4], hi[3], hi[4]]
        })
        .collect()
}

/// Every face of the client's that `ours` does not match one-to-one, and every face
/// of ours left over, per part.
fn face_mismatches(ours: &[(&str, Vec<[f32; 10]>)], scenario: &Scenario) -> Vec<String> {
    let mut out = Vec::new();
    let mut left: Vec<(&str, Vec<[f32; 10]>)> = ours.to_vec();
    for (part, want) in &scenario.faces {
        let pool = left.iter_mut().find(|(name, _)| name == part);
        let hit = pool.and_then(|(_, faces)| {
            faces
                .iter()
                .position(|got| got.iter().zip(want).all(|(g, w)| (g - w).abs() <= TOLERANCE))
                .map(|i| faces.swap_remove(i))
        });
        if hit.is_none() {
            out.push(format!("{part}: client face {want:?} unmatched"));
        }
    }
    for (part, faces) in left {
        for face in faces {
            out.push(format!("{part}: our face {face:?} has no client counterpart"));
        }
    }
    out
}

fn baby_faces(mesh: &BabyArmourMesh) -> Vec<(&'static str, Vec<[f32; 10]>)> {
    mesh.mesh.parts.iter().map(|(name, range)| (*name, quads(&mesh.mesh.vertices, *range))).collect()
}

fn adult_faces(mesh: &ArmourMesh) -> Vec<(&'static str, Vec<[f32; 10]>)> {
    mesh.parts.iter().map(|(name, range)| (*name, quads(&mesh.vertices, *range))).collect()
}

fn baby_mesh(scenario: &Scenario) -> BabyArmourMesh {
    let kind = BabyArmourKind::for_baby_rig(&scenario.rig).unwrap_or_else(|| panic!("{} wears no baby armour", scenario.rig));
    BabyArmourMesh::new(&scenario.rig, kind, scenario.slot)
}

#[test]
fn every_baby_armour_scenario_matches_the_client_mesh_and_pose() {
    let scenarios = parse();
    // Six wearers, four slots, three scenarios each.
    assert_eq!(scenarios.len(), 72, "unexpected scenario count");
    let mut failures = Vec::new();
    let mut faces = 0;
    for scenario in &scenarios {
        assert!(!scenario.faces.is_empty(), "{} has no faces", scenario.name);
        faces += scenario.faces.len();
        let mesh = baby_mesh(scenario);
        for miss in pose_mismatches(&mesh.posed.skeleton, scenario) {
            failures.push(format!("{} pose: {miss}", scenario.name));
        }
        for miss in face_mismatches(&baby_faces(&mesh), scenario) {
            failures.push(format!("{} faces: {miss}", scenario.name));
        }
    }
    assert!(faces > 900, "only {faces} faces were compared");
    assert!(failures.is_empty(), "{} mismatches:\n{}", failures.len(), failures.join("\n"));
}

/// Control: the face check sees geometry. The adult armour mesh for each slot, the
/// one a baby wore before it had its own, must disagree with every scenario.
#[test]
fn the_baby_armour_gate_rejects_the_adult_mesh() {
    for scenario in parse() {
        let adult = ArmourMesh::for_slot(scenario.slot);
        assert!(
            !face_mismatches(&adult_faces(&adult), &scenario).is_empty(),
            "{}: the adult mesh passes the face gate",
            scenario.name
        );
    }
}

/// Control: the pose check sees the armour's own pivots and pose. Posing with the
/// wearer's body skeleton (the armour once rode its parts) must disagree with the
/// client in every scenario that draws a limb or the body: the baby armour's arm,
/// leg and body pivots are not the wearer's.
#[test]
fn the_baby_armour_gate_rejects_the_wearer_skeleton() {
    let set = EntityModelSet::load();
    let mut checked = 0;
    for scenario in parse() {
        if scenario.slot == ArmourSlot::Head {
            continue;
        }
        let wearer = &set.get(&scenario.rig).unwrap_or_else(|| panic!("no baked {}", scenario.rig)).skeleton;
        let misses: Vec<String> = pose_mismatches(wearer, &scenario)
            .into_iter()
            .filter(|m| !m.contains("a part the client's mesh does not have"))
            .collect();
        assert!(!misses.is_empty(), "{}: the wearer skeleton passes the pose gate", scenario.name);
        checked += 1;
    }
    assert_eq!(checked, 54);
}
