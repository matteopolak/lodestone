use std::{collections::BTreeMap, env, fs};

use lodestone_core::Reader;
use lodestone_data::block_states::{block_name, properties};
use lodestone_v26_2::packets::chunk::{ChunkShape, LevelChunkWithLight};

fn state_label(id: u32) -> String {
    let name = block_name(id).unwrap_or("unknown");
    let properties = properties(id).unwrap_or_default();
    if properties.is_empty() {
        name.to_owned()
    } else {
        let properties = properties
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join(",");
        format!("{name}[{properties}]")
    }
}

fn decode(path: &str) -> LevelChunkWithLight {
    let bytes = fs::read(path).expect("read packet");
    let mut reader = Reader::new(&bytes);
    let packet = LevelChunkWithLight::decode(&mut reader, &ChunkShape::overworld_1_21())
        .expect("decode packet");
    reader.ensure_empty().expect("packet has no trailing bytes");
    packet
}

fn main() {
    let reference = decode(&env::args().nth(1).expect("reference packet path"));
    let actual = decode(&env::args().nth(2).expect("actual packet path"));
    assert_eq!((reference.x, reference.z), (actual.x, actual.z));

    let mut pairs = BTreeMap::<(String, String), usize>::new();
    let mut positions = Vec::new();
    for section_index in 0..reference.column.section_count() {
        let reference_section = reference.column.section(section_index);
        let actual_section = actual.column.section(section_index);
        for cell in 0..4096 {
            let expected = reference_section.map_or(0, |section| section.block_states().get(cell));
            let current = actual_section.map_or(0, |section| section.block_states().get(cell));
            if expected == current {
                continue;
            }
            let expected = state_label(expected);
            let current = state_label(current);
            *pairs.entry((expected.clone(), current.clone())).or_default() += 1;
            let x = cell % 16;
            let z = (cell / 16) % 16;
            let y = reference.column.min_y() + section_index as i32 * 16 + (cell / 256) as i32;
            positions.push((x, y, z, expected, current));
        }
    }
    println!("mismatches={}", positions.len());
    for ((expected, current), count) in &pairs {
        println!("{count}: {expected} -> {current}");
    }
    for (x, y, z, expected, current) in positions {
        if expected.contains("dark_oak")
            || current.contains("dark_oak")
            || expected.contains("cave_vines")
            || current.contains("cave_vines")
            || expected.contains("oak_log")
            || current.contains("oak_log")
        {
            println!("diff ({x},{y},{z}) {expected} -> {current}");
        }
    }
}
